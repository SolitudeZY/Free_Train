use super::*;

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct VideoPreview {
    pub path: String,
    pub is_proxy: bool,
}

#[derive(Debug, Clone)]
pub struct VideoPreviewPlan {
    source_path: PathBuf,
    output_path: PathBuf,
    duration_ms: u64,
    direct: bool,
}

pub fn plan_video_preview(
    session: &ProjectSession,
    source_id: &str,
    force_transcode: bool,
) -> Result<VideoPreviewPlan, ApplicationError> {
    let source = ProjectStore::open(session.database_path())?
        .get_source(source_id)?
        .ok_or_else(|| ApplicationError::SourceNotFound(source_id.to_owned()))?;
    if source.kind != SourceKind::Video {
        return Err(ApplicationError::SourceIsNotVideo);
    }
    if source.status != SourceStatus::Online {
        return Err(ApplicationError::SourceOffline(source.absolute_path));
    }
    let source_path = PathBuf::from(&source.absolute_path);
    if !force_transcode && browser_can_play_directly(&source) {
        return Ok(VideoPreviewPlan {
            source_path: source_path.clone(),
            output_path: source_path,
            duration_ms: source.duration_ms.unwrap_or_default(),
            direct: true,
        });
    }

    let preview_dir = session.project_dir().join("cache").join("video-previews");
    fs::create_dir_all(&preview_dir)?;
    Ok(VideoPreviewPlan {
        source_path,
        output_path: preview_dir.join(format!("{source_id}-webview-v1.mp4")),
        duration_ms: source
            .duration_ms
            .ok_or(ApplicationError::MissingDuration)?,
        direct: false,
    })
}

pub fn execute_video_preview<F>(
    plan: VideoPreviewPlan,
    ffmpeg: &Path,
    report_progress: F,
) -> Result<VideoPreview, ApplicationError>
where
    F: FnMut(u8),
{
    if plan.direct {
        return Ok(VideoPreview {
            path: browser_path_text(&plan.source_path),
            is_proxy: false,
        });
    }
    if plan
        .output_path
        .metadata()
        .is_ok_and(|metadata| metadata.len() > 0)
    {
        if media::validate_browser_video_preview(ffmpeg, &plan.output_path, plan.duration_ms)
            .is_ok()
        {
            return Ok(VideoPreview {
                path: browser_path_text(&plan.output_path),
                is_proxy: true,
            });
        }
        let _ = fs::remove_file(&plan.output_path);
    }

    let temporary = plan.output_path.with_extension("partial.mp4");
    if let Err(error) = media::create_browser_video_preview(
        ffmpeg,
        &plan.source_path,
        &temporary,
        plan.duration_ms,
        report_progress,
    ) {
        let _ = fs::remove_file(&temporary);
        return Err(error.into());
    }
    fs::rename(&temporary, &plan.output_path)?;
    Ok(VideoPreview {
        path: browser_path_text(&plan.output_path),
        is_proxy: true,
    })
}

fn browser_path_text(path: &Path) -> String {
    let value = path_text(path);
    #[cfg(windows)]
    {
        if let Some(unc) = value.strip_prefix(r"\\?\UNC\") {
            return format!(r"\\{unc}");
        }
        if let Some(local) = value.strip_prefix(r"\\?\") {
            return local.to_owned();
        }
    }
    value
}

fn browser_can_play_directly(source: &StoredSourceAsset) -> bool {
    let extension = Path::new(&source.file_name)
        .extension()
        .and_then(|value| value.to_str())
        .unwrap_or_default()
        .to_ascii_lowercase();
    let codec = source
        .codec
        .as_deref()
        .unwrap_or_default()
        .to_ascii_lowercase();
    matches!(
        (codec.as_str(), extension.as_str()),
        ("h264", "mp4" | "m4v" | "mov") | ("av1", "mp4" | "webm") | ("vp8" | "vp9", "webm")
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    fn source(codec: &str, file_name: &str) -> StoredSourceAsset {
        StoredSourceAsset {
            id: "source".to_owned(),
            absolute_path: file_name.to_owned(),
            file_name: file_name.to_owned(),
            relative_folder: String::new(),
            source_group: "group".to_owned(),
            source_identifier: "source".to_owned(),
            kind: SourceKind::Video,
            status: SourceStatus::Online,
            size_bytes: 1,
            modified_unix_ms: 0,
            quick_fingerprint: "fingerprint".to_owned(),
            sha256: None,
            width: Some(1920),
            height: Some(1080),
            duration_ms: Some(1_000),
            codec: Some(codec.to_owned()),
            frame_rate: Some("30/1".to_owned()),
            capture_time: None,
            capture_time_source: None,
            orientation: None,
            thumbnail_path: None,
            error: None,
            imported_at: String::new(),
            last_checked_at: String::new(),
        }
    }

    #[test]
    fn hevc_mp4_requires_a_browser_proxy() {
        assert!(!browser_can_play_directly(&source("hevc", "long.mp4")));
    }

    #[test]
    fn h264_mp4_can_use_the_source_directly() {
        assert!(browser_can_play_directly(&source("h264", "clip.mp4")));
    }

    #[cfg(windows)]
    #[test]
    fn browser_paths_remove_the_windows_extended_length_prefix() {
        assert_eq!(
            browser_path_text(Path::new(r"\\?\E:\video cache\preview.mp4")),
            r"E:\video cache\preview.mp4"
        );
        assert_eq!(
            browser_path_text(Path::new(r"\\?\UNC\server\share\preview.mp4")),
            r"\\server\share\preview.mp4"
        );
    }
}
