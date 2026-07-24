use std::{
    fs::File,
    io::{BufReader, Read},
    path::Path,
    process::{Command, Stdio},
};

#[cfg(windows)]
use std::os::windows::process::CommandExt;

use exif::{In, Reader, Tag};
use image::{DynamicImage, ImageFormat};
use serde::{Deserialize, Serialize};
use thiserror::Error;

#[cfg(windows)]
const CREATE_NO_WINDOW: u32 = 0x0800_0000;

fn media_command(executable: impl AsRef<Path>) -> Command {
    let mut command = Command::new(executable.as_ref());
    #[cfg(windows)]
    command.creation_flags(CREATE_NO_WINDOW);
    command
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FfprobeVersion {
    pub first_line: String,
    pub is_full_gpl_build: bool,
}

pub fn probe_ffprobe(executable: impl AsRef<Path>) -> Result<FfprobeVersion, MediaError> {
    let output = media_command(executable).arg("-version").output()?;
    if !output.status.success() {
        return Err(MediaError::CommandFailed(output.status.code()));
    }
    let stdout = String::from_utf8(output.stdout)?;
    let first_line = stdout.lines().next().unwrap_or_default().to_owned();
    let is_full_gpl_build = stdout.contains("--enable-gpl") || stdout.contains("full_build");
    Ok(FfprobeVersion {
        first_line,
        is_full_gpl_build,
    })
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ProbeDocument {
    #[serde(default)]
    pub streams: Vec<ProbeStream>,
    pub format: Option<ProbeFormat>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ProbeStream {
    pub index: u32,
    pub codec_type: Option<String>,
    pub codec_name: Option<String>,
    pub width: Option<u32>,
    pub height: Option<u32>,
    pub avg_frame_rate: Option<String>,
    #[serde(default)]
    pub tags: std::collections::HashMap<String, String>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ProbeFormat {
    pub filename: Option<String>,
    pub duration: Option<String>,
    pub size: Option<String>,
    #[serde(default)]
    pub tags: std::collections::HashMap<String, String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ImageInfo {
    pub width: u32,
    pub height: u32,
    pub orientation: u32,
    pub capture_time: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct VideoInfo {
    pub width: Option<u32>,
    pub height: Option<u32>,
    pub duration_ms: Option<u64>,
    pub codec: Option<String>,
    pub frame_rate: Option<String>,
    pub capture_time: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct FrameProbeDocument {
    #[serde(default)]
    pub frames: Vec<FrameProbe>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct FrameProbe {
    pub best_effort_timestamp_time: Option<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ChangePoint {
    pub timestamp_ms: u64,
    pub score: f64,
}

pub fn parse_probe_json(json: &str) -> Result<ProbeDocument, MediaError> {
    Ok(serde_json::from_str(json)?)
}

pub fn probe_media(
    executable: impl AsRef<Path>,
    source: impl AsRef<Path>,
) -> Result<ProbeDocument, MediaError> {
    let output = media_command(executable)
        .args([
            "-v",
            "error",
            "-show_streams",
            "-show_format",
            "-of",
            "json",
        ])
        .arg(source.as_ref())
        .output()?;
    if !output.status.success() {
        return Err(MediaError::CommandFailed(output.status.code()));
    }
    let stdout = String::from_utf8(output.stdout)?;
    parse_probe_json(&stdout)
}

pub fn inspect_image(source: impl AsRef<Path>) -> Result<ImageInfo, MediaError> {
    let source = source.as_ref();
    let (width, height) = image::image_dimensions(source)?;
    let mut orientation = 1_u32;
    let mut capture_time = None;
    if let Ok(file) = File::open(source)
        && let Ok(exif) = Reader::new().read_from_container(&mut BufReader::new(file))
    {
        orientation = exif
            .get_field(Tag::Orientation, In::PRIMARY)
            .and_then(|field| field.value.get_uint(0))
            .unwrap_or(1);
        capture_time = exif
            .get_field(Tag::DateTimeOriginal, In::PRIMARY)
            .or_else(|| exif.get_field(Tag::DateTime, In::PRIMARY))
            .map(|field| field.display_value().with_unit(&exif).to_string());
    }
    let (display_width, display_height) = if matches!(orientation, 5..=8) {
        (height, width)
    } else {
        (width, height)
    };
    Ok(ImageInfo {
        width: display_width,
        height: display_height,
        orientation,
        capture_time,
    })
}

pub fn video_info(document: &ProbeDocument) -> VideoInfo {
    let stream = document
        .streams
        .iter()
        .find(|stream| stream.codec_type.as_deref() == Some("video"));
    let duration_ms = document
        .format
        .as_ref()
        .and_then(|format| format.duration.as_deref())
        .and_then(|duration| duration.parse::<f64>().ok())
        .map(|duration| (duration * 1000.0).round() as u64);
    let capture_time = stream
        .and_then(|stream| stream.tags.get("creation_time").cloned())
        .or_else(|| {
            document
                .format
                .as_ref()
                .and_then(|format| format.tags.get("creation_time").cloned())
        });
    VideoInfo {
        width: stream.and_then(|stream| stream.width),
        height: stream.and_then(|stream| stream.height),
        duration_ms,
        codec: stream.and_then(|stream| stream.codec_name.clone()),
        frame_rate: stream.and_then(|stream| stream.avg_frame_rate.clone()),
        capture_time,
    }
}

pub fn probe_frame_timestamps(
    executable: impl AsRef<Path>,
    source: impl AsRef<Path>,
) -> Result<Vec<u64>, MediaError> {
    let output = media_command(executable)
        .args([
            "-v",
            "error",
            "-select_streams",
            "v:0",
            "-show_frames",
            "-show_entries",
            "frame=best_effort_timestamp_time",
            "-of",
            "json",
        ])
        .arg(source.as_ref())
        .output()?;
    if !output.status.success() {
        return Err(MediaError::CommandFailedWithMessage(
            output.status.code(),
            String::from_utf8_lossy(&output.stderr).trim().to_owned(),
        ));
    }
    let document: FrameProbeDocument = serde_json::from_slice(&output.stdout)?;
    let mut timestamps = document
        .frames
        .into_iter()
        .filter_map(|frame| frame.best_effort_timestamp_time)
        .filter_map(|value| value.parse::<f64>().ok())
        .filter(|value| value.is_finite() && *value >= 0.0)
        .map(|value| (value * 1000.0).round() as u64)
        .collect::<Vec<_>>();
    timestamps.sort_unstable();
    timestamps.dedup();
    Ok(timestamps)
}

pub fn extract_video_frame(
    executable: impl AsRef<Path>,
    source: impl AsRef<Path>,
    timestamp_ms: u64,
    destination: impl AsRef<Path>,
) -> Result<(), MediaError> {
    let timestamp = format!("{:.6}", timestamp_ms as f64 / 1000.0);
    let output = media_command(executable)
        .args(["-v", "error", "-y"])
        .arg("-i")
        .arg(source.as_ref())
        .args(["-ss", &timestamp, "-frames:v", "1", "-q:v", "2"])
        .arg(destination.as_ref())
        .output()?;
    if !output.status.success() {
        return Err(MediaError::CommandFailedWithMessage(
            output.status.code(),
            String::from_utf8_lossy(&output.stderr).trim().to_owned(),
        ));
    }
    Ok(())
}

pub fn analyze_video_changes(
    executable: impl AsRef<Path>,
    source: impl AsRef<Path>,
    source_width: u32,
    source_height: u32,
    analysis_fps: f64,
) -> Result<Vec<ChangePoint>, MediaError> {
    if source_width == 0 || source_height == 0 || !(0.1..=30.0).contains(&analysis_fps) {
        return Err(MediaError::InvalidAnalysisConfiguration);
    }
    let analysis_width = 160_u32;
    let raw_height = ((source_height as f64 * analysis_width as f64 / source_width as f64).round()
        as u32)
        .max(2);
    let analysis_height = raw_height + raw_height % 2;
    let filter = format!(
        "fps={analysis_fps:.4},scale={analysis_width}:{analysis_height}:flags=fast_bilinear,format=gray"
    );
    let mut child = media_command(executable)
        .args(["-v", "error", "-i"])
        .arg(source.as_ref())
        .args([
            "-an", "-sn", "-vf", &filter, "-pix_fmt", "gray", "-f", "rawvideo", "-",
        ])
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()?;
    let mut stdout = child.stdout.take().ok_or(MediaError::MissingProcessPipe)?;
    let frame_size = (analysis_width * analysis_height) as usize;
    let mut current = vec![0_u8; frame_size];
    let mut previous: Option<Vec<u8>> = None;
    let mut points = Vec::new();
    let mut frame_index = 0_u64;
    loop {
        let mut read = 0;
        while read < frame_size {
            let count = stdout.read(&mut current[read..])?;
            if count == 0 {
                break;
            }
            read += count;
        }
        if read != frame_size {
            break;
        }
        let score = previous.as_ref().map_or(0.0, |before| {
            before
                .iter()
                .zip(&current)
                .map(|(left, right)| (*left as f64 - *right as f64).abs())
                .sum::<f64>()
                / (frame_size as f64 * 255.0)
        });
        points.push(ChangePoint {
            timestamp_ms: (frame_index as f64 * 1000.0 / analysis_fps).round() as u64,
            score,
        });
        previous = Some(current.clone());
        frame_index += 1;
    }
    let status = child.wait()?;
    if !status.success() {
        let mut message = String::new();
        if let Some(mut stderr) = child.stderr.take() {
            stderr.read_to_string(&mut message)?;
        }
        return Err(MediaError::CommandFailedWithMessage(
            status.code(),
            message.trim().to_owned(),
        ));
    }
    Ok(points)
}

pub fn create_image_thumbnail(
    source: impl AsRef<Path>,
    destination: impl AsRef<Path>,
) -> Result<(), MediaError> {
    let mut image = image::open(source.as_ref())?;
    if let Ok(file) = File::open(source.as_ref())
        && let Ok(exif) = Reader::new().read_from_container(&mut BufReader::new(file))
        && let Some(orientation) = exif
            .get_field(Tag::Orientation, In::PRIMARY)
            .and_then(|field| field.value.get_uint(0))
    {
        image = apply_orientation(image, orientation);
    }
    let thumbnail = image.thumbnail(320, 180).to_rgb8();
    thumbnail.save_with_format(destination, ImageFormat::Jpeg)?;
    Ok(())
}

pub fn load_oriented_image(source: impl AsRef<Path>) -> Result<DynamicImage, MediaError> {
    let source = source.as_ref();
    let mut image = image::open(source)?;
    if let Ok(file) = File::open(source)
        && let Ok(exif) = Reader::new().read_from_container(&mut BufReader::new(file))
        && let Some(orientation) = exif
            .get_field(Tag::Orientation, In::PRIMARY)
            .and_then(|field| field.value.get_uint(0))
    {
        image = apply_orientation(image, orientation);
    }
    Ok(image)
}

pub fn create_video_thumbnail(
    executable: impl AsRef<Path>,
    source: impl AsRef<Path>,
    destination: impl AsRef<Path>,
) -> Result<(), MediaError> {
    let output = media_command(executable)
        .args(["-v", "error", "-y", "-ss", "0"])
        .arg("-i")
        .arg(source.as_ref())
        .args([
            "-frames:v",
            "1",
            "-vf",
            "scale=320:-2:force_original_aspect_ratio=decrease",
        ])
        .arg(destination.as_ref())
        .output()?;
    if !output.status.success() {
        return Err(MediaError::CommandFailedWithMessage(
            output.status.code(),
            String::from_utf8_lossy(&output.stderr).trim().to_owned(),
        ));
    }
    Ok(())
}

fn apply_orientation(image: DynamicImage, orientation: u32) -> DynamicImage {
    match orientation {
        2 => image.fliph(),
        3 => image.rotate180(),
        4 => image.flipv(),
        5 => image.rotate90().fliph(),
        6 => image.rotate90(),
        7 => image.rotate270().fliph(),
        8 => image.rotate270(),
        _ => image,
    }
}

#[derive(Debug, Error)]
pub enum MediaError {
    #[error("failed to start ffprobe: {0}")]
    Io(#[from] std::io::Error),
    #[error("ffprobe returned a failure status: {0:?}")]
    CommandFailed(Option<i32>),
    #[error("ffmpeg returned a failure status {0:?}: {1}")]
    CommandFailedWithMessage(Option<i32>, String),
    #[error("ffprobe output is not UTF-8: {0}")]
    Utf8(#[from] std::string::FromUtf8Error),
    #[error("ffprobe JSON is invalid: {0}")]
    Json(#[from] serde_json::Error),
    #[error("image operation failed: {0}")]
    Image(#[from] image::ImageError),
    #[error("invalid video change analysis configuration")]
    InvalidAnalysisConfiguration,
    #[error("media process did not expose the expected pipe")]
    MissingProcessPipe,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_minimal_ffprobe_document() {
        let document = parse_probe_json(
            r#"{"streams":[{"index":0,"codec_type":"video","codec_name":"h264","width":1920,"height":1080,"avg_frame_rate":"30000/1001"}],"format":{"filename":"sample.mp4","duration":"2.50","size":"4096"}}"#,
        )
        .expect("valid ffprobe JSON");
        assert_eq!(document.streams[0].width, Some(1920));
        assert_eq!(document.format.unwrap().duration.as_deref(), Some("2.50"));
    }

    #[test]
    fn parses_frame_timestamp_document() {
        let document: FrameProbeDocument = serde_json::from_str(
            r#"{"frames":[{"best_effort_timestamp_time":"0.000000"},{"best_effort_timestamp_time":"0.033333"}]}"#,
        )
        .unwrap();
        assert_eq!(document.frames.len(), 2);
    }
}
