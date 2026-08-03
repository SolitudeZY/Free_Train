use std::{
    fs::File,
    io::{BufRead, BufReader, Read},
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
        .args(["-v", "error", "-y", "-threads", "1", "-ss", &timestamp])
        .arg("-i")
        .arg(source.as_ref())
        .args(["-frames:v", "1", "-q:v", "2"])
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

pub fn extract_video_frames_batch<F>(
    executable: impl AsRef<Path>,
    source: impl AsRef<Path>,
    filter: &str,
    destination_dir: impl AsRef<Path>,
    duration_ms: u64,
    mut report_progress: F,
) -> Result<Vec<std::path::PathBuf>, MediaError>
where
    F: FnMut(u8),
{
    extract_video_frames_batch_controlled(
        executable,
        source,
        filter,
        destination_dir,
        duration_ms,
        |percent| {
            report_progress(percent);
            true
        },
    )
}

pub fn extract_video_frames_batch_controlled<F>(
    executable: impl AsRef<Path>,
    source: impl AsRef<Path>,
    filter: &str,
    destination_dir: impl AsRef<Path>,
    duration_ms: u64,
    mut report_progress: F,
) -> Result<Vec<std::path::PathBuf>, MediaError>
where
    F: FnMut(u8) -> bool,
{
    let executable = executable.as_ref();
    let source = source.as_ref();
    let destination_dir = destination_dir.as_ref();

    #[cfg(windows)]
    match run_video_frame_batch(
        executable,
        source,
        filter,
        destination_dir,
        duration_ms,
        &["-hwaccel", "d3d11va"],
        &mut report_progress,
    ) {
        Ok(paths) => return Ok(paths),
        Err(MediaError::Cancelled) => return Err(MediaError::Cancelled),
        Err(_) => reset_batch_directory(destination_dir)?,
    }

    run_video_frame_batch(
        executable,
        source,
        filter,
        destination_dir,
        duration_ms,
        &[],
        &mut report_progress,
    )
}

fn run_video_frame_batch<F>(
    executable: &Path,
    source: &Path,
    filter: &str,
    destination_dir: &Path,
    duration_ms: u64,
    input_args: &[&str],
    report_progress: &mut F,
) -> Result<Vec<std::path::PathBuf>, MediaError>
where
    F: FnMut(u8) -> bool,
{
    reset_batch_directory(destination_dir)?;
    let output_pattern = destination_dir.join("frame-%08d.jpg");
    let mut child = media_command(executable)
        .args(["-v", "error", "-y", "-threads", "1"])
        .args(input_args)
        .arg("-i")
        .arg(source)
        .args(["-an", "-sn", "-vf", filter, "-fps_mode", "vfr", "-q:v", "2"])
        .args(["-progress", "pipe:1", "-nostats"])
        .arg(&output_pattern)
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()?;
    let stdout = child.stdout.take().ok_or(MediaError::MissingProcessPipe)?;
    for line in BufReader::new(stdout).lines() {
        let line = line?;
        if let Some(value) = line.strip_prefix("out_time_us=")
            && let Ok(elapsed_us) = value.parse::<u64>()
            && duration_ms > 0
        {
            let percent = ((elapsed_us / 1_000).saturating_mul(100) / duration_ms).min(99) as u8;
            if !report_progress(percent) {
                let _ = child.kill();
                let _ = child.wait();
                let _ = reset_batch_directory(destination_dir);
                return Err(MediaError::Cancelled);
            }
        }
    }
    let output = child.wait_with_output()?;
    if !output.status.success() {
        return Err(MediaError::CommandFailedWithMessage(
            output.status.code(),
            String::from_utf8_lossy(&output.stderr).trim().to_owned(),
        ));
    }
    let mut paths = std::fs::read_dir(destination_dir)?
        .filter_map(Result::ok)
        .map(|entry| entry.path())
        .filter(|path| path.extension().is_some_and(|extension| extension == "jpg"))
        .collect::<Vec<_>>();
    paths.sort();
    if !report_progress(100) {
        let _ = reset_batch_directory(destination_dir);
        return Err(MediaError::Cancelled);
    }
    Ok(paths)
}

fn reset_batch_directory(path: &Path) -> Result<(), MediaError> {
    if path.exists() {
        std::fs::remove_dir_all(path)?;
    }
    std::fs::create_dir_all(path)?;
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
        .args(["-v", "error", "-y", "-threads", "1", "-ss", "0"])
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

pub fn create_browser_video_preview<F>(
    executable: impl AsRef<Path>,
    source: impl AsRef<Path>,
    destination: impl AsRef<Path>,
    duration_ms: u64,
    mut report_progress: F,
) -> Result<(), MediaError>
where
    F: FnMut(u8),
{
    let executable = executable.as_ref();
    let source = source.as_ref();
    let destination = destination.as_ref();
    let mut failures = Vec::new();
    let software = [
        "-c:v",
        "libx264",
        "-preset",
        "ultrafast",
        "-crf",
        "32",
        "-pix_fmt",
        "yuv420p",
        "-g",
        "30",
        "-keyint_min",
        "30",
        "-sc_threshold",
        "0",
    ];

    #[cfg(windows)]
    match run_browser_preview_encoder(
        executable,
        source,
        destination,
        duration_ms,
        &["-hwaccel", "d3d11va"],
        &software,
        &mut report_progress,
    ) {
        Ok(()) => return Ok(()),
        Err(error) => failures.push(format!("D3D11VA + libx264: {error}")),
    }

    match run_browser_preview_encoder(
        executable,
        source,
        destination,
        duration_ms,
        &[],
        &software,
        &mut report_progress,
    ) {
        Ok(()) => Ok(()),
        Err(error) => {
            failures.push(format!("libx264: {error}"));
            Err(MediaError::VideoPreviewFailed(failures.join("；")))
        }
    }
}

fn run_browser_preview_encoder<F>(
    executable: &Path,
    source: &Path,
    destination: &Path,
    duration_ms: u64,
    input_args: &[&str],
    encoder_args: &[&str],
    report_progress: &mut F,
) -> Result<(), MediaError>
where
    F: FnMut(u8),
{
    if destination.is_file() {
        std::fs::remove_file(destination)?;
    }
    let mut command = media_command(executable);
    command
        .args(["-v", "error", "-y", "-threads", "1"])
        .args(input_args)
        .arg("-i")
        .arg(source)
        .args(["-map", "0:v:0", "-an"])
        .args([
            "-vf",
            "fps=30,scale=960:540:force_original_aspect_ratio=decrease:force_divisible_by=2",
        ])
        .args(encoder_args)
        .args([
            "-fps_mode",
            "cfr",
            "-movflags",
            "+faststart",
            "-progress",
            "pipe:1",
            "-nostats",
        ])
        .arg(destination)
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());

    let mut child = command.spawn()?;
    let stdout = child.stdout.take().ok_or(MediaError::MissingProcessPipe)?;
    for line in BufReader::new(stdout).lines() {
        let line = line?;
        if let Some(value) = line.strip_prefix("out_time_us=")
            && let Ok(elapsed_us) = value.parse::<u64>()
            && duration_ms > 0
        {
            let percent = ((elapsed_us / 1_000).saturating_mul(100) / duration_ms).min(99) as u8;
            report_progress(percent);
        }
    }
    let output = child.wait_with_output()?;
    if !output.status.success() {
        let _ = std::fs::remove_file(destination);
        return Err(MediaError::CommandFailedWithMessage(
            output.status.code(),
            String::from_utf8_lossy(&output.stderr).trim().to_owned(),
        ));
    }
    validate_browser_video_preview(executable, destination, duration_ms)?;
    report_progress(100);
    Ok(())
}

pub fn validate_browser_video_preview(
    executable: impl AsRef<Path>,
    source: impl AsRef<Path>,
    duration_ms: u64,
) -> Result<(), MediaError> {
    let source = source.as_ref();
    let last_sample = duration_ms.saturating_sub(1_000);
    for timestamp_ms in [0, duration_ms / 2, last_sample] {
        let timestamp = format!("{:.6}", timestamp_ms as f64 / 1_000.0);
        let output = media_command(executable.as_ref())
            .args(["-v", "error", "-threads", "1", "-ss", &timestamp])
            .arg("-i")
            .arg(source)
            .args(["-frames:v", "1", "-f", "null", "-"])
            .stdout(Stdio::null())
            .output()?;
        if !output.status.success() {
            return Err(MediaError::CommandFailedWithMessage(
                output.status.code(),
                String::from_utf8_lossy(&output.stderr).trim().to_owned(),
            ));
        }
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
    #[error("无法生成浏览器兼容预览：{0}")]
    VideoPreviewFailed(String),
    #[error("操作已取消")]
    Cancelled,
}

#[cfg(test)]
mod tests {
    use super::*;

    fn test_root(label: &str) -> std::path::PathBuf {
        std::env::temp_dir().join(format!(
            "free-train-media-{label}-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ))
    }

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
    fn batch_extracts_regular_video_frames_with_progress() {
        if probe_ffprobe("ffmpeg").is_err() {
            return;
        }
        let root = test_root("batch");
        std::fs::create_dir_all(&root).unwrap();
        let source = root.join("source.mp4");
        let output = media_command("ffmpeg")
            .args([
                "-v",
                "error",
                "-y",
                "-f",
                "lavfi",
                "-i",
                "testsrc=size=320x180:rate=30:duration=3",
                "-c:v",
                "mpeg4",
            ])
            .arg(&source)
            .output()
            .unwrap();
        if !output.status.success() {
            let _ = std::fs::remove_dir_all(root);
            return;
        }
        let mut progress = Vec::new();
        let frames = extract_video_frames_batch(
            "ffmpeg",
            &source,
            "fps=1",
            root.join("frames"),
            3_000,
            |percent| progress.push(percent),
        )
        .unwrap();
        assert_eq!(frames.len(), 3);
        assert_eq!(progress.last(), Some(&100));
        assert!(frames.iter().all(|path| path.is_file()));
        std::fs::remove_dir_all(root).unwrap();
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
