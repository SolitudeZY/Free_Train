use domain::{EdgeStrategy, Roi, TileConfig};
use image::{
    DynamicImage, GrayImage, ImageFormat, Rgba, RgbaImage,
    imageops::{self, FilterType},
};
use rand::{Rng, SeedableRng};
use rand_chacha::ChaCha8Rng;
use rustdct::DctPlanner;
use serde::{Deserialize, Serialize};
use std::{collections::HashSet, io::Cursor};
use thiserror::Error;

const PHASH_SAMPLE_SIZE: usize = 32;
const PHASH_LOW_FREQUENCIES: usize = 8;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct TilePlacement {
    pub row: u32,
    pub column: u32,
    pub source_x: u32,
    pub source_y: u32,
    pub source_width: u32,
    pub source_height: u32,
    pub output_width: u32,
    pub output_height: u32,
    pub padded: bool,
}

/// How a partial tile is filled when a placement reaches the ROI boundary.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PaddingMode {
    Constant,
    Edge,
    Reflect,
}

/// Resize behavior applied after cropping and padding.  All modes produce the
/// exact configured output dimensions.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ResizeMode {
    Stretch,
    Fit,
    Fill,
    LongSide,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct TileRenderConfig {
    pub tile: TileConfig,
    pub resize: ResizeMode,
    pub padding: PaddingMode,
    pub fill: [u8; 4],
}

impl TileRenderConfig {
    pub fn validate(self) -> Result<Self, PipelineError> {
        self.tile.validate()?;
        Ok(self)
    }
}

/// Render one planned tile.  This is deliberately shared by preview and
/// export callers so coordinates and edge behavior cannot diverge.
pub fn render_tile(
    image: &DynamicImage,
    roi: Roi,
    placement: TilePlacement,
    config: TileRenderConfig,
) -> Result<DynamicImage, PipelineError> {
    config.validate()?;
    let rgba = image.to_rgba8();
    roi.validate(rgba.width(), rgba.height())?;
    let mut out = RgbaImage::from_pixel(
        placement.output_width.max(1),
        placement.output_height.max(1),
        Rgba(config.fill),
    );
    for y in 0..out.height() {
        for x in 0..out.width() {
            let sx = placement.source_x.saturating_add(x);
            let sy = placement.source_y.saturating_add(y);
            let inside = sx < rgba.width()
                && sy < rgba.height()
                && sx >= roi.x
                && sy >= roi.y
                && sx < roi.x + roi.width
                && sy < roi.y + roi.height;
            let pixel = if inside {
                *rgba.get_pixel(sx, sy)
            } else {
                sample_padding(&rgba, roi, sx, sy, config.padding, config.fill)
            };
            out.put_pixel(x, y, pixel);
        }
    }
    let rendered = DynamicImage::ImageRgba8(out);
    Ok(resize_tile(
        rendered,
        placement.output_width,
        placement.output_height,
        config.resize,
    ))
}

pub fn render_tiles(
    image: &DynamicImage,
    roi: Roi,
    config: TileRenderConfig,
) -> Result<Vec<(TilePlacement, DynamicImage)>, PipelineError> {
    let placements = plan_tiles(roi, config.tile)?;
    placements
        .into_iter()
        .map(|placement| {
            let tile = render_tile(image, roi, placement, config)?;
            Ok((placement, tile))
        })
        .collect()
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ExportFormat {
    Jpeg,
    Png,
    Webp,
}

impl ExportFormat {
    pub fn extension(self) -> &'static str {
        match self {
            Self::Jpeg => "jpg",
            Self::Png => "png",
            Self::Webp => "webp",
        }
    }

    fn image_format(self) -> ImageFormat {
        match self {
            Self::Jpeg => ImageFormat::Jpeg,
            Self::Png => ImageFormat::Png,
            Self::Webp => ImageFormat::WebP,
        }
    }
}

