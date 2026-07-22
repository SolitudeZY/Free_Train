use std::{
    fs::{self, File, OpenOptions},
    io::{Read, Seek, SeekFrom},
    path::{Path, PathBuf},
    time::UNIX_EPOCH,
};

use chrono::Utc;
use domain::{SamplingMode, SourceKind, SourceStatus, VideoRange};
use fs2::FileExt;
use media::ChangePoint;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use storage::{ProjectStore, StoredCandidateImage, StoredSourceAsset, StoredVideoSelection};
use thiserror::Error;
use uuid::Uuid;
use walkdir::WalkDir;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum HealthState {
    Ready,
    Warning,
    Blocked,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ComponentHealth {
    pub id: String,
    pub label: String,
    pub state: HealthState,
    pub detail: String,
}

impl ComponentHealth {
    pub fn ready(
        id: impl Into<String>,
        label: impl Into<String>,
        detail: impl Into<String>,
    ) -> Self {
        Self {
            id: id.into(),
            label: label.into(),
            state: HealthState::Ready,
            detail: detail.into(),
        }
    }

    pub fn warning(
        id: impl Into<String>,
        label: impl Into<String>,
        detail: impl Into<String>,
    ) -> Self {
        Self {
            id: id.into(),
            label: label.into(),
            state: HealthState::Warning,
            detail: detail.into(),
        }
    }

    pub fn blocked(
        id: impl Into<String>,
        label: impl Into<String>,
        detail: impl Into<String>,
    ) -> Self {
        Self {
            id: id.into(),
            label: label.into(),
            state: HealthState::Blocked,
            detail: detail.into(),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct M0Status {
    pub app_version: String,
    pub target: String,
    pub components: Vec<ComponentHealth>,
}

impl M0Status {
    pub fn is_ready(&self) -> bool {
        self.components
            .iter()
            .all(|component| component.state != HealthState::Blocked)
    }
}

const PROJECT_MANIFEST: &str = "project.json";
const PROJECT_DATABASE: &str = "project.db";
const LOCK_FILE: &str = ".free-train.lock";
const FULL_HASH_LIMIT: u64 = 64 * 1024 * 1024;

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ProjectSummary {
    pub id: String,
    pub name: String,
    pub path: String,
    pub created_at: String,
    pub source_count: u64,
    pub offline_count: u64,
    pub candidate_count: u64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ProjectManifest {
    pub schema_version: u32,
    pub id: String,
    pub name: String,
    pub created_at: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ImportFailure {
    pub path: String,
    pub error: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ImportResult {
    pub discovered: u64,
    pub imported: u64,
    pub updated: u64,
    pub unsupported: u64,
    pub failures: Vec<ImportFailure>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SamplingConfig {
    pub mode: SamplingMode,
    pub interval_ms: u64,
    pub frame_interval: u64,
    pub target_count: u64,
    #[serde(default)]
    pub range_ids: Vec<String>,
    #[serde(default)]
    pub custom_timestamps_ms: Vec<u64>,
    pub pin_results: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SamplingEstimate {
    pub timestamps_ms: Vec<u64>,
    pub estimated_count: u64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct GroupSamplingEstimate {
    pub source_count: u64,
    pub estimated_count: u64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CaptureResult {
    pub candidate: StoredCandidateImage,
    pub created: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SamplingExecutionResult {
    pub planned: u64,
    pub created: u64,
    pub existing: u64,
    pub failures: Vec<ImportFailure>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ChangeAnalysisResult {
    pub points: Vec<ChangePoint>,
    pub suggested_timestamps_ms: Vec<u64>,
}

#[derive(Debug)]
pub struct ProjectSession {
    pub summary: ProjectSummary,
    project_dir: PathBuf,
    _lock: File,
}

impl ProjectSession {
    pub fn create(parent: impl AsRef<Path>, name: &str) -> Result<Self, ApplicationError> {
        validate_project_name(name)?;
        let directory_name = if name.to_ascii_lowercase().ends_with(".ftproj") {
            name.to_owned()
        } else {
            format!("{name}.ftproj")
        };
        let project_dir = parent.as_ref().join(directory_name);
        fs::create_dir(&project_dir).map_err(|error| {
            if error.kind() == std::io::ErrorKind::AlreadyExists {
                ApplicationError::ProjectExists(project_dir.clone())
            } else {
                ApplicationError::Io(error)
            }
        })?;
        fs::create_dir_all(project_dir.join("cache").join("thumbnails"))?;
        fs::create_dir_all(project_dir.join("backups"))?;
        let manifest = ProjectManifest {
            schema_version: 1,
            id: Uuid::new_v4().to_string(),
            name: name.trim_end_matches(".ftproj").to_owned(),
            created_at: Utc::now().to_rfc3339(),
        };
        write_json_atomic(&project_dir.join(PROJECT_MANIFEST), &manifest)?;
        let store = ProjectStore::open(project_dir.join(PROJECT_DATABASE))?;
        store.set_meta("project_id", &manifest.id)?;
        store.set_meta("project_name", &manifest.name)?;
        store.set_meta("created_at", &manifest.created_at)?;
        Self::open(project_dir)
    }

    pub fn open(project_dir: impl AsRef<Path>) -> Result<Self, ApplicationError> {
        let project_dir = project_dir.as_ref().canonicalize()?;
        let manifest_path = project_dir.join(PROJECT_MANIFEST);
        if !manifest_path.is_file() {
            return Err(ApplicationError::InvalidProject(project_dir));
        }
        let lock = OpenOptions::new()
            .read(true)
            .write(true)
            .create(true)
            .truncate(false)
            .open(project_dir.join(LOCK_FILE))?;
        lock.try_lock_exclusive()
            .map_err(|_| ApplicationError::ProjectLocked(project_dir.clone()))?;
        let manifest: ProjectManifest = serde_json::from_slice(&fs::read(&manifest_path)?)?;
        let store = ProjectStore::open(project_dir.join(PROJECT_DATABASE))?;
        fs::create_dir_all(project_dir.join("cache").join("thumbnails"))?;
        fs::create_dir_all(project_dir.join("backups"))?;
        let (source_count, offline_count) = store.counts()?;
        let candidate_count = store.candidate_count()?;
        Ok(Self {
            summary: ProjectSummary {
                id: manifest.id,
                name: manifest.name,
                path: path_text(&project_dir),
                created_at: manifest.created_at,
                source_count,
                offline_count,
                candidate_count,
            },
            project_dir,
            _lock: lock,
        })
    }

    pub fn database_path(&self) -> PathBuf {
        self.project_dir.join(PROJECT_DATABASE)
    }

    pub fn project_dir(&self) -> &Path {
        &self.project_dir
    }

    pub fn backup_database(&self) -> Result<(), ApplicationError> {
        ProjectStore::open(self.database_path())?
            .backup_to(self.project_dir.join("backups").join("project-open.bak"))?;
        Ok(())
    }

    pub fn refresh_summary(&mut self) -> Result<(), ApplicationError> {
        let (source_count, offline_count) = ProjectStore::open(self.database_path())?.counts()?;
        let candidate_count = ProjectStore::open(self.database_path())?.candidate_count()?;
        self.summary.source_count = source_count;
        self.summary.offline_count = offline_count;
        self.summary.candidate_count = candidate_count;
        Ok(())
    }
}

pub fn import_sources(
    session: &mut ProjectSession,
    inputs: &[String],
    ffprobe: &Path,
    ffmpeg: &Path,
) -> Result<ImportResult, ApplicationError> {
    let store = ProjectStore::open(session.database_path())?;
    let thumbnail_dir = session.project_dir.join("cache").join("thumbnails");
    let mut result = ImportResult {
        discovered: 0,
        imported: 0,
        updated: 0,
        unsupported: 0,
        failures: Vec::new(),
    };

    for input in inputs {
        let input_path = PathBuf::from(input);
        if !input_path.exists() {
            result.failures.push(ImportFailure {
                path: input.clone(),
                error: "路径不存在或不可访问".to_owned(),
            });
            continue;
        }
        let root = if input_path.is_dir() {
            input_path.clone()
        } else {
            input_path
                .parent()
                .map(Path::to_path_buf)
                .unwrap_or_else(|| input_path.clone())
        }
        .canonicalize()
        .unwrap_or_else(|_| input_path.clone());
        let candidates: Vec<PathBuf> = if input_path.is_dir() {
            WalkDir::new(&input_path)
                .follow_links(false)
                .into_iter()
                .filter_map(Result::ok)
                .filter(|entry| entry.file_type().is_file())
                .map(|entry| entry.into_path())
                .collect()
        } else {
            vec![input_path]
        };
        for path in candidates {
            result.discovered += 1;
            let Some(kind) = SourceKind::from_path(&path) else {
                result.unsupported += 1;
                continue;
            };
            match inspect_and_store(&store, &thumbnail_dir, &root, &path, kind, ffprobe, ffmpeg) {
                Ok(true) => result.updated += 1,
                Ok(false) => result.imported += 1,
                Err(error) => result.failures.push(ImportFailure {
                    path: path_text(&path),
                    error: error.to_string(),
                }),
            }
        }
    }
    session.refresh_summary()?;
    Ok(result)
}

pub fn list_sources(
    session: &ProjectSession,
    offset: u32,
    limit: u32,
) -> Result<Vec<StoredSourceAsset>, ApplicationError> {
    Ok(ProjectStore::open(session.database_path())?.list_sources(offset, limit.min(10_000))?)
}

pub fn video_frame_timestamps(
    session: &mut ProjectSession,
    source_id: &str,
    ffprobe: &Path,
) -> Result<Vec<u64>, ApplicationError> {
    let source = require_online_video(session, source_id)?;
    let timestamps = media::probe_frame_timestamps(ffprobe, &source.absolute_path)?;
    if timestamps.is_empty() {
        return Err(ApplicationError::NoVideoFrames);
    }
    Ok(timestamps)
}

pub fn create_video_selection(
    session: &mut ProjectSession,
    source_id: &str,
    start_ms: u64,
    end_ms: u64,
    protected: bool,
) -> Result<StoredVideoSelection, ApplicationError> {
    let source = require_online_video(session, source_id)?;
    let duration = source
        .duration_ms
        .ok_or(ApplicationError::MissingDuration)?;
    VideoRange { start_ms, end_ms }.validate(duration)?;
    let store = ProjectStore::open(session.database_path())?;
    let selection = StoredVideoSelection {
        id: Uuid::new_v4().to_string(),
        source_id: source_id.to_owned(),
        start_ms,
        end_ms,
        label: format!(
            "有效片段 {}",
            store.list_video_selections(source_id)?.len() + 1
        ),
        protected,
        created_at: Utc::now().to_rfc3339(),
    };
    store.insert_video_selection(&selection)?;
    Ok(selection)
}

pub fn list_video_selections(
    session: &ProjectSession,
    source_id: &str,
) -> Result<Vec<StoredVideoSelection>, ApplicationError> {
    Ok(ProjectStore::open(session.database_path())?.list_video_selections(source_id)?)
}

pub fn delete_video_selection(
    session: &ProjectSession,
    selection_id: &str,
) -> Result<bool, ApplicationError> {
    Ok(ProjectStore::open(session.database_path())?.delete_video_selection(selection_id)?)
}

pub fn list_candidates(
    session: &ProjectSession,
    source_id: &str,
    offset: u32,
    limit: u32,
) -> Result<Vec<StoredCandidateImage>, ApplicationError> {
    Ok(
        ProjectStore::open(session.database_path())?.list_candidates(
            source_id,
            offset,
            limit.min(10_000),
        )?,
    )
}

pub fn capture_manual_frame(
    session: &mut ProjectSession,
    source_id: &str,
    requested_timestamp_ms: u64,
    ffprobe: &Path,
    ffmpeg: &Path,
) -> Result<CaptureResult, ApplicationError> {
    let source = require_online_video(session, source_id)?;
    let frame_timestamps = media::probe_frame_timestamps(ffprobe, &source.absolute_path)?;
    let timestamp_ms = nearest_timestamp(&frame_timestamps, requested_timestamp_ms)
        .ok_or(ApplicationError::NoVideoFrames)?;
    let result = create_candidate(
        session,
        CandidateRequest {
            source: &source,
            frame_timestamps: &frame_timestamps,
            timestamp_ms,
            selection_method: "manual",
            parameters_json: "{}",
            pinned: true,
        },
        ffmpeg,
    )?;
    session.refresh_summary()?;
    Ok(result)
}

pub fn estimate_sampling(
    session: &mut ProjectSession,
    source_id: &str,
    config: &SamplingConfig,
    ffprobe: &Path,
) -> Result<SamplingEstimate, ApplicationError> {
    let source = require_online_video(session, source_id)?;
    let duration = source
        .duration_ms
        .ok_or(ApplicationError::MissingDuration)?;
    let frame_timestamps = media::probe_frame_timestamps(ffprobe, &source.absolute_path)?;
    let ranges = ProjectStore::open(session.database_path())?.list_video_selections(source_id)?;
    let timestamps_ms = plan_sampling_times(&frame_timestamps, duration, &ranges, config)?;
    Ok(SamplingEstimate {
        estimated_count: timestamps_ms.len() as u64,
        timestamps_ms,
    })
}

pub fn execute_sampling(
    session: &mut ProjectSession,
    source_id: &str,
    config: &SamplingConfig,
    ffprobe: &Path,
    ffmpeg: &Path,
) -> Result<SamplingExecutionResult, ApplicationError> {
    let source = require_online_video(session, source_id)?;
    let duration = source
        .duration_ms
        .ok_or(ApplicationError::MissingDuration)?;
    let frame_timestamps = media::probe_frame_timestamps(ffprobe, &source.absolute_path)?;
    let ranges = ProjectStore::open(session.database_path())?.list_video_selections(source_id)?;
    let timestamps = plan_sampling_times(&frame_timestamps, duration, &ranges, config)?;
    let parameters = serde_json::to_string(config)?;
    let method = sampling_method_text(config.mode);
    let mut result = SamplingExecutionResult {
        planned: timestamps.len() as u64,
        created: 0,
        existing: 0,
        failures: Vec::new(),
    };
    for timestamp_ms in timestamps {
        let pinned = config.pin_results
            || ranges.iter().any(|selection| {
                selection.protected
                    && timestamp_ms >= selection.start_ms
                    && timestamp_ms < selection.end_ms
            });
        match create_candidate(
            session,
            CandidateRequest {
                source: &source,
                frame_timestamps: &frame_timestamps,
                timestamp_ms,
                selection_method: method,
                parameters_json: &parameters,
                pinned,
            },
            ffmpeg,
        ) {
            Ok(capture) if capture.created => result.created += 1,
            Ok(_) => result.existing += 1,
            Err(error) => result.failures.push(ImportFailure {
                path: format!("{} @ {timestamp_ms} ms", source.file_name),
                error: error.to_string(),
            }),
        }
    }
    session.refresh_summary()?;
    Ok(result)
}

pub fn estimate_group_sampling(
    session: &mut ProjectSession,
    source_group: &str,
    config: &SamplingConfig,
    ffprobe: &Path,
) -> Result<GroupSamplingEstimate, ApplicationError> {
    validate_group_sampling_mode(config.mode)?;
    let sources = ProjectStore::open(session.database_path())?
        .list_sources(0, 1_000_000)?
        .into_iter()
        .filter(|source| source.kind == SourceKind::Video && source.source_group == source_group)
        .collect::<Vec<_>>();
    let mut estimated_count = 0_u64;
    for source in &sources {
        if let Ok(estimate) = estimate_sampling(session, &source.id, config, ffprobe) {
            estimated_count += estimate.estimated_count;
        }
    }
    Ok(GroupSamplingEstimate {
        source_count: sources.len() as u64,
        estimated_count,
    })
}

pub fn execute_group_sampling(
    session: &mut ProjectSession,
    source_group: &str,
    config: &SamplingConfig,
    ffprobe: &Path,
    ffmpeg: &Path,
) -> Result<SamplingExecutionResult, ApplicationError> {
    validate_group_sampling_mode(config.mode)?;
    let sources = ProjectStore::open(session.database_path())?
        .list_sources(0, 1_000_000)?
        .into_iter()
        .filter(|source| source.kind == SourceKind::Video && source.source_group == source_group)
        .collect::<Vec<_>>();
    let mut aggregate = SamplingExecutionResult {
        planned: 0,
        created: 0,
        existing: 0,
        failures: Vec::new(),
    };
    for source in sources {
        match execute_sampling(session, &source.id, config, ffprobe, ffmpeg) {
            Ok(result) => {
                aggregate.planned += result.planned;
                aggregate.created += result.created;
                aggregate.existing += result.existing;
                aggregate.failures.extend(result.failures);
            }
            Err(error) => aggregate.failures.push(ImportFailure {
                path: source.absolute_path,
                error: error.to_string(),
            }),
        }
    }
    session.refresh_summary()?;
    Ok(aggregate)
}

pub fn analyze_changes(
    session: &mut ProjectSession,
    source_id: &str,
    analysis_fps: f64,
    threshold: f64,
    min_interval_ms: u64,
    max_interval_ms: u64,
    ffmpeg: &Path,
) -> Result<ChangeAnalysisResult, ApplicationError> {
    if !(0.0..=1.0).contains(&threshold)
        || min_interval_ms == 0
        || max_interval_ms < min_interval_ms
    {
        return Err(ApplicationError::InvalidSamplingConfiguration);
    }
    let source = require_online_video(session, source_id)?;
    let width = source.width.ok_or(ApplicationError::MissingDimensions)?;
    let height = source.height.ok_or(ApplicationError::MissingDimensions)?;
    let points =
        media::analyze_video_changes(ffmpeg, &source.absolute_path, width, height, analysis_fps)?;
    let suggested_timestamps_ms =
        suggest_change_timestamps(&points, threshold, min_interval_ms, max_interval_ms);
    Ok(ChangeAnalysisResult {
        points,
        suggested_timestamps_ms,
    })
}

pub fn plan_sampling_times(
    frame_timestamps: &[u64],
    duration_ms: u64,
    selections: &[StoredVideoSelection],
    config: &SamplingConfig,
) -> Result<Vec<u64>, ApplicationError> {
    if frame_timestamps.is_empty() || duration_ms == 0 {
        return Err(ApplicationError::NoVideoFrames);
    }
    let requested = match config.mode {
        SamplingMode::FixedInterval => {
            if config.interval_ms == 0 {
                return Err(ApplicationError::InvalidSamplingConfiguration);
            }
            (0..duration_ms)
                .step_by(config.interval_ms as usize)
                .collect::<Vec<_>>()
        }
        SamplingMode::FrameInterval => {
            if config.frame_interval == 0 {
                return Err(ApplicationError::InvalidSamplingConfiguration);
            }
            frame_timestamps
                .iter()
                .step_by(config.frame_interval as usize)
                .copied()
                .collect::<Vec<_>>()
        }
        SamplingMode::TargetCount => {
            if config.target_count == 0 || config.target_count > 100_000 {
                return Err(ApplicationError::InvalidSamplingConfiguration);
            }
            if config.target_count == 1 {
                vec![duration_ms / 2]
            } else {
                (0..config.target_count)
                    .map(|index| index * duration_ms.saturating_sub(1) / (config.target_count - 1))
                    .collect()
            }
        }
        SamplingMode::ValidRanges => {
            if config.interval_ms == 0 {
                return Err(ApplicationError::InvalidSamplingConfiguration);
            }
            let selected = selections.iter().filter(|selection| {
                config.range_ids.is_empty() || config.range_ids.contains(&selection.id)
            });
            let mut values = Vec::new();
            for selection in selected {
                let mut timestamp = selection.start_ms;
                while timestamp < selection.end_ms {
                    values.push(timestamp);
                    timestamp = timestamp.saturating_add(config.interval_ms);
                }
            }
            values
        }
        SamplingMode::ChangeTriggered => config.custom_timestamps_ms.clone(),
    };
    let mut snapped = requested
        .into_iter()
        .filter_map(|timestamp| nearest_timestamp(frame_timestamps, timestamp))
        .collect::<Vec<_>>();
    snapped.sort_unstable();
    snapped.dedup();
    if snapped.len() > 100_000 {
        return Err(ApplicationError::SamplingPlanTooLarge);
    }
    Ok(snapped)
}

pub fn complete_pending_hashes(database_path: &Path) -> Result<u64, ApplicationError> {
    let store = ProjectStore::open(database_path)?;
    let assets = store.list_sources(0, 1_000_000)?;
    let mut completed = 0;
    for asset in assets
        .into_iter()
        .filter(|asset| asset.sha256.is_none() && asset.status == SourceStatus::Online)
    {
        let path = PathBuf::from(&asset.absolute_path);
        if path.is_file() {
            store.update_source_sha256(&asset.id, &full_hash(&path)?)?;
            completed += 1;
        }
    }
    Ok(completed)
}

pub fn refresh_source_statuses(session: &mut ProjectSession) -> Result<u64, ApplicationError> {
    let store = ProjectStore::open(session.database_path())?;
    let assets = store.list_sources(0, 1_000_000)?;
    let checked_at = Utc::now().to_rfc3339();
    let mut changed = 0;
    for asset in assets {
        let next = current_source_status(&asset);
        if next.0 != asset.status || next.1 != asset.error {
            changed += 1;
        }
        store.update_source_status(&asset.id, next.0, next.1.as_deref(), &checked_at)?;
    }
    session.refresh_summary()?;
    Ok(changed)
}

pub fn refresh_source_status(
    session: &mut ProjectSession,
    source_id: &str,
) -> Result<StoredSourceAsset, ApplicationError> {
    let store = ProjectStore::open(session.database_path())?;
    let asset = store
        .get_source(source_id)?
        .ok_or_else(|| ApplicationError::SourceNotFound(source_id.to_owned()))?;
    let next = current_source_status(&asset);
    store.update_source_status(
        source_id,
        next.0,
        next.1.as_deref(),
        &Utc::now().to_rfc3339(),
    )?;
    session.refresh_summary()?;
    store
        .get_source(source_id)?
        .ok_or_else(|| ApplicationError::SourceNotFound(source_id.to_owned()))
}

pub fn relink_source(
    session: &mut ProjectSession,
    source_id: &str,
    new_path: impl AsRef<Path>,
) -> Result<StoredSourceAsset, ApplicationError> {
    let store = ProjectStore::open(session.database_path())?;
    let mut asset = store
        .get_source(source_id)?
        .ok_or_else(|| ApplicationError::SourceNotFound(source_id.to_owned()))?;
    let new_path = new_path.as_ref().canonicalize()?;
    if SourceKind::from_path(&new_path) != Some(asset.kind) {
        return Err(ApplicationError::RelinkMismatch(
            "文件类型不一致".to_owned(),
        ));
    }
    if quick_fingerprint(&new_path)? != asset.quick_fingerprint {
        return Err(ApplicationError::RelinkMismatch(
            "快速内容指纹不一致，未更新引用".to_owned(),
        ));
    }
    let metadata = fs::metadata(&new_path)?;
    asset.absolute_path = path_text(&new_path);
    asset.file_name = new_path
        .file_name()
        .unwrap_or_default()
        .to_string_lossy()
        .into_owned();
    asset.size_bytes = metadata.len();
    asset.modified_unix_ms = modified_ms(&metadata)?;
    asset.status = SourceStatus::Online;
    asset.error = None;
    asset.last_checked_at = Utc::now().to_rfc3339();
    store.upsert_source(&asset)?;
    session.refresh_summary()?;
    store
        .get_source(source_id)?
        .ok_or_else(|| ApplicationError::SourceNotFound(source_id.to_owned()))
}

pub fn write_recent_project(
    config_file: &Path,
    project_path: &str,
) -> Result<(), ApplicationError> {
    if let Some(parent) = config_file.parent() {
        fs::create_dir_all(parent)?;
    }
    write_json_atomic(config_file, &serde_json::json!({ "path": project_path }))
}

pub fn read_recent_project(config_file: &Path) -> Result<Option<String>, ApplicationError> {
    if !config_file.is_file() {
        return Ok(None);
    }
    let value: serde_json::Value = serde_json::from_slice(&fs::read(config_file)?)?;
    Ok(value
        .get("path")
        .and_then(|path| path.as_str())
        .map(str::to_owned))
}

fn require_online_video(
    session: &mut ProjectSession,
    source_id: &str,
) -> Result<StoredSourceAsset, ApplicationError> {
    let source = refresh_source_status(session, source_id)?;
    if source.kind != SourceKind::Video {
        return Err(ApplicationError::SourceIsNotVideo);
    }
    if source.status != SourceStatus::Online {
        return Err(ApplicationError::SourceOffline(
            source.error.unwrap_or_else(|| "源文件不可访问".to_owned()),
        ));
    }
    Ok(source)
}

struct CandidateRequest<'a> {
    source: &'a StoredSourceAsset,
    frame_timestamps: &'a [u64],
    timestamp_ms: u64,
    selection_method: &'a str,
    parameters_json: &'a str,
    pinned: bool,
}

fn create_candidate(
    session: &ProjectSession,
    request: CandidateRequest<'_>,
    ffmpeg: &Path,
) -> Result<CaptureResult, ApplicationError> {
    let store = ProjectStore::open(session.database_path())?;
    if let Some(candidate) = store.get_candidate_at(&request.source.id, request.timestamp_ms)? {
        if request.pinned && !candidate.pinned {
            let mut updated = candidate;
            updated.pinned = true;
            updated.selection_method = request.selection_method.to_owned();
            updated.parameters_json = request.parameters_json.to_owned();
            store.upsert_candidate(&updated)?;
            return Ok(CaptureResult {
                candidate: updated,
                created: false,
            });
        }
        return Ok(CaptureResult {
            candidate,
            created: false,
        });
    }

    let id = Uuid::new_v4().to_string();
    let image_dir = session
        .project_dir()
        .join("cache")
        .join("candidates")
        .join(&request.source.id);
    let thumbnail_dir = session
        .project_dir()
        .join("cache")
        .join("candidate-thumbnails")
        .join(&request.source.id);
    fs::create_dir_all(&image_dir)?;
    fs::create_dir_all(&thumbnail_dir)?;
    let image_path = image_dir.join(format!("{id}.jpg"));
    let image_temporary = image_dir.join(format!("{id}.tmp.jpg"));
    let thumbnail_path = thumbnail_dir.join(format!("{id}.jpg"));
    let thumbnail_temporary = thumbnail_dir.join(format!("{id}.tmp.jpg"));

    if let Err(error) = media::extract_video_frame(
        ffmpeg,
        &request.source.absolute_path,
        request.timestamp_ms,
        &image_temporary,
    ) {
        let _ = fs::remove_file(&image_temporary);
        return Err(error.into());
    }
    fs::rename(&image_temporary, &image_path)?;
    if let Err(error) = media::create_image_thumbnail(&image_path, &thumbnail_temporary) {
        let _ = fs::remove_file(&thumbnail_temporary);
        let _ = fs::remove_file(&image_path);
        return Err(error.into());
    }
    fs::rename(&thumbnail_temporary, &thumbnail_path)?;
    let info = media::inspect_image(&image_path)?;
    let source_frame_number = request
        .frame_timestamps
        .binary_search(&request.timestamp_ms)
        .ok()
        .map(|index| index as u64);
    let candidate = StoredCandidateImage {
        id,
        source_id: request.source.id.clone(),
        video_offset_ms: request.timestamp_ms,
        source_frame_number,
        selection_method: request.selection_method.to_owned(),
        parameters_json: request.parameters_json.to_owned(),
        image_path: path_text(&image_path),
        thumbnail_path: path_text(&thumbnail_path),
        width: info.width,
        height: info.height,
        pinned: request.pinned,
        created_at: Utc::now().to_rfc3339(),
    };
    store.upsert_candidate(&candidate)?;
    Ok(CaptureResult {
        candidate,
        created: true,
    })
}

fn nearest_timestamp(timestamps: &[u64], requested: u64) -> Option<u64> {
    match timestamps.binary_search(&requested) {
        Ok(index) => timestamps.get(index).copied(),
        Err(0) => timestamps.first().copied(),
        Err(index) if index >= timestamps.len() => timestamps.last().copied(),
        Err(index) => {
            let before = timestamps[index - 1];
            let after = timestamps[index];
            if requested - before <= after - requested {
                Some(before)
            } else {
                Some(after)
            }
        }
    }
}

fn sampling_method_text(mode: SamplingMode) -> &'static str {
    match mode {
        SamplingMode::FixedInterval => "fixed_interval",
        SamplingMode::FrameInterval => "frame_interval",
        SamplingMode::TargetCount => "target_count",
        SamplingMode::ValidRanges => "valid_ranges",
        SamplingMode::ChangeTriggered => "change_triggered",
    }
}

fn validate_group_sampling_mode(mode: SamplingMode) -> Result<(), ApplicationError> {
    if matches!(
        mode,
        SamplingMode::ValidRanges | SamplingMode::ChangeTriggered
    ) {
        return Err(ApplicationError::GroupSamplingModeUnsupported);
    }
    Ok(())
}

fn suggest_change_timestamps(
    points: &[ChangePoint],
    threshold: f64,
    min_interval_ms: u64,
    max_interval_ms: u64,
) -> Vec<u64> {
    let Some(first) = points.first() else {
        return Vec::new();
    };
    let mut selected = vec![first.timestamp_ms];
    let mut last = first.timestamp_ms;
    for point in points.iter().skip(1) {
        let elapsed = point.timestamp_ms.saturating_sub(last);
        if elapsed >= max_interval_ms || (point.score >= threshold && elapsed >= min_interval_ms) {
            selected.push(point.timestamp_ms);
            last = point.timestamp_ms;
        }
    }
    selected
}

fn inspect_and_store(
    store: &ProjectStore,
    thumbnail_dir: &Path,
    root: &Path,
    path: &Path,
    kind: SourceKind,
    ffprobe: &Path,
    ffmpeg: &Path,
) -> Result<bool, ApplicationError> {
    let path = path.canonicalize()?;
    let metadata = fs::metadata(&path)?;
    if metadata.len() == 0 {
        return Err(ApplicationError::InvalidMedia("零字节文件".to_owned()));
    }
    let now = Utc::now().to_rfc3339();
    let absolute_path = path_text(&path);
    let fingerprint = quick_fingerprint(&path)?;
    let existing = match store.get_source_by_path(&absolute_path)? {
        Some(asset) => Some(asset),
        None => {
            let mut displaced = store
                .find_sources_by_fingerprint(kind, metadata.len(), &fingerprint)?
                .into_iter()
                .filter(|asset| current_source_status(asset).0 != SourceStatus::Online)
                .collect::<Vec<_>>();
            if displaced.len() == 1 {
                displaced.pop()
            } else {
                None
            }
        }
    };
    let id = existing
        .as_ref()
        .map(|asset| asset.id.clone())
        .unwrap_or_else(|| Uuid::new_v4().to_string());
    let thumbnail_path = existing
        .as_ref()
        .and_then(|asset| asset.thumbnail_path.as_ref())
        .map(PathBuf::from)
        .unwrap_or_else(|| thumbnail_dir.join(format!("{id}.jpg")));
    let relative_parent = path
        .parent()
        .and_then(|parent| parent.strip_prefix(root).ok())
        .unwrap_or_else(|| Path::new(""));
    let fallback_group = root
        .file_name()
        .map(|name| name.to_string_lossy().into_owned())
        .unwrap_or_else(|| "未分组".to_owned());
    let components = relative_parent
        .components()
        .map(|part| part.as_os_str().to_string_lossy().into_owned())
        .collect::<Vec<_>>();
    let source_group = components
        .first()
        .cloned()
        .unwrap_or_else(|| fallback_group.clone());
    let source_identifier = components.last().cloned().unwrap_or(fallback_group);
    let mut asset = StoredSourceAsset {
        id,
        absolute_path,
        file_name: path
            .file_name()
            .unwrap_or_default()
            .to_string_lossy()
            .into_owned(),
        relative_folder: path_text(relative_parent),
        source_group,
        source_identifier,
        kind,
        status: SourceStatus::Online,
        size_bytes: metadata.len(),
        modified_unix_ms: modified_ms(&metadata)?,
        quick_fingerprint: fingerprint,
        sha256: if metadata.len() <= FULL_HASH_LIMIT {
            Some(full_hash(&path)?)
        } else {
            None
        },
        width: None,
        height: None,
        duration_ms: None,
        codec: None,
        frame_rate: None,
        capture_time: None,
        capture_time_source: None,
        orientation: None,
        thumbnail_path: None,
        error: None,
        imported_at: existing
            .as_ref()
            .map(|asset| asset.imported_at.clone())
            .unwrap_or_else(|| now.clone()),
        last_checked_at: now,
    };
    match kind {
        SourceKind::Image => {
            let info = media::inspect_image(&path)?;
            asset.width = Some(info.width);
            asset.height = Some(info.height);
            asset.orientation = Some(info.orientation);
            asset.capture_time = info.capture_time;
            asset.capture_time_source = asset.capture_time.as_ref().map(|_| "exif".to_owned());
            media::create_image_thumbnail(&path, &thumbnail_path)?;
        }
        SourceKind::Video => {
            let document = media::probe_media(ffprobe, &path)?;
            let info = media::video_info(&document);
            asset.width = info.width;
            asset.height = info.height;
            asset.duration_ms = info.duration_ms;
            asset.codec = info.codec;
            asset.frame_rate = info.frame_rate;
            asset.capture_time = info.capture_time;
            asset.capture_time_source = asset.capture_time.as_ref().map(|_| "embedded".to_owned());
            media::create_video_thumbnail(ffmpeg, &path, &thumbnail_path)?;
        }
    }
    asset.thumbnail_path = Some(path_text(&thumbnail_path));
    Ok(store.upsert_source(&asset)?)
}

fn validate_project_name(name: &str) -> Result<(), ApplicationError> {
    let trimmed = name.trim();
    if trimmed.is_empty()
        || trimmed.ends_with(['.', ' '])
        || trimmed
            .chars()
            .any(|character| "<>:\"/\\|?*".contains(character))
    {
        return Err(ApplicationError::InvalidProjectName);
    }
    Ok(())
}

fn current_source_status(asset: &StoredSourceAsset) -> (SourceStatus, Option<String>) {
    let path = PathBuf::from(&asset.absolute_path);
    if !path.is_file() {
        return (
            SourceStatus::Offline,
            Some("源文件不可访问，可能已移动或改名".to_owned()),
        );
    }
    match quick_fingerprint(&path) {
        Ok(fingerprint) if fingerprint == asset.quick_fingerprint => (SourceStatus::Online, None),
        Ok(_) => (SourceStatus::Offline, Some("源文件内容已变化".to_owned())),
        Err(error) => (SourceStatus::Offline, Some(error.to_string())),
    }
}

fn quick_fingerprint(path: &Path) -> Result<String, ApplicationError> {
    const BLOCK: usize = 64 * 1024;
    let mut file = File::open(path)?;
    let size = file.metadata()?.len();
    let mut hasher = Sha256::new();
    hasher.update(size.to_le_bytes());
    let mut buffer = vec![0_u8; BLOCK];
    let read = file.read(&mut buffer)?;
    hasher.update(&buffer[..read]);
    if size > BLOCK as u64 {
        file.seek(SeekFrom::End(-(BLOCK.min(size as usize) as i64)))?;
        let read = file.read(&mut buffer)?;
        hasher.update(&buffer[..read]);
    }
    Ok(hex::encode(hasher.finalize()))
}

fn full_hash(path: &Path) -> Result<String, ApplicationError> {
    let mut file = File::open(path)?;
    let mut hasher = Sha256::new();
    let mut buffer = vec![0_u8; 1024 * 1024];
    loop {
        let read = file.read(&mut buffer)?;
        if read == 0 {
            break;
        }
        hasher.update(&buffer[..read]);
    }
    Ok(hex::encode(hasher.finalize()))
}

fn modified_ms(metadata: &fs::Metadata) -> Result<i64, ApplicationError> {
    Ok(metadata
        .modified()?
        .duration_since(UNIX_EPOCH)
        .map_err(|_| ApplicationError::InvalidModifiedTime)?
        .as_millis() as i64)
}

fn path_text(path: &Path) -> String {
    path.to_string_lossy().into_owned()
}

fn write_json_atomic(path: &Path, value: &impl Serialize) -> Result<(), ApplicationError> {
    let temporary = path.with_extension("tmp");
    fs::write(&temporary, serde_json::to_vec_pretty(value)?)?;
    fs::rename(temporary, path)?;
    Ok(())
}

#[derive(Debug, Error)]
pub enum ApplicationError {
    #[error("文件操作失败：{0}")]
    Io(#[from] std::io::Error),
    #[error("项目数据库失败：{0}")]
    Storage(#[from] storage::StorageError),
    #[error("媒体解析失败：{0}")]
    Media(#[from] media::MediaError),
    #[error("项目清单解析失败：{0}")]
    Json(#[from] serde_json::Error),
    #[error("项目名称为空或包含 Windows 不允许的字符")]
    InvalidProjectName,
    #[error("项目目录已存在：{0}")]
    ProjectExists(PathBuf),
    #[error("不是有效的 Free-Train 项目：{0}")]
    InvalidProject(PathBuf),
    #[error("项目正被另一个 Free-Train 窗口使用：{0}")]
    ProjectLocked(PathBuf),
    #[error("素材无效：{0}")]
    InvalidMedia(String),
    #[error("找不到源素材记录：{0}")]
    SourceNotFound(String),
    #[error("无法重新定位：{0}")]
    RelinkMismatch(String),
    #[error("文件修改时间早于 Unix epoch")]
    InvalidModifiedTime,
    #[error("视频范围无效：{0}")]
    Domain(#[from] domain::DomainError),
    #[error("源素材不是视频")]
    SourceIsNotVideo,
    #[error("源素材离线：{0}")]
    SourceOffline(String),
    #[error("视频没有可用帧时间戳")]
    NoVideoFrames,
    #[error("视频缺少时长元数据")]
    MissingDuration,
    #[error("视频缺少尺寸元数据")]
    MissingDimensions,
    #[error("抽帧参数无效")]
    InvalidSamplingConfiguration,
    #[error("抽帧计划超过 100,000 个候选")]
    SamplingPlanTooLarge,
    #[error("来源组批量抽帧仅支持固定时间、固定帧间隔和目标数量模式")]
    GroupSamplingModeUnsupported,
}

#[cfg(test)]
mod project_tests {
    use super::*;
    use image::{ImageBuffer, Rgb};

    fn test_root(label: &str) -> PathBuf {
        std::env::temp_dir().join(format!("free-train-{label}-{}", Uuid::new_v4()))
    }

    #[test]
    fn creates_imports_reopens_and_detects_offline_unicode_source() {
        let root = test_root("m1-unicode");
        let source_dir = root.join("中文 素材").join("cam1_01");
        fs::create_dir_all(&source_dir).unwrap();
        let source = source_dir.join("测试 图片.png");
        ImageBuffer::from_fn(96, 64, |x, y| Rgb([(x % 255) as u8, (y % 255) as u8, 80]))
            .save(&source)
            .unwrap();

        let mut session = ProjectSession::create(&root, "中文 项目").unwrap();
        let project_path = session.summary.path.clone();
        let result = import_sources(
            &mut session,
            &[path_text(&root.join("中文 素材"))],
            Path::new("ffprobe"),
            Path::new("ffmpeg"),
        )
        .unwrap();
        assert_eq!(result.imported, 1);
        let assets = list_sources(&session, 0, 10).unwrap();
        assert_eq!(assets[0].source_group, "cam1_01");
        assert_eq!(assets[0].width, Some(96));
        assert!(assets[0].sha256.is_some());

        let second = import_sources(
            &mut session,
            &[path_text(&root.join("中文 素材"))],
            Path::new("ffprobe"),
            Path::new("ffmpeg"),
        )
        .unwrap();
        assert_eq!(second.imported, 0);
        assert_eq!(second.updated, 1);
        assert_eq!(list_sources(&session, 0, 10).unwrap().len(), 1);
        assert_eq!(
            fs::read_dir(session.project_dir().join("cache").join("thumbnails"))
                .unwrap()
                .count(),
            1
        );

        assert!(matches!(
            ProjectSession::open(&project_path),
            Err(ApplicationError::ProjectLocked(_))
        ));
        drop(session);
        let mut reopened = ProjectSession::open(&project_path).unwrap();
        let moved = source.with_file_name("已移动.png");
        fs::rename(&source, &moved).unwrap();
        let offline = refresh_source_status(&mut reopened, &assets[0].id).unwrap();
        assert_eq!(offline.status, SourceStatus::Offline);
        let relinked = relink_source(&mut reopened, &offline.id, &moved).unwrap();
        assert_eq!(relinked.status, SourceStatus::Online);
        assert_eq!(relinked.file_name, "已移动.png");
        drop(reopened);
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn reimporting_a_uniquely_matching_renamed_file_updates_the_existing_asset() {
        let root = test_root("m1-rename");
        let source_dir = root.join("rename-source").join("cam2_01");
        fs::create_dir_all(&source_dir).unwrap();
        let original = source_dir.join("before.png");
        ImageBuffer::from_pixel(48, 32, Rgb([90_u8, 30, 160]))
            .save(&original)
            .unwrap();
        let mut session = ProjectSession::create(&root, "rename-project").unwrap();
        import_sources(
            &mut session,
            &[path_text(&root.join("rename-source"))],
            Path::new("ffprobe"),
            Path::new("ffmpeg"),
        )
        .unwrap();
        let original_asset = list_sources(&session, 0, 10).unwrap().remove(0);

        let renamed = source_dir.join("after.png");
        fs::rename(&original, &renamed).unwrap();
        let result = import_sources(
            &mut session,
            &[path_text(&root.join("rename-source"))],
            Path::new("ffprobe"),
            Path::new("ffmpeg"),
        )
        .unwrap();

        assert_eq!(result.imported, 0);
        assert_eq!(result.updated, 1);
        let assets = list_sources(&session, 0, 10).unwrap();
        assert_eq!(assets.len(), 1);
        assert_eq!(assets[0].id, original_asset.id);
        assert_eq!(assets[0].file_name, "after.png");
        assert_eq!(assets[0].source_identifier, "cam2_01");
        drop(session);
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn identical_online_sources_are_not_merged_as_a_rename() {
        let root = test_root("m1-identical-sources");
        let source_a = root.join("cam_a");
        let source_b = root.join("cam_b");
        fs::create_dir_all(&source_a).unwrap();
        fs::create_dir_all(&source_b).unwrap();
        let image = ImageBuffer::from_pixel(32, 24, Rgb([12_u8, 34, 56]));
        image.save(source_a.join("frame.png")).unwrap();
        image.save(source_b.join("frame.png")).unwrap();

        let mut session = ProjectSession::create(&root, "identical-project").unwrap();
        let result = import_sources(
            &mut session,
            &[path_text(&source_a), path_text(&source_b)],
            Path::new("ffprobe"),
            Path::new("ffmpeg"),
        )
        .unwrap();
        assert_eq!(result.imported, 2);
        let assets = list_sources(&session, 0, 10).unwrap();
        assert_eq!(assets.len(), 2);
        assert_ne!(assets[0].id, assets[1].id);
        drop(session);
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn imports_video_metadata_and_thumbnail_when_ffmpeg_is_available() {
        let ffprobe = PathBuf::from(r"D:\ffmpeg\bin\ffprobe.exe");
        let ffmpeg = PathBuf::from(r"D:\ffmpeg\bin\ffmpeg.exe");
        let fixture = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("..")
            .join("..")
            .join("tests")
            .join("fixtures")
            .join("m0-sample.mp4");
        if !ffprobe.is_file() || !ffmpeg.is_file() || !fixture.is_file() {
            return;
        }
        let root = test_root("m1-video");
        fs::create_dir_all(&root).unwrap();
        let mut session = ProjectSession::create(&root, "video-project").unwrap();
        let project_path = session.summary.path.clone();
        let result =
            import_sources(&mut session, &[path_text(&fixture)], &ffprobe, &ffmpeg).unwrap();
        assert_eq!(result.imported, 1);
        let asset = list_sources(&session, 0, 10).unwrap().remove(0);
        assert_eq!(asset.kind, SourceKind::Video);
        assert!(asset.width.is_some_and(|width| width > 0));
        assert!(asset.duration_ms.is_some_and(|duration| duration > 0));
        assert!(
            asset
                .thumbnail_path
                .as_ref()
                .is_some_and(|path| Path::new(path).is_file())
        );
        let timestamps = video_frame_timestamps(&mut session, &asset.id, &ffprobe).unwrap();
        assert_eq!(timestamps.len(), 60);
        let manual =
            capture_manual_frame(&mut session, &asset.id, 1_000, &ffprobe, &ffmpeg).unwrap();
        assert!(manual.created);
        assert!(manual.candidate.pinned);
        let duplicate =
            capture_manual_frame(&mut session, &asset.id, 1_000, &ffprobe, &ffmpeg).unwrap();
        assert!(!duplicate.created);
        create_video_selection(&mut session, &asset.id, 300, 900, false).unwrap();
        let config = SamplingConfig {
            mode: SamplingMode::FixedInterval,
            interval_ms: 500,
            frame_interval: 1,
            target_count: 1,
            range_ids: Vec::new(),
            custom_timestamps_ms: Vec::new(),
            pin_results: false,
        };
        let estimate = estimate_sampling(&mut session, &asset.id, &config, &ffprobe).unwrap();
        assert_eq!(estimate.timestamps_ms, vec![0, 500, 1_000, 1_500]);
        let execution =
            execute_sampling(&mut session, &asset.id, &config, &ffprobe, &ffmpeg).unwrap();
        assert_eq!(execution.created, 3);
        assert_eq!(execution.existing, 1);
        assert_eq!(
            list_candidates(&session, &asset.id, 0, 10).unwrap().len(),
            4
        );
        let changes =
            analyze_changes(&mut session, &asset.id, 2.0, 0.01, 250, 1_000, &ffmpeg).unwrap();
        assert!(!changes.points.is_empty());
        assert!(!changes.suggested_timestamps_ms.is_empty());

        let batch_dir = root.join("batch").join("cam_group");
        fs::create_dir_all(&batch_dir).unwrap();
        fs::copy(&fixture, batch_dir.join("a.mp4")).unwrap();
        fs::copy(&fixture, batch_dir.join("b.mp4")).unwrap();
        import_sources(
            &mut session,
            &[path_text(&root.join("batch"))],
            &ffprobe,
            &ffmpeg,
        )
        .unwrap();
        let group_config = SamplingConfig {
            mode: SamplingMode::TargetCount,
            interval_ms: 1_000,
            frame_interval: 1,
            target_count: 1,
            range_ids: Vec::new(),
            custom_timestamps_ms: Vec::new(),
            pin_results: false,
        };
        let group_estimate =
            estimate_group_sampling(&mut session, "cam_group", &group_config, &ffprobe).unwrap();
        assert_eq!(group_estimate.source_count, 2);
        assert_eq!(group_estimate.estimated_count, 2);
        let group_result =
            execute_group_sampling(&mut session, "cam_group", &group_config, &ffprobe, &ffmpeg)
                .unwrap();
        assert_eq!(group_result.created, 2);
        drop(session);
        let reopened = ProjectSession::open(&project_path).unwrap();
        assert_eq!(
            list_candidates(&reopened, &asset.id, 0, 10).unwrap().len(),
            4
        );
        assert_eq!(
            list_video_selections(&reopened, &asset.id).unwrap().len(),
            1
        );
        assert_eq!(reopened.summary.candidate_count, 6);
        drop(reopened);
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn corrupt_media_does_not_block_other_files_in_the_batch() {
        let root = test_root("m1-errors");
        let source_dir = root.join("mixed");
        fs::create_dir_all(&source_dir).unwrap();
        ImageBuffer::from_pixel(40, 30, Rgb([20_u8, 80, 120]))
            .save(source_dir.join("valid.png"))
            .unwrap();
        fs::write(source_dir.join("corrupt.jpg"), b"not-a-jpeg").unwrap();
        fs::write(source_dir.join("notes.txt"), b"unsupported").unwrap();

        let mut session = ProjectSession::create(&root, "error-isolation").unwrap();
        let result = import_sources(
            &mut session,
            &[path_text(&source_dir)],
            Path::new("ffprobe"),
            Path::new("ffmpeg"),
        )
        .unwrap();
        assert_eq!(result.imported, 1);
        assert_eq!(result.unsupported, 1);
        assert_eq!(result.failures.len(), 1);
        assert_eq!(list_sources(&session, 0, 10).unwrap().len(), 1);
        drop(session);
        fs::remove_dir_all(root).unwrap();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn warning_components_do_not_block_m0() {
        let status = M0Status {
            app_version: "0.1.0".into(),
            target: "windows-x86_64".into(),
            components: vec![ComponentHealth::warning("ffmpeg", "FFmpeg", "full build")],
        };
        assert!(status.is_ready());
    }

    #[test]
    fn target_count_plan_snaps_to_real_frame_timestamps() {
        let frames = vec![0, 40, 80, 120, 160, 200];
        let config = SamplingConfig {
            mode: SamplingMode::TargetCount,
            interval_ms: 1_000,
            frame_interval: 1,
            target_count: 3,
            range_ids: Vec::new(),
            custom_timestamps_ms: Vec::new(),
            pin_results: false,
        };
        let planned = plan_sampling_times(&frames, 201, &[], &config).unwrap();
        assert_eq!(planned, vec![0, 80, 200]);
    }
}
