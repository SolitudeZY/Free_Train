use std::{
    collections::{HashMap, HashSet},
    fs::{self, File, OpenOptions},
    io::{Read, Seek, SeekFrom, Write},
    path::{Path, PathBuf},
    time::UNIX_EPOCH,
};

use chrono::Utc;
use domain::{Roi, SamplingMode, SourceKind, SourceStatus, VideoRange};
use fs2::FileExt;
use image_pipeline::{
    ConflictStrategy, ExportFormat, QualityMetrics, TilePlacement, TileRenderConfig, encode_image,
    global_ssim, hamming_distance, measure_quality, perceptual_hash, render_tile, render_tiles,
    resolve_file_name,
};
use media::ChangePoint;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use storage::{
    ProjectStore, StoredCandidateImage, StoredQualityAssessment, StoredReviewAsset,
    StoredReviewAuditEvent, StoredRoiProfile, StoredSourceAsset, StoredVideoSelection,
};
use thiserror::Error;
use uuid::Uuid;
use walkdir::WalkDir;

mod export;
mod review;
mod roi;
mod sampling;
mod video_preview;

pub use export::{plan_export, run_export, run_export_with_progress};
pub use review::{
    list_review_workspace, list_review_workspace_at, redo_review_action, run_review_analysis,
    run_review_analysis_at, undo_review_action, update_review_items,
};
pub use roi::{
    delete_roi_profile, list_effective_roi_profiles, preview_source_tiles, save_roi_profile,
};
pub use sampling::{
    analyze_changes, capture_manual_frame, create_video_selection, delete_candidates,
    delete_video_selection, estimate_group_sampling, estimate_sampling, execute_group_sampling,
    execute_group_sampling_with_progress, execute_sampling, execute_sampling_with_progress,
    list_candidates, list_video_selections, plan_sampling_times, video_frame_timestamps,
};
pub use video_preview::{
    VideoPreview, VideoPreviewPlan, execute_video_preview, plan_video_preview,
};

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
    pub existing_count: u64,
    pub estimated_new_count: u64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct GroupSamplingEstimate {
    pub source_count: u64,
    pub estimated_count: u64,
    pub existing_count: u64,
    pub estimated_new_count: u64,
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
pub struct OperationProgress {
    pub phase: String,
    pub completed: u64,
    pub total: u64,
    pub succeeded: u64,
    pub existing: u64,
    pub failed: u64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ImportProgress {
    pub phase: String,
    pub completed: u64,
    pub total: u64,
    pub imported: u64,
    pub updated: u64,
    pub unsupported: u64,
    pub failed: u64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CandidateDeletionResult {
    pub deleted: u64,
    pub failures: Vec<ImportFailure>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SourceDeletionResult {
    pub deleted: u64,
    pub candidate_deleted: u64,
    pub failures: Vec<ImportFailure>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SourceDeletionProgress {
    pub completed: u64,
    pub total: u64,
    pub deleted: u64,
    pub candidate_deleted: u64,
}

const QUALITY_ALGORITHM_VERSION: &str = "quality-v1";
const SIMILARITY_ALGORITHM_VERSION: &str = "similarity-v1";

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SimilarityScope {
    Source,
    SourceGroup,
    Project,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ReviewAnalysisConfig {
    pub min_width: u32,
    pub min_height: u32,
    pub min_sharpness: f64,
    pub max_underexposed_ratio: f64,
    pub max_overexposed_ratio: f64,
    pub max_low_information: f64,
    pub phash_distance: u32,
    pub ssim_threshold: f64,
    pub similarity_scope: SimilarityScope,
    pub video_time_window_ms: u64,
}

impl Default for ReviewAnalysisConfig {
    fn default() -> Self {
        Self {
            min_width: 320,
            min_height: 240,
            min_sharpness: 80.0,
            max_underexposed_ratio: 0.35,
            max_overexposed_ratio: 0.35,
            max_low_information: 0.72,
            phash_distance: 8,
            ssim_threshold: 0.94,
            similarity_scope: SimilarityScope::Source,
            video_time_window_ms: 30_000,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ReviewItem {
    pub asset_key: String,
    pub source_id: String,
    pub candidate_id: Option<String>,
    pub source_group: String,
    pub source_identifier: String,
    pub display_name: String,
    pub image_path: String,
    pub thumbnail_path: String,
    pub video_offset_ms: Option<u64>,
    pub selection_method: String,
    pub pinned: bool,
    pub metrics: Option<QualityMetrics>,
    pub automatic_status: String,
    pub automatic_reasons: Vec<String>,
    pub manual_decision: Option<String>,
    pub locked: bool,
    pub similarity_group_id: Option<String>,
    pub similarity_score: Option<f64>,
    pub representative: bool,
    pub locked_conflict: bool,
    pub decode_error: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ReviewSummary {
    pub total: u64,
    pub keep: u64,
    pub suggested_exclude: u64,
    pub manually_excluded: u64,
    pub warning: u64,
    pub failed: u64,
    pub locked: u64,
    pub similarity_groups: u64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ReviewWorkspace {
    pub items: Vec<ReviewItem>,
    pub summary: ReviewSummary,
    pub can_undo: bool,
    pub can_redo: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ReviewAction {
    Keep,
    Exclude,
    Restore,
    Lock,
    Unlock,
    MakeRepresentative,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ChangeAnalysisResult {
    pub points: Vec<ChangePoint>,
    pub suggested_timestamps_ms: Vec<u64>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RoiScope {
    SourceGroup,
    Source,
}

impl RoiScope {
    fn as_str(self) -> &'static str {
        match self {
            Self::SourceGroup => "source_group",
            Self::Source => "source",
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RoiProfile {
    pub id: String,
    pub scope: RoiScope,
    pub scope_value: String,
    pub name: String,
    pub roi: Roi,
    pub render_config: TileRenderConfig,
    pub inherited: bool,
    pub updated_at: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SaveRoiProfile {
    pub id: Option<String>,
    pub scope: RoiScope,
    pub scope_value: String,
    pub name: String,
    pub roi: Roi,
    pub render_config: TileRenderConfig,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct TilePreview {
    pub source_id: String,
    pub candidate_id: Option<String>,
    pub roi_profile_id: String,
    pub roi_name: String,
    pub placement: TilePlacement,
    pub preview_path: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ExportRequest {
    pub source_id: String,
    #[serde(default)]
    pub source_scope: ExportSourceScope,
    pub candidate_id: Option<String>,
    pub output_dir: String,
    pub naming_template: String,
    pub format: ExportFormat,
    pub conflict_strategy: ConflictStrategy,
    #[serde(default)]
    pub content: ExportContent,
    #[serde(default)]
    pub review_scope: ExportReviewScope,
    #[serde(default)]
    pub excluded_tiles: Vec<ExcludedTile>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ExcludedTile {
    pub source_id: String,
    pub candidate_id: Option<String>,
    pub roi_profile_id: String,
    pub row: u32,
    pub column: u32,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ExportContent {
    Frames,
    #[default]
    Tiles,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ExportReviewScope {
    #[default]
    Eligible,
    All,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ExportSourceScope {
    #[default]
    Current,
    SourceGroup,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ExportPlanItem {
    pub source_id: String,
    pub candidate_id: Option<String>,
    pub content: ExportContent,
    pub roi_profile_id: Option<String>,
    pub roi_name: Option<String>,
    pub placement: TilePlacement,
    pub file_name: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ExportPlan {
    pub output_dir: String,
    pub items: Vec<ExportPlanItem>,
    pub skipped: u64,
    pub estimated_bytes: u64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ExportResult {
    pub export_id: String,
    pub written: u64,
    pub skipped: u64,
    pub manifest_path: String,
    pub failures: Vec<ImportFailure>,
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
    import_sources_with_progress(session, inputs, ffprobe, ffmpeg, |_| {})
}

pub fn import_sources_with_progress<F>(
    session: &mut ProjectSession,
    inputs: &[String],
    ffprobe: &Path,
    ffmpeg: &Path,
    mut report_progress: F,
) -> Result<ImportResult, ApplicationError>
where
    F: FnMut(ImportProgress),
{
    let store = ProjectStore::open(session.database_path())?;
    let thumbnail_dir = session.project_dir.join("cache").join("thumbnails");
    let mut result = ImportResult {
        discovered: 0,
        imported: 0,
        updated: 0,
        unsupported: 0,
        failures: Vec::new(),
    };
    let mut work = Vec::<(PathBuf, PathBuf)>::new();

    for (input_index, input) in inputs.iter().enumerate() {
        report_progress(ImportProgress {
            phase: "正在扫描导入路径".to_owned(),
            completed: input_index as u64,
            total: inputs.len() as u64,
            imported: 0,
            updated: 0,
            unsupported: 0,
            failed: result.failures.len() as u64,
        });
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
        work.extend(candidates.into_iter().map(|path| (root.clone(), path)));
    }
    let total = work.len() as u64;
    report_progress(ImportProgress {
        phase: "正在读取素材并生成缩略图".to_owned(),
        completed: 0,
        total,
        imported: 0,
        updated: 0,
        unsupported: 0,
        failed: result.failures.len() as u64,
    });
    for (index, (root, path)) in work.into_iter().enumerate() {
        result.discovered += 1;
        if let Some(kind) = SourceKind::from_path(&path) {
            match inspect_and_store(&store, &thumbnail_dir, &root, &path, kind, ffprobe, ffmpeg) {
                Ok(true) => result.updated += 1,
                Ok(false) => result.imported += 1,
                Err(error) => result.failures.push(ImportFailure {
                    path: path_text(&path),
                    error: error.to_string(),
                }),
            }
        } else {
            result.unsupported += 1;
        }
        report_progress(ImportProgress {
            phase: "正在读取素材并生成缩略图".to_owned(),
            completed: index as u64 + 1,
            total,
            imported: result.imported,
            updated: result.updated,
            unsupported: result.unsupported,
            failed: result.failures.len() as u64,
        });
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

pub fn list_all_source_ids(session: &ProjectSession) -> Result<Vec<String>, ApplicationError> {
    Ok(ProjectStore::open(session.database_path())?.list_source_ids()?)
}

pub fn delete_sources(
    session: &mut ProjectSession,
    source_ids: &[String],
) -> Result<SourceDeletionResult, ApplicationError> {
    delete_sources_with_progress(session, source_ids, |_| {})
}

pub fn delete_sources_with_progress<F>(
    session: &mut ProjectSession,
    source_ids: &[String],
    mut report_progress: F,
) -> Result<SourceDeletionResult, ApplicationError>
where
    F: FnMut(SourceDeletionProgress),
{
    let store = ProjectStore::open(session.database_path())?;
    let cache_root = session.project_dir().join("cache").canonicalize()?;
    let mut seen = HashSet::new();
    let targets = source_ids
        .iter()
        .filter(|source_id| seen.insert(source_id.as_str()))
        .collect::<Vec<_>>();
    let mut result = SourceDeletionResult {
        deleted: 0,
        candidate_deleted: 0,
        failures: Vec::new(),
    };
    let total = targets.len() as u64;
    report_progress(SourceDeletionProgress {
        completed: 0,
        total,
        deleted: 0,
        candidate_deleted: 0,
    });
    for (position, source_id) in targets.into_iter().enumerate() {
        let Some(source) = store.get_source(source_id)? else {
            report_progress(SourceDeletionProgress {
                completed: position as u64 + 1,
                total,
                deleted: result.deleted,
                candidate_deleted: result.candidate_deleted,
            });
            continue;
        };
        let candidates = store.delete_candidates(source_id, None)?;
        result.candidate_deleted += candidates.len() as u64;
        remove_candidate_cache_files(&cache_root, candidates, &mut result.failures);
        if store.delete_source(source_id)? {
            result.deleted += 1;
            if let Some(thumbnail_path) = source.thumbnail_path {
                remove_cache_file(
                    &cache_root,
                    Path::new(&thumbnail_path),
                    &mut result.failures,
                );
            }
            for file_name in [
                format!("{}.mp4", source.id),
                format!("{}.partial.mp4", source.id),
                format!("{}-webview-v1.mp4", source.id),
                format!("{}-webview-v1.partial.mp4", source.id),
            ] {
                remove_cache_file(
                    &cache_root,
                    &session
                        .project_dir()
                        .join("cache")
                        .join("video-previews")
                        .join(file_name),
                    &mut result.failures,
                );
            }
        }
        report_progress(SourceDeletionProgress {
            completed: position as u64 + 1,
            total,
            deleted: result.deleted,
            candidate_deleted: result.candidate_deleted,
        });
    }
    session.refresh_summary()?;
    Ok(result)
}

fn remove_candidate_cache_files(
    cache_root: &Path,
    candidates: Vec<StoredCandidateImage>,
    failures: &mut Vec<ImportFailure>,
) {
    for candidate in candidates {
        remove_cache_file(cache_root, Path::new(&candidate.image_path), failures);
        remove_cache_file(cache_root, Path::new(&candidate.thumbnail_path), failures);
    }
}

fn remove_cache_file(cache_root: &Path, path: &Path, failures: &mut Vec<ImportFailure>) {
    if !path.exists() {
        return;
    }
    match path.canonicalize() {
        Ok(canonical) if canonical.starts_with(cache_root) => {
            if let Err(error) = fs::remove_file(&canonical) {
                failures.push(ImportFailure {
                    path: path_text(&canonical),
                    error: error.to_string(),
                });
            }
        }
        Ok(canonical) => failures.push(ImportFailure {
            path: path_text(&canonical),
            error: "缓存路径超出项目 cache 边界，已跳过删除".to_owned(),
        }),
        Err(error) => failures.push(ImportFailure {
            path: path_text(path),
            error: error.to_string(),
        }),
    }
}

pub fn complete_pending_hashes(database_path: &Path) -> Result<u64, ApplicationError> {
    let store = ProjectStore::open(database_path)?;
    let assets = store.list_sources(0, 1_000_000)?;
    let mut completed = 0;
    for asset in assets.into_iter().filter(|asset| {
        asset.kind == SourceKind::Image
            && asset.sha256.is_none()
            && asset.status == SourceStatus::Online
    }) {
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
        sha256: existing.as_ref().and_then(|asset| asset.sha256.clone()),
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

#[derive(Debug, Clone)]
struct ProcessingInput {
    candidate_id: Option<String>,
    candidate: Option<StoredCandidateImage>,
    path: PathBuf,
    width: u32,
    height: u32,
}

fn resolve_one_input(
    store: &ProjectStore,
    source: &StoredSourceAsset,
    candidate_id: Option<&str>,
) -> Result<ProcessingInput, ApplicationError> {
    if source.status != SourceStatus::Online {
        return Err(ApplicationError::SourceOffline(
            source.absolute_path.clone(),
        ));
    }
    match (source.kind, candidate_id) {
        (SourceKind::Image, None) => {
            let path = PathBuf::from(&source.absolute_path);
            if !path.is_file() {
                return Err(ApplicationError::SourceOffline(
                    source.absolute_path.clone(),
                ));
            }
            Ok(ProcessingInput {
                candidate_id: None,
                candidate: None,
                path,
                width: source.width.ok_or(ApplicationError::MissingDimensions)?,
                height: source.height.ok_or(ApplicationError::MissingDimensions)?,
            })
        }
        (SourceKind::Image, Some(_)) => Err(ApplicationError::CandidateSourceMismatch),
        (SourceKind::Video, Some(candidate_id)) => {
            let candidate = store
                .get_candidate(candidate_id)?
                .ok_or_else(|| ApplicationError::CandidateNotFound(candidate_id.to_owned()))?;
            processing_input_from_candidate(source, candidate)
        }
        (SourceKind::Video, None) => Err(ApplicationError::NoCandidateImages),
    }
}

fn processing_input_from_candidate(
    source: &StoredSourceAsset,
    candidate: StoredCandidateImage,
) -> Result<ProcessingInput, ApplicationError> {
    if candidate.source_id != source.id {
        return Err(ApplicationError::CandidateSourceMismatch);
    }
    let path = PathBuf::from(&candidate.image_path);
    if !path.is_file() {
        return Err(ApplicationError::SourceOffline(candidate.image_path));
    }
    Ok(ProcessingInput {
        candidate_id: Some(candidate.id.clone()),
        width: candidate.width,
        height: candidate.height,
        path,
        candidate: Some(candidate),
    })
}

fn write_bytes_atomic(path: &Path, bytes: &[u8]) -> Result<(), ApplicationError> {
    let file_name = path
        .file_name()
        .ok_or_else(|| ApplicationError::InvalidExportDirectory(path.to_path_buf()))?
        .to_string_lossy();
    let temporary = path.with_file_name(format!(".{file_name}.{}.tmp", Uuid::new_v4()));
    let mut file = OpenOptions::new()
        .create_new(true)
        .write(true)
        .open(&temporary)?;
    file.write_all(bytes)?;
    file.sync_all()?;
    match fs::rename(&temporary, path) {
        Ok(()) => Ok(()),
        Err(error) => {
            let _ = fs::remove_file(&temporary);
            Err(ApplicationError::Io(error))
        }
    }
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
    #[error("图片处理失败：{0}")]
    Pipeline(#[from] image_pipeline::PipelineError),
    #[error("图片解码失败：{0}")]
    Image(#[from] image::ImageError),
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
    #[error("视频缺少有效帧率元数据，无法按帧间隔抽帧")]
    MissingFrameRate,
    #[error("视频缺少尺寸元数据")]
    MissingDimensions,
    #[error("抽帧参数无效")]
    InvalidSamplingConfiguration,
    #[error("抽帧计划超过 100,000 个候选")]
    SamplingPlanTooLarge,
    #[error("来源组批量抽帧仅支持固定时间、固定帧间隔和目标数量模式")]
    GroupSamplingModeUnsupported,
    #[error("ROI 名称、作用域或范围无效")]
    InvalidRoiProfile,
    #[error("当前来源没有可用 ROI")]
    NoRoiProfiles,
    #[error("当前视频没有可用候选图片")]
    NoCandidateImages,
    #[error("当前项目没有可审核的在线图片或视频候选")]
    NoReviewAssets,
    #[error("质量或相似分析参数无效")]
    InvalidReviewConfiguration,
    #[error("相似比较范围过大，请缩小到来源或来源组")]
    SimilarityComparisonTooLarge,
    #[error("所选审核项不属于相似组")]
    ReviewAssetHasNoSimilarityGroup,
    #[error("没有可撤销的审核操作")]
    NoReviewUndoAvailable,
    #[error("没有可重做的审核操作")]
    NoReviewRedoAvailable,
    #[error("找不到候选图片：{0}")]
    CandidateNotFound(String),
    #[error("候选图片不属于当前源素材")]
    CandidateSourceMismatch,
    #[error("命名模板无效：{0}")]
    InvalidNamingTemplate(String),
    #[error("导出目录无效：{0}")]
    InvalidExportDirectory(PathBuf),
    #[error("导出目标已存在：{0}")]
    ExportConflict(PathBuf),
}

#[cfg(test)]
mod project_tests {
    use super::*;
    use domain::{EdgeStrategy, TileConfig};
    use image::{ImageBuffer, Rgb};
    use image_pipeline::{PaddingMode, ResizeMode};

    fn test_root(label: &str) -> PathBuf {
        std::env::temp_dir().join(format!("free-train-{label}-{}", Uuid::new_v4()))
    }

    #[test]
    fn review_analysis_preserves_manual_state_and_export_skips_manual_exclusions() {
        let root = test_root("m4-review");
        let source_dir = root.join("camera-a");
        fs::create_dir_all(&source_dir).unwrap();
        let first_path = source_dir.join("first.png");
        let second_path = source_dir.join("second.png");
        let image = ImageBuffer::from_fn(96, 64, |x, y| {
            Rgb([(x * 2) as u8, (y * 3) as u8, ((x + y) * 2) as u8])
        });
        image.save(&first_path).unwrap();
        image.save(&second_path).unwrap();
        let first_hash = full_hash(&first_path).unwrap();
        let second_hash = full_hash(&second_path).unwrap();
        let mut session = ProjectSession::create(&root, "m4-project").unwrap();
        import_sources(
            &mut session,
            &[path_text(&source_dir)],
            Path::new("ffprobe"),
            Path::new("ffmpeg"),
        )
        .unwrap();
        let config = ReviewAnalysisConfig {
            min_width: 1,
            min_height: 1,
            min_sharpness: 0.0,
            max_underexposed_ratio: 1.0,
            max_overexposed_ratio: 1.0,
            max_low_information: 1.0,
            similarity_scope: SimilarityScope::Source,
            ..ReviewAnalysisConfig::default()
        };
        let workspace = run_review_analysis(&session, &config).unwrap();
        assert_eq!(workspace.items.len(), 2);
        assert_eq!(workspace.summary.similarity_groups, 1);
        assert_eq!(
            workspace
                .items
                .iter()
                .filter(|item| item.representative)
                .count(),
            1
        );

        let initially_excluded = workspace
            .items
            .iter()
            .find(|item| !item.representative)
            .unwrap()
            .asset_key
            .clone();
        let initially_representative = workspace
            .items
            .iter()
            .find(|item| item.representative)
            .unwrap()
            .asset_key
            .clone();
        update_review_items(
            &session,
            std::slice::from_ref(&initially_excluded),
            ReviewAction::Lock,
        )
        .unwrap();
        update_review_items(
            &session,
            std::slice::from_ref(&initially_representative),
            ReviewAction::Exclude,
        )
        .unwrap();
        let rerun = run_review_analysis(&session, &config).unwrap();
        let locked = rerun
            .items
            .iter()
            .find(|item| item.asset_key == initially_excluded)
            .unwrap();
        assert!(locked.locked);
        assert!(locked.representative);
        let excluded = rerun
            .items
            .iter()
            .find(|item| item.asset_key == initially_representative)
            .unwrap();
        assert_eq!(excluded.manual_decision.as_deref(), Some("exclude"));

        let undone = undo_review_action(&session).unwrap();
        assert!(undone.can_redo);
        assert!(
            undone
                .items
                .iter()
                .find(|item| item.asset_key == initially_representative)
                .unwrap()
                .manual_decision
                .is_none()
        );
        let redone = redo_review_action(&session).unwrap();
        assert!(redone.can_undo);
        assert_eq!(
            redone
                .items
                .iter()
                .find(|item| item.asset_key == initially_representative)
                .unwrap()
                .manual_decision
                .as_deref(),
            Some("exclude")
        );

        let switched = update_review_items(
            &session,
            std::slice::from_ref(&initially_representative),
            ReviewAction::MakeRepresentative,
        )
        .unwrap();
        assert!(
            switched
                .items
                .iter()
                .find(|item| item.asset_key == initially_representative)
                .unwrap()
                .representative
        );
        let switch_undone = undo_review_action(&session).unwrap();
        assert!(
            switch_undone
                .items
                .iter()
                .find(|item| item.asset_key == initially_excluded)
                .unwrap()
                .representative
        );
        let switch_redone = redo_review_action(&session).unwrap();
        assert!(
            switch_redone
                .items
                .iter()
                .find(|item| item.asset_key == initially_representative)
                .unwrap()
                .representative
        );

        let sources = list_sources(&session, 0, 10).unwrap();
        let output = root.join("export");
        let plan = plan_export(
            &session,
            &ExportRequest {
                source_id: sources[0].id.clone(),
                source_scope: ExportSourceScope::SourceGroup,
                candidate_id: None,
                output_dir: path_text(&output),
                naming_template: "{source}_{index}".to_owned(),
                format: ExportFormat::Png,
                conflict_strategy: ConflictStrategy::AppendSequence,
                content: ExportContent::Frames,
                review_scope: ExportReviewScope::Eligible,
                excluded_tiles: Vec::new(),
            },
        )
        .unwrap();
        assert_eq!(plan.items.len(), 1);
        assert_eq!(plan.skipped, 1);
        let mut all_request = ExportRequest {
            source_id: sources[0].id.clone(),
            source_scope: ExportSourceScope::SourceGroup,
            candidate_id: None,
            output_dir: path_text(&root.join("export-all")),
            naming_template: "{source}_{index}".to_owned(),
            format: ExportFormat::Png,
            conflict_strategy: ConflictStrategy::AppendSequence,
            content: ExportContent::Frames,
            review_scope: ExportReviewScope::All,
            excluded_tiles: Vec::new(),
        };
        let all_plan = plan_export(&session, &all_request).unwrap();
        assert_eq!(all_plan.items.len(), 2);
        assert_eq!(all_plan.skipped, 0);
        all_request.review_scope = ExportReviewScope::Eligible;
        assert_eq!(plan_export(&session, &all_request).unwrap().items.len(), 1);
        assert_eq!(full_hash(&first_path).unwrap(), first_hash);
        assert_eq!(full_hash(&second_path).unwrap(), second_hash);

        drop(session);
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn m3_roi_preview_and_atomic_export_share_the_same_plan() {
        let root = test_root("m3-export");
        let source_dir = root.join("cam_a");
        fs::create_dir_all(&source_dir).unwrap();
        let source_path = source_dir.join("frame 01.png");
        ImageBuffer::from_fn(64, 48, |x, y| Rgb([(x * 3) as u8, (y * 5) as u8, 90_u8]))
            .save(&source_path)
            .unwrap();
        ImageBuffer::from_fn(64, 48, |x, y| Rgb([40_u8, (x * 2) as u8, (y * 4) as u8]))
            .save(source_dir.join("frame 02.png"))
            .unwrap();
        let source_hash_before = full_hash(&source_path).unwrap();
        let mut session = ProjectSession::create(&root, "m3-project").unwrap();
        import_sources(
            &mut session,
            &[path_text(&source_dir)],
            Path::new("ffprobe"),
            Path::new("ffmpeg"),
        )
        .unwrap();
        let source = list_sources(&session, 0, 10).unwrap().remove(0);
        let render_config = TileRenderConfig {
            tile: TileConfig {
                tile_width: 32,
                tile_height: 32,
                overlap_x: 8,
                overlap_y: 8,
                edge_strategy: EdgeStrategy::Pad,
            },
            resize: ResizeMode::Stretch,
            padding: PaddingMode::Edge,
            fill: [0, 0, 0, 255],
        };
        save_roi_profile(
            &session,
            SaveRoiProfile {
                id: None,
                scope: RoiScope::SourceGroup,
                scope_value: source.source_group.clone(),
                name: "track".to_owned(),
                roi: Roi {
                    x: 0,
                    y: 0,
                    width: 48,
                    height: 40,
                },
                render_config,
            },
        )
        .unwrap();
        let override_profile = save_roi_profile(
            &session,
            SaveRoiProfile {
                id: None,
                scope: RoiScope::Source,
                scope_value: source.id.clone(),
                name: "track".to_owned(),
                roi: Roi {
                    x: 8,
                    y: 4,
                    width: 40,
                    height: 40,
                },
                render_config,
            },
        )
        .unwrap();
        let effective = list_effective_roi_profiles(&session, &source.id).unwrap();
        assert_eq!(effective.len(), 1);
        assert_eq!(effective[0].id, override_profile.id);
        assert!(!effective[0].inherited);

        let previews = preview_source_tiles(&session, &source.id, None, 100).unwrap();
        assert_eq!(previews.len(), 4);
        assert!(
            previews
                .iter()
                .all(|preview| Path::new(&preview.preview_path).is_file())
        );

        let output_dir = root.join("exports");
        let request = ExportRequest {
            source_id: source.id.clone(),
            source_scope: ExportSourceScope::Current,
            candidate_id: None,
            output_dir: path_text(&output_dir),
            naming_template: "{source}_{roi}_r{row}_c{col}_{index}".to_owned(),
            format: ExportFormat::Png,
            conflict_strategy: ConflictStrategy::AppendSequence,
            content: ExportContent::Tiles,
            review_scope: ExportReviewScope::Eligible,
            excluded_tiles: Vec::new(),
        };
        let plan = plan_export(&session, &request).unwrap();
        assert_eq!(plan.items.len(), previews.len());
        for (planned, preview) in plan.items.iter().zip(&previews) {
            assert_eq!(planned.placement, preview.placement);
        }
        let mut excluded_request = request.clone();
        excluded_request.excluded_tiles.push(ExcludedTile {
            source_id: source.id.clone(),
            candidate_id: None,
            roi_profile_id: previews[0].roi_profile_id.clone(),
            row: previews[0].placement.row,
            column: previews[0].placement.column,
        });
        let excluded_plan = plan_export(&session, &excluded_request).unwrap();
        assert_eq!(excluded_plan.items.len(), previews.len() - 1);
        assert_eq!(excluded_plan.skipped, 1);
        let mut group_request = request.clone();
        group_request.source_scope = ExportSourceScope::SourceGroup;
        group_request.output_dir = path_text(&root.join("group-exports"));
        let group_plan = plan_export(&session, &group_request).unwrap();
        assert_eq!(group_plan.items.len(), previews.len() * 2);
        let mut export_progress = Vec::new();
        let result =
            run_export_with_progress(&session, &request, |event| export_progress.push(event))
                .unwrap();
        assert_eq!(result.written, 4);
        assert!(result.failures.is_empty());
        assert_eq!(export_progress.last().map(|event| event.completed), Some(4));
        let manifest = fs::read_to_string(&result.manifest_path).unwrap();
        assert_eq!(manifest.lines().count(), 4);
        assert_eq!(full_hash(&source_path).unwrap(), source_hash_before);
        drop(session);
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn exports_all_video_candidates_without_requiring_roi_tiles() {
        let root = test_root("m3-video-frame-export");
        fs::create_dir_all(&root).unwrap();
        let session = ProjectSession::create(&root, "video-frame-export").unwrap();
        let store = ProjectStore::open(session.database_path()).unwrap();
        let now = Utc::now().to_rfc3339();
        let source = StoredSourceAsset {
            id: "video-source".to_owned(),
            absolute_path: path_text(&root.join("source.mp4")),
            file_name: "source.mp4".to_owned(),
            relative_folder: String::new(),
            source_group: "camera-a".to_owned(),
            source_identifier: "source".to_owned(),
            kind: SourceKind::Video,
            status: SourceStatus::Online,
            size_bytes: 0,
            modified_unix_ms: 0,
            quick_fingerprint: "fixture".to_owned(),
            sha256: None,
            width: Some(40),
            height: Some(30),
            duration_ms: Some(2_000),
            codec: Some("fixture".to_owned()),
            frame_rate: Some("1/1".to_owned()),
            capture_time: None,
            capture_time_source: None,
            orientation: Some(1),
            thumbnail_path: None,
            error: None,
            imported_at: now.clone(),
            last_checked_at: now.clone(),
        };
        store.upsert_source(&source).unwrap();
        for (index, timestamp_ms) in [500_u64, 1_500].into_iter().enumerate() {
            let frame_path = root.join(format!("frame-{index}.jpg"));
            ImageBuffer::from_pixel(40, 30, Rgb([20 + index as u8, 80, 120]))
                .save(&frame_path)
                .unwrap();
            store
                .upsert_candidate(&StoredCandidateImage {
                    id: format!("candidate-{index}"),
                    source_id: source.id.clone(),
                    video_offset_ms: timestamp_ms,
                    source_frame_number: Some(index as u64),
                    selection_method: "fixture".to_owned(),
                    parameters_json: "{}".to_owned(),
                    image_path: path_text(&frame_path),
                    thumbnail_path: path_text(&frame_path),
                    width: 40,
                    height: 30,
                    pinned: false,
                    created_at: now.clone(),
                })
                .unwrap();
        }

        let output_dir = root.join("exports");
        let mut request: ExportRequest = serde_json::from_value(serde_json::json!({
            "sourceId": source.id,
            "candidateId": null,
            "outputDir": path_text(&output_dir),
            "namingTemplate": "{source}_{timestamp_ms}_{index}",
            "format": "jpeg",
            "conflictStrategy": "append_sequence",
            "content": "frames"
        }))
        .unwrap();
        let plan = plan_export(&session, &request).unwrap();
        assert_eq!(plan.items.len(), 2);
        assert_eq!(plan.items[0].candidate_id.as_deref(), Some("candidate-0"));
        assert_eq!(plan.items[1].candidate_id.as_deref(), Some("candidate-1"));
        let result = run_export(&session, &request).unwrap();
        assert_eq!(result.written, 2);
        assert!(result.failures.is_empty());
        assert_eq!(
            fs::read(output_dir.join(&plan.items[0].file_name)).unwrap(),
            fs::read(root.join("frame-0.jpg")).unwrap()
        );
        assert_eq!(
            fs::read_to_string(result.manifest_path)
                .unwrap()
                .lines()
                .count(),
            2
        );

        request.candidate_id = Some("candidate-1".to_owned());
        request.output_dir = path_text(&root.join("selected-export"));
        let selected_plan = plan_export(&session, &request).unwrap();
        assert_eq!(selected_plan.items.len(), 1);
        assert_eq!(
            selected_plan.items[0].candidate_id.as_deref(),
            Some("candidate-1")
        );

        drop(session);
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn deletes_selected_or_all_candidates_without_touching_the_source_video() {
        let root = test_root("candidate-delete");
        fs::create_dir_all(&root).unwrap();
        let mut session = ProjectSession::create(&root, "candidate-delete").unwrap();
        let store = ProjectStore::open(session.database_path()).unwrap();
        let source_path = root.join("source.mp4");
        fs::write(&source_path, b"immutable source fixture").unwrap();
        let now = Utc::now().to_rfc3339();
        let source = StoredSourceAsset {
            id: "delete-source".to_owned(),
            absolute_path: path_text(&source_path),
            file_name: "source.mp4".to_owned(),
            relative_folder: String::new(),
            source_group: "camera-a".to_owned(),
            source_identifier: "source".to_owned(),
            kind: SourceKind::Video,
            status: SourceStatus::Online,
            size_bytes: fs::metadata(&source_path).unwrap().len(),
            modified_unix_ms: 0,
            quick_fingerprint: "fixture".to_owned(),
            sha256: None,
            width: Some(40),
            height: Some(30),
            duration_ms: Some(2_000),
            codec: Some("fixture".to_owned()),
            frame_rate: Some("1/1".to_owned()),
            capture_time: None,
            capture_time_source: None,
            orientation: Some(1),
            thumbnail_path: None,
            error: None,
            imported_at: now.clone(),
            last_checked_at: now.clone(),
        };
        store.upsert_source(&source).unwrap();
        let image_dir = session.project_dir().join("cache/candidates/delete-source");
        let thumbnail_dir = session
            .project_dir()
            .join("cache/candidate-thumbnails/delete-source");
        fs::create_dir_all(&image_dir).unwrap();
        fs::create_dir_all(&thumbnail_dir).unwrap();
        let mut candidate_paths = Vec::new();
        for index in 0_u8..2 {
            let image_path = image_dir.join(format!("candidate-{index}.jpg"));
            let thumbnail_path = thumbnail_dir.join(format!("candidate-{index}.jpg"));
            ImageBuffer::from_pixel(40, 30, Rgb([20_u8 + index, 80, 120]))
                .save(&image_path)
                .unwrap();
            fs::copy(&image_path, &thumbnail_path).unwrap();
            store
                .upsert_candidate(&StoredCandidateImage {
                    id: format!("candidate-{index}"),
                    source_id: source.id.clone(),
                    video_offset_ms: u64::from(index) * 500,
                    source_frame_number: Some(u64::from(index)),
                    selection_method: "fixture".to_owned(),
                    parameters_json: "{}".to_owned(),
                    image_path: path_text(&image_path),
                    thumbnail_path: path_text(&thumbnail_path),
                    width: 40,
                    height: 30,
                    pinned: index == 0,
                    created_at: now.clone(),
                })
                .unwrap();
            candidate_paths.push((image_path, thumbnail_path));
        }
        session.refresh_summary().unwrap();

        let selected =
            delete_candidates(&mut session, &source.id, Some(&["candidate-0".to_owned()])).unwrap();
        assert_eq!(selected.deleted, 1);
        assert!(selected.failures.is_empty());
        assert!(!candidate_paths[0].0.exists());
        assert!(!candidate_paths[0].1.exists());
        assert!(candidate_paths[1].0.exists());
        assert_eq!(store.list_candidates(&source.id, 0, 10).unwrap().len(), 1);

        let all = delete_candidates(&mut session, &source.id, None).unwrap();
        assert_eq!(all.deleted, 1);
        assert!(all.failures.is_empty());
        assert!(!candidate_paths[1].0.exists());
        assert!(!candidate_paths[1].1.exists());
        assert!(source_path.exists());
        assert_eq!(session.summary.candidate_count, 0);

        drop(session);
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn removes_offline_source_records_without_deleting_or_blocking_reimport() {
        let root = test_root("source-delete");
        let source_dir = root.join("batch-images");
        fs::create_dir_all(&source_dir).unwrap();
        let source_path = source_dir.join("frame.png");
        ImageBuffer::from_pixel(40, 30, Rgb([20_u8, 80, 120]))
            .save(&source_path)
            .unwrap();
        let mut session = ProjectSession::create(&root, "source-delete").unwrap();
        import_sources(
            &mut session,
            &[path_text(&source_dir)],
            Path::new("ffprobe"),
            Path::new("ffmpeg"),
        )
        .unwrap();
        let source = list_sources(&session, 0, 10).unwrap().remove(0);
        assert_eq!(
            list_all_source_ids(&session).unwrap(),
            vec![source.id.clone()]
        );
        let thumbnail_path = PathBuf::from(source.thumbnail_path.clone().unwrap());
        let preview_dir = session.project_dir().join("cache").join("video-previews");
        fs::create_dir_all(&preview_dir).unwrap();
        let preview_path = preview_dir.join(format!("{}-webview-v1.mp4", source.id));
        let partial_preview_path =
            preview_dir.join(format!("{}-webview-v1.partial.mp4", source.id));
        fs::write(&preview_path, b"preview").unwrap();
        fs::write(&partial_preview_path, b"partial").unwrap();
        save_roi_profile(
            &session,
            SaveRoiProfile {
                id: None,
                scope: RoiScope::Source,
                scope_value: source.id.clone(),
                name: "temporary".to_owned(),
                roi: Roi {
                    x: 0,
                    y: 0,
                    width: 40,
                    height: 30,
                },
                render_config: TileRenderConfig {
                    tile: TileConfig {
                        tile_width: 40,
                        tile_height: 30,
                        overlap_x: 0,
                        overlap_y: 0,
                        edge_strategy: EdgeStrategy::ShiftToEdge,
                    },
                    resize: ResizeMode::Stretch,
                    padding: PaddingMode::Constant,
                    fill: [0, 0, 0, 255],
                },
            },
        )
        .unwrap();

        let mut progress = Vec::new();
        let removed =
            delete_sources_with_progress(&mut session, std::slice::from_ref(&source.id), |event| {
                progress.push(event)
            })
            .unwrap();
        assert_eq!(removed.deleted, 1);
        assert_eq!(removed.candidate_deleted, 0);
        assert!(removed.failures.is_empty());
        assert_eq!(progress.len(), 2);
        assert_eq!(progress[0].completed, 0);
        assert_eq!(progress[0].total, 1);
        assert_eq!(progress[1].completed, 1);
        assert_eq!(progress[1].deleted, 1);
        assert!(source_path.exists());
        assert!(!thumbnail_path.exists());
        assert!(!preview_path.exists());
        assert!(!partial_preview_path.exists());
        assert!(list_sources(&session, 0, 10).unwrap().is_empty());
        assert!(
            ProjectStore::open(session.database_path())
                .unwrap()
                .list_roi_profiles("source", &source.id)
                .unwrap()
                .is_empty()
        );

        let reimported = import_sources(
            &mut session,
            &[path_text(&source_dir)],
            Path::new("ffprobe"),
            Path::new("ffmpeg"),
        )
        .unwrap();
        assert_eq!(reimported.imported, 1);
        assert_eq!(list_sources(&session, 0, 10).unwrap().len(), 1);

        drop(session);
        fs::remove_dir_all(root).unwrap();
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
        let mut import_progress = Vec::new();
        let result = import_sources_with_progress(
            &mut session,
            &[path_text(&root.join("中文 素材"))],
            Path::new("ffprobe"),
            Path::new("ffmpeg"),
            |progress| import_progress.push(progress),
        )
        .unwrap();
        assert_eq!(result.imported, 1);
        assert_eq!(import_progress.last().map(|event| event.completed), Some(1));
        let mut assets = list_sources(&session, 0, 10).unwrap();
        assert_eq!(assets[0].source_group, "cam1_01");
        assert_eq!(assets[0].width, Some(96));
        assert!(assets[0].sha256.is_none());
        assert_eq!(
            complete_pending_hashes(&session.database_path()).unwrap(),
            1
        );
        assets = list_sources(&session, 0, 10).unwrap();
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
}