pub fn encode_image(image: &DynamicImage, format: ExportFormat) -> Result<Vec<u8>, PipelineError> {
    let mut bytes = Cursor::new(Vec::new());
    image
        .write_to(&mut bytes, format.image_format())
        .map_err(|error| PipelineError::Encode(error.to_string()))?;
    Ok(bytes.into_inner())
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ConflictStrategy {
    AppendSequence,
    AppendHash,
    Skip,
    Fail,
}

pub fn sanitize_file_stem(value: &str) -> Result<String, PipelineError> {
    let mut result = value
        .chars()
        .map(|character| {
            if character.is_control() || "<>:\"/\\|?*".contains(character) {
                '_'
            } else {
                character
            }
        })
        .collect::<String>();
    result = result.trim().trim_end_matches(['.', ' ']).to_owned();
    if result.is_empty() {
        return Err(PipelineError::InvalidFileName);
    }
    let base = result
        .split('.')
        .next()
        .unwrap_or_default()
        .to_ascii_uppercase();
    let reserved = matches!(
        base.as_str(),
        "CON"
            | "PRN"
            | "AUX"
            | "NUL"
            | "COM1"
            | "COM2"
            | "COM3"
            | "COM4"
            | "COM5"
            | "COM6"
            | "COM7"
            | "COM8"
            | "COM9"
            | "LPT1"
            | "LPT2"
            | "LPT3"
            | "LPT4"
            | "LPT5"
            | "LPT6"
            | "LPT7"
            | "LPT8"
            | "LPT9"
    );
    if reserved {
        result.insert(0, '_');
    }
    Ok(result)
}

pub fn resolve_file_name(
    stem: &str,
    extension: &str,
    stable_hash: &str,
    strategy: ConflictStrategy,
    occupied: &mut HashSet<String>,
) -> Result<Option<String>, PipelineError> {
    let stem = sanitize_file_stem(stem)?;
    let extension = extension.trim_start_matches('.').to_ascii_lowercase();
    if extension.is_empty()
        || !extension
            .chars()
            .all(|character| character.is_ascii_alphanumeric())
    {
        return Err(PipelineError::InvalidFileName);
    }
    let original = format!("{stem}.{extension}");
    if occupied.insert(original.to_ascii_lowercase()) {
        return Ok(Some(original));
    }
    match strategy {
        ConflictStrategy::Skip => Ok(None),
        ConflictStrategy::Fail => Err(PipelineError::FileNameConflict(original)),
        ConflictStrategy::AppendHash => {
            let suffix = stable_hash
                .chars()
                .filter(|character| character.is_ascii_hexdigit())
                .take(8)
                .collect::<String>();
            if suffix.len() < 8 {
                return Err(PipelineError::InvalidStableHash);
            }
            let candidate = format!("{stem}_{suffix}.{extension}");
            if occupied.insert(candidate.to_ascii_lowercase()) {
                Ok(Some(candidate))
            } else {
                Err(PipelineError::FileNameConflict(candidate))
            }
        }
        ConflictStrategy::AppendSequence => {
            for sequence in 2..=100_000 {
                let candidate = format!("{stem}_{sequence:04}.{extension}");
                if occupied.insert(candidate.to_ascii_lowercase()) {
                    return Ok(Some(candidate));
                }
            }
            Err(PipelineError::FileNameConflict(original))
        }
    }
}

fn sample_padding(
    image: &RgbaImage,
    roi: Roi,
    x: u32,
    y: u32,
    mode: PaddingMode,
    fill: [u8; 4],
) -> Rgba<u8> {
    if mode == PaddingMode::Constant {
        return Rgba(fill);
    }
    if roi.width == 0 || roi.height == 0 {
        return Rgba(fill);
    }
    let (mut lx, mut ly) = (x as i64 - roi.x as i64, y as i64 - roi.y as i64);
    let w = roi.width as i64;
    let h = roi.height as i64;
    if mode == PaddingMode::Edge {
        lx = lx.clamp(0, w - 1);
        ly = ly.clamp(0, h - 1);
    } else {
        lx = reflect_index(lx, w);
        ly = reflect_index(ly, h);
    }
    *image.get_pixel(roi.x + lx as u32, roi.y + ly as u32)
}

fn reflect_index(value: i64, length: i64) -> i64 {
    if length <= 1 {
        return 0;
    }
    let period = 2 * length - 2;
    let mut v = value % period;
    if v < 0 {
        v += period;
    }
    if v >= length { period - v } else { v }
}

fn resize_tile(image: DynamicImage, width: u32, height: u32, mode: ResizeMode) -> DynamicImage {
    if width == 0 || height == 0 {
        return image;
    }
    match mode {
        // Nearest keeps explicit padding pixels intact at the output edge.
        ResizeMode::Stretch => image.resize_exact(width, height, FilterType::Nearest),
        ResizeMode::Fit => {
            let fitted = image.resize(width, height, FilterType::Lanczos3);
            let mut canvas = RgbaImage::from_pixel(width, height, Rgba([0, 0, 0, 0]));
            let x = (width - fitted.width()) / 2;
            let y = (height - fitted.height()) / 2;
            imageops::overlay(&mut canvas, &fitted.to_rgba8(), x.into(), y.into());
            DynamicImage::ImageRgba8(canvas)
        }
        ResizeMode::Fill => image.resize_to_fill(width, height, FilterType::Lanczos3),
        ResizeMode::LongSide => {
            let scale =
                (width as f32 / image.width() as f32).min(height as f32 / image.height() as f32);
            let w = (image.width() as f32 * scale).round().max(1.0) as u32;
            let h = (image.height() as f32 * scale).round().max(1.0) as u32;
            let fitted = image.resize_exact(w, h, FilterType::Lanczos3);
            let mut canvas = RgbaImage::from_pixel(width, height, Rgba([0, 0, 0, 0]));
            imageops::overlay(
                &mut canvas,
                &fitted.to_rgba8(),
                ((width - w) / 2).into(),
                ((height - h) / 2).into(),
            );
            DynamicImage::ImageRgba8(canvas)
        }
    }
}

pub fn plan_tiles(roi: Roi, config: TileConfig) -> Result<Vec<TilePlacement>, PipelineError> {
    let config = config.validate()?;
    let x_positions = positions(
        roi.width,
        config.tile_width,
        config.overlap_x,
        config.edge_strategy,
    );
    let y_positions = positions(
        roi.height,
        config.tile_height,
        config.overlap_y,
        config.edge_strategy,
    );
    let mut placements = Vec::with_capacity(x_positions.len() * y_positions.len());
    for (row, local_y) in y_positions.into_iter().enumerate() {
        for (column, local_x) in x_positions.iter().copied().enumerate() {
            let source_width = config.tile_width.min(roi.width.saturating_sub(local_x));
            let source_height = config.tile_height.min(roi.height.saturating_sub(local_y));
            placements.push(TilePlacement {
                row: row as u32,
                column: column as u32,
                source_x: roi.x + local_x,
                source_y: roi.y + local_y,
                source_width,
                source_height,
                output_width: config.tile_width,
                output_height: config.tile_height,
                padded: source_width != config.tile_width || source_height != config.tile_height,
            });
        }
    }
    Ok(placements)
}

fn positions(length: u32, tile: u32, overlap: u32, strategy: EdgeStrategy) -> Vec<u32> {
    if length == 0 {
        return Vec::new();
    }
    if length <= tile {
        return match strategy {
            EdgeStrategy::Discard if length < tile => Vec::new(),
            _ => vec![0],
        };
    }
    let step = tile - overlap;
    let mut result = Vec::new();
    let mut position = 0;
    loop {
        match strategy {
            EdgeStrategy::Discard => {
                if position + tile > length {
                    break;
                }
                result.push(position);
            }
            EdgeStrategy::Pad => {
                if position >= length {
                    break;
                }
                result.push(position);
            }
            EdgeStrategy::ShiftToEdge => {
                if position + tile >= length {
                    let edge = length - tile;
                    if result.last().copied() != Some(edge) {
                        result.push(edge);
                    }
                    break;
                }
                result.push(position);
            }
        }
        position = match position.checked_add(step) {
            Some(next) => next,
            None => break,
        };
    }
    result
}

pub fn perceptual_hash(image: &DynamicImage) -> u64 {
    let resized = image
        .resize_exact(
            PHASH_SAMPLE_SIZE as u32,
            PHASH_SAMPLE_SIZE as u32,
            FilterType::Lanczos3,
        )
        .to_luma8();
    let mut coefficients: Vec<f32> = resized.pixels().map(|pixel| pixel[0] as f32).collect();
    let mut planner = DctPlanner::new();
    let dct = planner.plan_dct2(PHASH_SAMPLE_SIZE);
    for row in coefficients.chunks_exact_mut(PHASH_SAMPLE_SIZE) {
        dct.process_dct2(row);
    }
    let mut column = vec![0.0_f32; PHASH_SAMPLE_SIZE];
    for x in 0..PHASH_SAMPLE_SIZE {
        for y in 0..PHASH_SAMPLE_SIZE {
            column[y] = coefficients[y * PHASH_SAMPLE_SIZE + x];
        }
        dct.process_dct2(&mut column);
        for y in 0..PHASH_SAMPLE_SIZE {
            coefficients[y * PHASH_SAMPLE_SIZE + x] = column[y];
        }
    }
    let mut low_frequency = Vec::with_capacity(PHASH_LOW_FREQUENCIES.pow(2) - 1);
    for y in 0..PHASH_LOW_FREQUENCIES {
        for x in 0..PHASH_LOW_FREQUENCIES {
            if x != 0 || y != 0 {
                low_frequency.push(coefficients[y * PHASH_SAMPLE_SIZE + x]);
            }
        }
    }
    let mut sorted = low_frequency.clone();
    sorted.sort_by(f32::total_cmp);
    let median = sorted[sorted.len() / 2];
    low_frequency
        .into_iter()
        .enumerate()
        .fold(0_u64, |hash, (bit, value)| {
            if value > median {
                hash | (1_u64 << bit)
            } else {
                hash
            }
        })
}

pub fn hamming_distance(left: u64, right: u64) -> u32 {
    (left ^ right).count_ones()
}

#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct QualityMetrics {
    pub width: u32,
    pub height: u32,
    pub aspect_ratio: f64,
    pub sharpness: f64,
    pub underexposed_ratio: f64,
    pub overexposed_ratio: f64,
    pub entropy: f64,
    pub low_information: f64,
}

