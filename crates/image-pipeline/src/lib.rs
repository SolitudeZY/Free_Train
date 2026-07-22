use domain::{EdgeStrategy, Roi, TileConfig};
use image::{DynamicImage, GrayImage, imageops::FilterType};
use rand::{Rng, SeedableRng};
use rand_chacha::ChaCha8Rng;
use rustdct::DctPlanner;
use thiserror::Error;

const PHASH_SAMPLE_SIZE: usize = 32;
const PHASH_LOW_FREQUENCIES: usize = 8;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
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
    fn deterministic_augmentation_reuses_the_same_parameter() {
        let image = gradient();
        let (_, first) = deterministic_brightness(&image, 42, -20, 20).unwrap();
        let (_, second) = deterministic_brightness(&image, 42, -20, 20).unwrap();
        assert_eq!(first, second);
    }
}