pub fn measure_quality(image: &DynamicImage) -> QualityMetrics {
    let luma = image.to_luma8();
    let width = luma.width();
    let height = luma.height();
    let count = (width as u64 * height as u64).max(1) as f64;
    let mut histogram = [0_u64; 256];
    let mut sum = 0.0;
    let mut underexposed = 0_u64;
    let mut overexposed = 0_u64;
    for pixel in luma.pixels() {
        let value = pixel[0] as usize;
        histogram[value] += 1;
        sum += value as f64;
        underexposed += u64::from(value <= 16);
        overexposed += u64::from(value >= 239);
    }
    let mean = sum / count;
    let variance = luma
        .pixels()
        .map(|pixel| {
            let delta = pixel[0] as f64 - mean;
            delta * delta
        })
        .sum::<f64>()
        / count;
    let entropy = histogram
        .into_iter()
        .filter(|bucket| *bucket > 0)
        .map(|bucket| {
            let probability = bucket as f64 / count;
            -probability * probability.log2()
        })
        .sum::<f64>();
    let sharpness = if width < 3 || height < 3 {
        0.0
    } else {
        let mut laplacian_sum = 0.0;
        let mut laplacian_squared_sum = 0.0;
        let laplacian_count = ((width - 2) as u64 * (height - 2) as u64) as f64;
        for y in 1..height - 1 {
            for x in 1..width - 1 {
                let center = luma.get_pixel(x, y)[0] as f64;
                let value = luma.get_pixel(x - 1, y)[0] as f64
                    + luma.get_pixel(x + 1, y)[0] as f64
                    + luma.get_pixel(x, y - 1)[0] as f64
                    + luma.get_pixel(x, y + 1)[0] as f64
                    - 4.0 * center;
                laplacian_sum += value;
                laplacian_squared_sum += value * value;
            }
        }
        let laplacian_mean = laplacian_sum / laplacian_count;
        (laplacian_squared_sum / laplacian_count) - laplacian_mean * laplacian_mean
    };
    let normalized_entropy = (entropy / 8.0).clamp(0.0, 1.0);
    let normalized_spread = (variance.sqrt() / 64.0).clamp(0.0, 1.0);
    QualityMetrics {
        width,
        height,
        aspect_ratio: width as f64 / height.max(1) as f64,
        sharpness,
        underexposed_ratio: underexposed as f64 / count,
        overexposed_ratio: overexposed as f64 / count,
        entropy,
        low_information: 1.0 - (normalized_entropy * 0.7 + normalized_spread * 0.3),
    }
}

pub fn global_ssim(left: &DynamicImage, right: &DynamicImage) -> f64 {
    let left = normalize_luma(left);
    let right = normalize_luma(right);
    let count = (left.width() * left.height()) as f64;
    let mean_left = left.pixels().map(|pixel| pixel[0] as f64).sum::<f64>() / count;
    let mean_right = right.pixels().map(|pixel| pixel[0] as f64).sum::<f64>() / count;
    let mut variance_left = 0.0;
    let mut variance_right = 0.0;
    let mut covariance = 0.0;
    for (left_pixel, right_pixel) in left.pixels().zip(right.pixels()) {
        let left_delta = left_pixel[0] as f64 - mean_left;
        let right_delta = right_pixel[0] as f64 - mean_right;
        variance_left += left_delta * left_delta;
        variance_right += right_delta * right_delta;
        covariance += left_delta * right_delta;
    }
    let denominator = (count - 1.0).max(1.0);
    variance_left /= denominator;
    variance_right /= denominator;
    covariance /= denominator;
    let c1 = (0.01_f64 * 255.0).powi(2);
    let c2 = (0.03_f64 * 255.0).powi(2);
    ((2.0 * mean_left * mean_right + c1) * (2.0 * covariance + c2))
        / ((mean_left.powi(2) + mean_right.powi(2) + c1) * (variance_left + variance_right + c2))
}

fn normalize_luma(image: &DynamicImage) -> GrayImage {
    image
        .resize_exact(256, 256, FilterType::Triangle)
        .to_luma8()
}

pub fn deterministic_brightness(
    image: &DynamicImage,
    seed: u64,
    min_offset: i32,
    max_offset: i32,
) -> Result<(DynamicImage, i32), PipelineError> {
    if min_offset > max_offset {
        return Err(PipelineError::InvalidBrightnessRange);
    }
    let mut rng = ChaCha8Rng::seed_from_u64(seed);
    let offset = rng.random_range(min_offset..=max_offset);
    Ok((image.brighten(offset), offset))
}

#[derive(Debug, Error)]
pub enum PipelineError {
    #[error(transparent)]
    Domain(#[from] domain::DomainError),
    #[error("brightness minimum must not exceed maximum")]
    InvalidBrightnessRange,
    #[error("image encoding failed: {0}")]
    Encode(String),
    #[error("file name is empty or invalid")]
    InvalidFileName,
    #[error("file name conflict: {0}")]
    FileNameConflict(String),
    #[error("stable hash must contain at least eight hexadecimal characters")]
    InvalidStableHash,
}

#[cfg(test)]
mod tests {
    use super::*;
    use image::{ImageBuffer, Luma};

    fn gradient() -> DynamicImage {
        DynamicImage::ImageLuma8(ImageBuffer::from_fn(64, 64, |x, y| {
            Luma([((x + y) % 256) as u8])
        }))
    }

    #[test]
    fn shift_to_edge_covers_the_roi_end() {
        let placements = plan_tiles(
            Roi {
                x: 10,
                y: 20,
                width: 100,
                height: 60,
            },
            TileConfig {
                tile_width: 48,
                tile_height: 40,
                overlap_x: 8,
                overlap_y: 0,
                edge_strategy: EdgeStrategy::ShiftToEdge,
            },
        )
        .expect("valid tile plan");
        let last = placements.last().expect("at least one tile");
        assert_eq!(last.source_x + last.source_width, 110);
        assert_eq!(last.source_y + last.source_height, 80);
    }

    #[test]
    fn identical_images_have_identical_hash_and_ssim() {
        let image = gradient();
        assert_eq!(
            hamming_distance(perceptual_hash(&image), perceptual_hash(&image)),
            0
        );
        assert!((global_ssim(&image, &image) - 1.0).abs() < 1e-9);
    }

    #[test]
    fn flat_images_are_low_information_and_have_no_sharp_edges() {
        let image = DynamicImage::ImageLuma8(ImageBuffer::from_pixel(64, 48, Luma([24])));
        let metrics = measure_quality(&image);
        assert_eq!(metrics.width, 64);
        assert_eq!(metrics.height, 48);
        assert_eq!(metrics.sharpness, 0.0);
        assert!(metrics.low_information > 0.95);
        assert_eq!(metrics.underexposed_ratio, 0.0);
    }

    #[test]
    fn deterministic_augmentation_reuses_the_same_parameter() {
        let image = gradient();
        let (_, first) = deterministic_brightness(&image, 42, -20, 20).unwrap();
        let (_, second) = deterministic_brightness(&image, 42, -20, 20).unwrap();
        assert_eq!(first, second);
    }

    #[test]
    fn render_tile_always_returns_configured_dimensions_and_constant_padding() {
        let image = DynamicImage::ImageRgba8(RgbaImage::from_pixel(4, 4, Rgba([10, 20, 30, 255])));
        let roi = Roi {
            x: 1,
            y: 1,
            width: 3,
            height: 3,
        };
        let config = TileRenderConfig {
            tile: TileConfig {
                tile_width: 5,
                tile_height: 5,
                overlap_x: 0,
                overlap_y: 0,
                edge_strategy: EdgeStrategy::Pad,
            },
            resize: ResizeMode::Stretch,
            padding: PaddingMode::Constant,
            fill: [1, 2, 3, 255],
        };
        let placement = plan_tiles(roi, config.tile).unwrap().remove(0);
        let rendered = render_tile(&image, roi, placement, config)
            .unwrap()
            .to_rgba8();
        assert_eq!((rendered.width(), rendered.height()), (5, 5));
        assert_eq!(rendered.get_pixel(4, 4), &Rgba([1, 2, 3, 255]));
    }

    #[test]
    fn edge_and_reflect_padding_sample_inside_roi() {
        let mut source = RgbaImage::new(2, 1);
        source.put_pixel(0, 0, Rgba([10, 0, 0, 255]));
        source.put_pixel(1, 0, Rgba([20, 0, 0, 255]));
        let image = DynamicImage::ImageRgba8(source);
        let roi = Roi {
            x: 0,
            y: 0,
            width: 2,
            height: 1,
        };
        let tile = TileConfig {
            tile_width: 4,
            tile_height: 1,
            overlap_x: 0,
            overlap_y: 0,
            edge_strategy: EdgeStrategy::Pad,
        };
        let placement = plan_tiles(roi, tile).unwrap().remove(0);
        let base = TileRenderConfig {
            tile,
            resize: ResizeMode::Stretch,
            padding: PaddingMode::Edge,
            fill: [0, 0, 0, 255],
        };
        let edge = render_tile(&image, roi, placement, base)
            .unwrap()
            .to_rgba8();
        assert_eq!(edge.get_pixel(3, 0), &Rgba([20, 0, 0, 255]));
        let reflected = render_tile(
            &image,
            roi,
            placement,
            TileRenderConfig {
                padding: PaddingMode::Reflect,
                ..base
            },
        )
        .unwrap()
        .to_rgba8();
        assert_eq!(reflected.get_pixel(2, 0), &Rgba([10, 0, 0, 255]));
        assert_eq!(reflected.get_pixel(3, 0), &Rgba([20, 0, 0, 255]));
    }

    #[test]
    fn sanitizes_windows_names_and_resolves_conflicts_without_overwrite() {
        assert_eq!(sanitize_file_stem(" CON. ").unwrap(), "_CON");
        assert_eq!(
            sanitize_file_stem("cam:01/frame?").unwrap(),
            "cam_01_frame_"
        );
        let mut occupied = HashSet::new();
        let first = resolve_file_name(
            "frame",
            "png",
            "0123456789abcdef",
            ConflictStrategy::AppendSequence,
            &mut occupied,
        )
        .unwrap();
        let second = resolve_file_name(
            "FRAME",
            ".PNG",
            "0123456789abcdef",
            ConflictStrategy::AppendSequence,
            &mut occupied,
        )
        .unwrap();
        assert_eq!(first.as_deref(), Some("frame.png"));
        assert_eq!(second.as_deref(), Some("FRAME_0002.png"));
    }

    #[test]
    fn exports_all_m3_image_formats() {
        let image =
            DynamicImage::ImageRgb8(ImageBuffer::from_pixel(8, 6, image::Rgb([12, 34, 56])));
        for format in [ExportFormat::Jpeg, ExportFormat::Png, ExportFormat::Webp] {
            let encoded = encode_image(&image, format).unwrap();
            assert!(!encoded.is_empty());
            let decoded =
                image::load_from_memory_with_format(&encoded, format.image_format()).unwrap();
            assert_eq!((decoded.width(), decoded.height()), (8, 6));
        }
    }
}
