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
    StoredRoiProfile, StoredSourceAsset, StoredVideoSelection,
};
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

pub fn list_all_source_ids(session: &ProjectSession) -> Result<Vec<String>, ApplicationError> {
    Ok(ProjectStore::open(session.database_path())?.list_source_ids()?)
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

pub fn delete_candidates(
    session: &mut ProjectSession,
    source_id: &str,
    candidate_ids: Option<&[String]>,
) -> Result<CandidateDeletionResult, ApplicationError> {
    let store = ProjectStore::open(session.database_path())?;
    store
        .get_source(source_id)?
        .ok_or_else(|| ApplicationError::SourceNotFound(source_id.to_owned()))?;
    let candidates = store.delete_candidates(source_id, candidate_ids)?;
    let cache_root = session.project_dir().join("cache").canonicalize()?;
    let mut result = CandidateDeletionResult {
        deleted: candidates.len() as u64,
        failures: Vec::new(),
    };
    remove_candidate_cache_files(&cache_root, candidates, &mut result.failures);
    session.refresh_summary()?;
    Ok(result)
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

#[derive(Debug, Clone)]
struct ReviewAnalysisInput {
    asset_key: String,
    source: StoredSourceAsset,
    candidate: Option<StoredCandidateImage>,
    image_path: String,
    video_offset_ms: Option<u64>,
    pinned: bool,
    metrics: Option<QualityMetrics>,
    content_sha256: String,
    perceptual_hash: u64,
    decode_error: Option<String>,
    reasons: Vec<String>,
}

pub fn run_review_analysis(
    session: &ProjectSession,
    config: &ReviewAnalysisConfig,
) -> Result<ReviewWorkspace, ApplicationError> {
    validate_review_config(config)?;
    let store = ProjectStore::open(session.database_path())?;
    store.reset_review_analysis()?;
    let sources = store.list_sources(0, 10_000)?;
    let analyzed_at = Utc::now().to_rfc3339();
    let mut inputs = Vec::new();
    for source in sources {
        if source.status != SourceStatus::Online {
            continue;
        }
        if source.kind == SourceKind::Image {
            inputs.push(analyze_review_input(
                &source,
                None,
                &source.absolute_path,
                config,
            ));
        } else {
            for candidate in store.list_candidates(&source.id, 0, 100_000)? {
                let image_path = candidate.image_path.clone();
                inputs.push(analyze_review_input(
                    &source,
                    Some(candidate),
                    &image_path,
                    config,
                ));
            }
        }
    }
    if inputs.is_empty() {
        return Err(ApplicationError::NoReviewAssets);
    }

    for input in &inputs {
        let metrics = input.metrics.unwrap_or(QualityMetrics {
            width: 0,
            height: 0,
            aspect_ratio: 0.0,
            sharpness: 0.0,
            underexposed_ratio: 0.0,
            overexposed_ratio: 0.0,
            entropy: 0.0,
            low_information: 1.0,
        });
        store.upsert_quality_assessment(&StoredQualityAssessment {
            asset_key: input.asset_key.clone(),
            source_id: input.source.id.clone(),
            candidate_id: input
                .candidate
                .as_ref()
                .map(|candidate| candidate.id.clone()),
            width: metrics.width,
            height: metrics.height,
            aspect_ratio: metrics.aspect_ratio,
            sharpness: metrics.sharpness,
            underexposed_ratio: metrics.underexposed_ratio,
            overexposed_ratio: metrics.overexposed_ratio,
            entropy: metrics.entropy,
            low_information: metrics.low_information,
            content_sha256: input.content_sha256.clone(),
            perceptual_hash: format!("{:016x}", input.perceptual_hash),
            algorithm_version: QUALITY_ALGORITHM_VERSION.to_owned(),
            analyzed_at: analyzed_at.clone(),
            decode_error: input.decode_error.clone(),
        })?;
        let automatic_status = if input.decode_error.is_some() {
            "error"
        } else if input.reasons.is_empty() {
            "keep"
        } else if input.pinned {
            "warning"
        } else {
            "suggest_exclude"
        };
        store.upsert_review_asset(&StoredReviewAsset {
            asset_key: input.asset_key.clone(),
            source_id: input.source.id.clone(),
            candidate_id: input
                .candidate
                .as_ref()
                .map(|candidate| candidate.id.clone()),
            automatic_status: automatic_status.to_owned(),
            automatic_reasons_json: serde_json::to_string(&input.reasons)?,
            manual_decision: None,
            locked: input.pinned,
            similarity_group_id: None,
            similarity_score: None,
            representative: false,
            locked_conflict: false,
            updated_at: analyzed_at.clone(),
        })?;
    }

    assign_similarity_groups(&store, &mut inputs, config, &analyzed_at)?;
    list_review_workspace(session)
}

pub fn list_review_workspace(
    session: &ProjectSession,
) -> Result<ReviewWorkspace, ApplicationError> {
    let store = ProjectStore::open(session.database_path())?;
    let sources = store
        .list_sources(0, 10_000)?
        .into_iter()
        .map(|source| (source.id.clone(), source))
        .collect::<HashMap<_, _>>();
    let qualities = store
        .list_quality_assessments()?
        .into_iter()
        .map(|quality| (quality.asset_key.clone(), quality))
        .collect::<HashMap<_, _>>();
    let mut items = Vec::new();
    for state in store.list_review_assets()? {
        let Some(source) = sources.get(&state.source_id) else {
            continue;
        };
        let candidate = state
            .candidate_id
            .as_deref()
            .map(|id| store.get_candidate(id))
            .transpose()?
            .flatten();
        let quality = qualities.get(&state.asset_key);
        let metrics = quality.map(|quality| QualityMetrics {
            width: quality.width,
            height: quality.height,
            aspect_ratio: quality.aspect_ratio,
            sharpness: quality.sharpness,
            underexposed_ratio: quality.underexposed_ratio,
            overexposed_ratio: quality.overexposed_ratio,
            entropy: quality.entropy,
            low_information: quality.low_information,
        });
        items.push(ReviewItem {
            asset_key: state.asset_key,
            source_id: source.id.clone(),
            candidate_id: candidate.as_ref().map(|candidate| candidate.id.clone()),
            source_group: source.source_group.clone(),
            source_identifier: source.source_identifier.clone(),
            display_name: candidate
                .as_ref()
                .map(|candidate| {
                    format!(
                        "{} · {}",
                        source.file_name,
                        format_review_time(candidate.video_offset_ms)
                    )
                })
                .unwrap_or_else(|| source.file_name.clone()),
            image_path: candidate
                .as_ref()
                .map(|candidate| candidate.image_path.clone())
                .unwrap_or_else(|| source.absolute_path.clone()),
            thumbnail_path: candidate
                .as_ref()
                .map(|candidate| candidate.thumbnail_path.clone())
                .or_else(|| source.thumbnail_path.clone())
                .unwrap_or_else(|| source.absolute_path.clone()),
            video_offset_ms: candidate
                .as_ref()
                .map(|candidate| candidate.video_offset_ms),
            selection_method: candidate
                .as_ref()
                .map(|candidate| candidate.selection_method.clone())
                .unwrap_or_else(|| "source_image".to_owned()),
            pinned: candidate.as_ref().is_some_and(|candidate| candidate.pinned),
            metrics,
            automatic_status: state.automatic_status,
            automatic_reasons: serde_json::from_str(&state.automatic_reasons_json)
                .unwrap_or_default(),
            manual_decision: state.manual_decision,
            locked: state.locked,
            similarity_group_id: state.similarity_group_id,
            similarity_score: state.similarity_score,
            representative: state.representative,
            locked_conflict: state.locked_conflict,
            decode_error: quality.and_then(|quality| quality.decode_error.clone()),
        });
    }
    items.sort_by(|left, right| {
        left.source_group
            .cmp(&right.source_group)
            .then_with(|| left.source_identifier.cmp(&right.source_identifier))
            .then_with(|| left.video_offset_ms.cmp(&right.video_offset_ms))
            .then_with(|| left.asset_key.cmp(&right.asset_key))
    });
    let groups = items
        .iter()
        .filter_map(|item| item.similarity_group_id.as_deref())
        .collect::<HashSet<_>>()
        .len() as u64;
    let summary = ReviewSummary {
        total: items.len() as u64,
        keep: items
            .iter()
            .filter(|item| {
                item.manual_decision.as_deref() == Some("keep")
                    || (item.manual_decision.is_none() && item.automatic_status == "keep")
            })
            .count() as u64,
        suggested_exclude: items
            .iter()
            .filter(|item| {
                item.manual_decision.is_none() && item.automatic_status == "suggest_exclude"
            })
            .count() as u64,
        manually_excluded: items
            .iter()
            .filter(|item| item.manual_decision.as_deref() == Some("exclude"))
            .count() as u64,
        warning: items
            .iter()
            .filter(|item| item.automatic_status == "warning")
            .count() as u64,
        failed: items
            .iter()
            .filter(|item| item.automatic_status == "error")
            .count() as u64,
        locked: items.iter().filter(|item| item.locked).count() as u64,
        similarity_groups: groups,
    };
    Ok(ReviewWorkspace { items, summary })
}

pub fn update_review_items(
    session: &ProjectSession,
    asset_keys: &[String],
    action: ReviewAction,
) -> Result<ReviewWorkspace, ApplicationError> {
    let store = ProjectStore::open(session.database_path())?;
    let now = Utc::now().to_rfc3339();
    for asset_key in asset_keys.iter().collect::<HashSet<_>>() {
        let Some(before) = store.get_review_asset(asset_key)? else {
            continue;
        };
        if action == ReviewAction::MakeRepresentative {
            let group_id = before
                .similarity_group_id
                .as_deref()
                .ok_or(ApplicationError::ReviewAssetHasNoSimilarityGroup)?;
            if !store.set_group_representative(group_id, asset_key, &now)? {
                continue;
            }
            let after = store.get_review_asset(asset_key)?.unwrap_or(before.clone());
            store.insert_review_audit(
                &Uuid::new_v4().to_string(),
                asset_key,
                "make_representative",
                &serde_json::to_string(&before)?,
                &serde_json::to_string(&after)?,
                "local_user",
                &now,
            )?;
            continue;
        }
        let mut manual_decision = before.manual_decision.clone();
        let mut locked = before.locked;
        match action {
            ReviewAction::Keep => manual_decision = Some("keep".to_owned()),
            ReviewAction::Exclude => manual_decision = Some("exclude".to_owned()),
            ReviewAction::Restore => manual_decision = None,
            ReviewAction::Lock => {
                locked = true;
                manual_decision = Some("keep".to_owned());
            }
            ReviewAction::Unlock => locked = false,
            ReviewAction::MakeRepresentative => unreachable!(),
        }
        store.set_review_state(asset_key, manual_decision.as_deref(), locked, &now)?;
        let after = store.get_review_asset(asset_key)?.unwrap_or(before.clone());
        store.insert_review_audit(
            &Uuid::new_v4().to_string(),
            asset_key,
            review_action_text(action),
            &serde_json::to_string(&before)?,
            &serde_json::to_string(&after)?,
            "local_user",
            &now,
        )?;
    }
    list_review_workspace(session)
}

fn analyze_review_input(
    source: &StoredSourceAsset,
    candidate: Option<StoredCandidateImage>,
    image_path: &str,
    config: &ReviewAnalysisConfig,
) -> ReviewAnalysisInput {
    let asset_key = candidate
        .as_ref()
        .map(|candidate| format!("candidate:{}", candidate.id))
        .unwrap_or_else(|| format!("source:{}", source.id));
    let pinned = candidate.as_ref().is_some_and(|candidate| candidate.pinned);
    let video_offset_ms = candidate
        .as_ref()
        .map(|candidate| candidate.video_offset_ms);
    match image::open(image_path) {
        Ok(image) => {
            let metrics = measure_quality(&image);
            let mut reasons = Vec::new();
            if metrics.width < config.min_width || metrics.height < config.min_height {
                reasons.push("分辨率低于阈值".to_owned());
            }
            if metrics.sharpness < config.min_sharpness {
                reasons.push("清晰度低于阈值".to_owned());
            }
            if metrics.underexposed_ratio > config.max_underexposed_ratio {
                reasons.push("暗部裁切比例过高".to_owned());
            }
            if metrics.overexposed_ratio > config.max_overexposed_ratio {
                reasons.push("高光裁切比例过高".to_owned());
            }
            if metrics.low_information > config.max_low_information {
                reasons.push("低信息量程度过高".to_owned());
            }
            ReviewAnalysisInput {
                asset_key,
                source: source.clone(),
                candidate,
                image_path: image_path.to_owned(),
                video_offset_ms,
                pinned,
                metrics: Some(metrics),
                content_sha256: full_hash(Path::new(image_path)).unwrap_or_default(),
                perceptual_hash: perceptual_hash(&image),
                decode_error: None,
                reasons,
            }
        }
        Err(error) => ReviewAnalysisInput {
            asset_key,
            source: source.clone(),
            candidate,
            image_path: image_path.to_owned(),
            video_offset_ms,
            pinned,
            metrics: None,
            content_sha256: String::new(),
            perceptual_hash: 0,
            decode_error: Some(error.to_string()),
            reasons: vec!["图片解码失败".to_owned()],
        },
    }
}

fn assign_similarity_groups(
    store: &ProjectStore,
    inputs: &mut [ReviewAnalysisInput],
    config: &ReviewAnalysisConfig,
    analyzed_at: &str,
) -> Result<(), ApplicationError> {
    let valid = inputs
        .iter()
        .enumerate()
        .filter(|(_, input)| input.decode_error.is_none())
        .map(|(index, _)| index)
        .collect::<Vec<_>>();
    let mut parents = (0..inputs.len()).collect::<Vec<_>>();
    let mut edge_scores = HashMap::<(usize, usize), f64>::new();
    let mut exact = HashMap::<String, Vec<usize>>::new();
    for &index in &valid {
        if !inputs[index].content_sha256.is_empty() {
            exact
                .entry(inputs[index].content_sha256.clone())
                .or_default()
                .push(index);
        }
    }
    for members in exact.values().filter(|members| members.len() > 1) {
        for pair in members.windows(2) {
            union_review(&mut parents, pair[0], pair[1]);
            edge_scores.insert(ordered_pair(pair[0], pair[1]), 1.0);
        }
    }

    let mut comparisons = 0_u64;
    for (position, &left_index) in valid.iter().enumerate() {
        for &right_index in valid.iter().skip(position + 1) {
            let left = &inputs[left_index];
            let right = &inputs[right_index];
            if left.content_sha256 == right.content_sha256
                || !same_similarity_partition(left, right, config.similarity_scope)
                || outside_video_window(left, right, config.video_time_window_ms)
            {
                continue;
            }
            comparisons += 1;
            if comparisons > 2_000_000 {
                return Err(ApplicationError::SimilarityComparisonTooLarge);
            }
            if hamming_distance(left.perceptual_hash, right.perceptual_hash) > config.phash_distance
            {
                continue;
            }
            let left_image = image::open(&left.image_path)?;
            let right_image = image::open(&right.image_path)?;
            let score = global_ssim(&left_image, &right_image);
            if score >= config.ssim_threshold {
                union_review(&mut parents, left_index, right_index);
                edge_scores.insert(ordered_pair(left_index, right_index), score);
            }
        }
    }

    let current_states = store
        .list_review_assets()?
        .into_iter()
        .map(|state| (state.asset_key.clone(), state))
        .collect::<HashMap<_, _>>();
    let mut groups = HashMap::<usize, Vec<usize>>::new();
    for &index in &valid {
        let root = find_review_root(&mut parents, index);
        groups.entry(root).or_default().push(index);
    }
    for members in groups.values().filter(|members| members.len() > 1) {
        let mut sorted_keys = members
            .iter()
            .map(|index| inputs[*index].asset_key.as_str())
            .collect::<Vec<_>>();
        sorted_keys.sort_unstable();
        let mut hasher = Sha256::new();
        hasher.update(SIMILARITY_ALGORITHM_VERSION.as_bytes());
        for key in sorted_keys {
            hasher.update(key.as_bytes());
        }
        let group_id = format!("sim-{}", &hex::encode(hasher.finalize())[..12]);
        let locked_members = members
            .iter()
            .filter(|index| {
                current_states
                    .get(&inputs[**index].asset_key)
                    .is_some_and(|state| state.locked)
            })
            .copied()
            .collect::<Vec<_>>();
        let representative = choose_representative(
            if locked_members.is_empty() {
                members
            } else {
                &locked_members
            },
            inputs,
        );
        let locked_conflict = locked_members.len() > 1;
        for &index in members {
            let input = &mut inputs[index];
            let state = current_states.get(&input.asset_key);
            let locked = state.is_some_and(|state| state.locked);
            let is_representative = index == representative;
            let similarity_score = if is_representative {
                1.0
            } else {
                members
                    .iter()
                    .filter_map(|other| edge_scores.get(&ordered_pair(index, *other)).copied())
                    .fold(0.0_f64, f64::max)
            };
            let mut reasons = input.reasons.clone();
            if !is_representative {
                reasons.push(format!("相似组 {} 中存在更合适的代表图", &group_id[4..]));
            }
            let automatic_status = if is_representative {
                if reasons.is_empty() {
                    "keep"
                } else {
                    "warning"
                }
            } else if locked {
                "warning"
            } else {
                "suggest_exclude"
            };
            store.upsert_review_asset(&StoredReviewAsset {
                asset_key: input.asset_key.clone(),
                source_id: input.source.id.clone(),
                candidate_id: input
                    .candidate
                    .as_ref()
                    .map(|candidate| candidate.id.clone()),
                automatic_status: automatic_status.to_owned(),
                automatic_reasons_json: serde_json::to_string(&reasons)?,
                manual_decision: None,
                locked,
                similarity_group_id: Some(group_id.clone()),
                similarity_score: Some(similarity_score),
                representative: is_representative,
                locked_conflict,
                updated_at: analyzed_at.to_owned(),
            })?;
        }
    }
    Ok(())
}

fn validate_review_config(config: &ReviewAnalysisConfig) -> Result<(), ApplicationError> {
    if config.min_width == 0
        || config.min_height == 0
        || !config.min_sharpness.is_finite()
        || !(0.0..=1.0).contains(&config.max_underexposed_ratio)
        || !(0.0..=1.0).contains(&config.max_overexposed_ratio)
        || !(0.0..=1.0).contains(&config.max_low_information)
        || config.phash_distance > 64
        || !(0.0..=1.0).contains(&config.ssim_threshold)
    {
        return Err(ApplicationError::InvalidReviewConfiguration);
    }
    Ok(())
}

fn review_action_text(action: ReviewAction) -> &'static str {
    match action {
        ReviewAction::Keep => "keep",
        ReviewAction::Exclude => "exclude",
        ReviewAction::Restore => "restore",
        ReviewAction::Lock => "lock",
        ReviewAction::Unlock => "unlock",
        ReviewAction::MakeRepresentative => "make_representative",
    }
}

fn same_similarity_partition(
    left: &ReviewAnalysisInput,
    right: &ReviewAnalysisInput,
    scope: SimilarityScope,
) -> bool {
    match scope {
        SimilarityScope::Source => left.source.id == right.source.id,
        SimilarityScope::SourceGroup => left.source.source_group == right.source.source_group,
        SimilarityScope::Project => true,
    }
}

fn outside_video_window(
    left: &ReviewAnalysisInput,
    right: &ReviewAnalysisInput,
    window_ms: u64,
) -> bool {
    left.source.id == right.source.id
        && left.video_offset_ms.is_some()
        && right.video_offset_ms.is_some()
        && left
            .video_offset_ms
            .unwrap()
            .abs_diff(right.video_offset_ms.unwrap())
            > window_ms
}

fn choose_representative(members: &[usize], inputs: &[ReviewAnalysisInput]) -> usize {
    let mut ranked = members.to_vec();
    ranked.sort_by(|left, right| {
        review_quality_score(&inputs[*right])
            .total_cmp(&review_quality_score(&inputs[*left]))
            .then_with(|| {
                inputs[*left]
                    .video_offset_ms
                    .cmp(&inputs[*right].video_offset_ms)
            })
            .then_with(|| inputs[*left].asset_key.cmp(&inputs[*right].asset_key))
    });
    ranked[0]
}

fn review_quality_score(input: &ReviewAnalysisInput) -> f64 {
    let Some(metrics) = input.metrics else {
        return f64::NEG_INFINITY;
    };
    (metrics.sharpness + 1.0).ln() * 3.0
        + ((metrics.width as f64 * metrics.height as f64) + 1.0).ln() * 0.25
        - (metrics.underexposed_ratio + metrics.overexposed_ratio) * 10.0
        - metrics.low_information * 4.0
}

fn ordered_pair(left: usize, right: usize) -> (usize, usize) {
    if left <= right {
        (left, right)
    } else {
        (right, left)
    }
}

fn find_review_root(parents: &mut [usize], index: usize) -> usize {
    if parents[index] != index {
        parents[index] = find_review_root(parents, parents[index]);
    }
    parents[index]
}

fn union_review(parents: &mut [usize], left: usize, right: usize) {
    let left_root = find_review_root(parents, left);
    let right_root = find_review_root(parents, right);
    if left_root != right_root {
        parents[right_root] = left_root;
    }
}

fn format_review_time(timestamp_ms: u64) -> String {
    let seconds = timestamp_ms / 1_000;
    format!(
        "{:02}:{:02}:{:02}.{:03}",
        seconds / 3_600,
        (seconds % 3_600) / 60,
        seconds % 60,
        timestamp_ms % 1_000
    )
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

pub fn save_roi_profile(
    session: &ProjectSession,
    draft: SaveRoiProfile,
) -> Result<RoiProfile, ApplicationError> {
    let scope_value = draft.scope_value.trim();
    let name = draft.name.trim();
    if scope_value.is_empty() || name.is_empty() {
        return Err(ApplicationError::InvalidRoiProfile);
    }
    draft.render_config.validate()?;
    draft
        .roi
        .validate(u32::MAX.saturating_sub(1), u32::MAX.saturating_sub(1))?;
    let store = ProjectStore::open(session.database_path())?;
    if draft.scope == RoiScope::Source {
        let source = store
            .get_source(scope_value)?
            .ok_or_else(|| ApplicationError::SourceNotFound(scope_value.to_owned()))?;
        draft.roi.validate(
            source.width.ok_or(ApplicationError::MissingDimensions)?,
            source.height.ok_or(ApplicationError::MissingDimensions)?,
        )?;
    }
    let now = Utc::now().to_rfc3339();
    let stored = StoredRoiProfile {
        id: draft.id.unwrap_or_else(|| Uuid::new_v4().to_string()),
        scope_kind: draft.scope.as_str().to_owned(),
        scope_value: scope_value.to_owned(),
        name: name.to_owned(),
        x: draft.roi.x,
        y: draft.roi.y,
        width: draft.roi.width,
        height: draft.roi.height,
        render_config_json: serde_json::to_string(&draft.render_config)?,
        created_at: now.clone(),
        updated_at: now,
    };
    store.upsert_roi_profile(&stored)?;
    roi_profile_from_stored(stored, false)
}

pub fn list_effective_roi_profiles(
    session: &ProjectSession,
    source_id: &str,
) -> Result<Vec<RoiProfile>, ApplicationError> {
    let store = ProjectStore::open(session.database_path())?;
    let source = store
        .get_source(source_id)?
        .ok_or_else(|| ApplicationError::SourceNotFound(source_id.to_owned()))?;
    let mut profiles = HashMap::<String, RoiProfile>::new();
    for stored in store.list_roi_profiles("source_group", &source.source_group)? {
        let profile = roi_profile_from_stored(stored, true)?;
        profiles.insert(profile.name.to_ascii_lowercase(), profile);
    }
    for stored in store.list_roi_profiles("source", source_id)? {
        let profile = roi_profile_from_stored(stored, false)?;
        profiles.insert(profile.name.to_ascii_lowercase(), profile);
    }
    let mut result = profiles.into_values().collect::<Vec<_>>();
    result.sort_by(|left, right| left.name.to_lowercase().cmp(&right.name.to_lowercase()));
    Ok(result)
}

pub fn delete_roi_profile(
    session: &ProjectSession,
    profile_id: &str,
) -> Result<bool, ApplicationError> {
    Ok(ProjectStore::open(session.database_path())?.delete_roi_profile(profile_id)?)
}

pub fn preview_source_tiles(
    session: &ProjectSession,
    source_id: &str,
    candidate_id: Option<&str>,
    limit: u32,
) -> Result<Vec<TilePreview>, ApplicationError> {
    let store = ProjectStore::open(session.database_path())?;
    let source = store
        .get_source(source_id)?
        .ok_or_else(|| ApplicationError::SourceNotFound(source_id.to_owned()))?;
    let input = resolve_preview_input(&store, &source, candidate_id)?;
    let image = media::load_oriented_image(&input.path)?;
    let profiles = list_effective_roi_profiles(session, source_id)?;
    if profiles.is_empty() {
        return Err(ApplicationError::NoRoiProfiles);
    }
    let preview_dir = session
        .project_dir()
        .join("cache")
        .join("tile-previews")
        .join(source_id)
        .join(input.candidate_id.as_deref().unwrap_or("source"));
    fs::create_dir_all(&preview_dir)?;
    let mut previews = Vec::new();
    for profile in profiles {
        profile.roi.validate(image.width(), image.height())?;
        for (placement, tile) in render_tiles(&image, profile.roi, profile.render_config)? {
            if previews.len() >= limit.min(1_000) as usize {
                return Ok(previews);
            }
            let file_name = format!(
                "{}-r{:04}-c{:04}.jpg",
                profile.id, placement.row, placement.column
            );
            let destination = preview_dir.join(file_name);
            if destination.is_file() {
                fs::remove_file(&destination)?;
            }
            write_bytes_atomic(&destination, &encode_image(&tile, ExportFormat::Jpeg)?)?;
            previews.push(TilePreview {
                source_id: source_id.to_owned(),
                candidate_id: input.candidate_id.clone(),
                roi_profile_id: profile.id.clone(),
                roi_name: profile.name.clone(),
                placement,
                preview_path: path_text(&destination),
            });
        }
    }
    Ok(previews)
}

pub fn plan_export(
    session: &ProjectSession,
    request: &ExportRequest,
) -> Result<ExportPlan, ApplicationError> {
    let output_dir = PathBuf::from(&request.output_dir);
    if output_dir.exists() && !output_dir.is_dir() {
        return Err(ApplicationError::InvalidExportDirectory(output_dir));
    }
    let template = NamingTemplate::parse(&request.naming_template)?;
    let store = ProjectStore::open(session.database_path())?;
    let selected_source = store
        .get_source(&request.source_id)?
        .ok_or_else(|| ApplicationError::SourceNotFound(request.source_id.clone()))?;
    let sources = resolve_export_sources(&store, &selected_source, request.source_scope)?;
    let manually_excluded = store
        .list_review_assets()?
        .into_iter()
        .filter(|asset| asset.manual_decision.as_deref() == Some("exclude"))
        .map(|asset| asset.asset_key)
        .collect::<HashSet<_>>();
    let mut occupied = existing_file_names(&output_dir)?;
    let mut items = Vec::new();
    let mut skipped = 0_u64;
    let mut index = 1_u64;
    let extension = request.format.extension();
    for source in sources {
        let candidate_id = if request.source_scope == ExportSourceScope::Current {
            request.candidate_id.as_deref()
        } else {
            None
        };
        let inputs = resolve_export_inputs(&store, &source, candidate_id)?;
        let profiles = if request.content == ExportContent::Tiles {
            list_effective_roi_profiles(session, &source.id)?
        } else {
            Vec::new()
        };
        if request.content == ExportContent::Tiles && profiles.is_empty() {
            return Err(ApplicationError::NoRoiProfiles);
        }
        for input in inputs {
            let review_key = input
                .candidate_id
                .as_ref()
                .map(|candidate_id| format!("candidate:{candidate_id}"))
                .unwrap_or_else(|| format!("source:{}", source.id));
            if manually_excluded.contains(&review_key) {
                skipped += if request.content == ExportContent::Frames {
                    1
                } else {
                    profiles.iter().try_fold(0_u64, |count, profile| {
                        profile.roi.validate(input.width, input.height)?;
                        Ok::<_, ApplicationError>(
                            count
                                + image_pipeline::plan_tiles(
                                    profile.roi,
                                    profile.render_config.tile,
                                )?
                                .len() as u64,
                        )
                    })?
                };
                continue;
            }
            if request.content == ExportContent::Frames {
                let placement = full_frame_placement(input.width, input.height);
                let stem = template.render(&NamingContext {
                    source: &source,
                    candidate: input.candidate.as_ref(),
                    roi_name: "frame",
                    placement,
                    index,
                })?;
                let stable_hash =
                    export_item_hash(&source, &input, request.content, None, placement, index);
                match resolve_file_name(
                    &stem,
                    extension,
                    &stable_hash,
                    request.conflict_strategy,
                    &mut occupied,
                )? {
                    Some(file_name) => items.push(ExportPlanItem {
                        source_id: source.id.clone(),
                        candidate_id: input.candidate_id.clone(),
                        content: request.content,
                        roi_profile_id: None,
                        roi_name: None,
                        placement,
                        file_name,
                    }),
                    None => skipped += 1,
                }
                index += 1;
                continue;
            }
            for profile in &profiles {
                profile.roi.validate(input.width, input.height)?;
                for placement in
                    image_pipeline::plan_tiles(profile.roi, profile.render_config.tile)?
                {
                    if request.excluded_tiles.iter().any(|excluded| {
                        excluded.source_id == source.id
                            && excluded.candidate_id == input.candidate_id
                            && excluded.roi_profile_id == profile.id
                            && excluded.row == placement.row
                            && excluded.column == placement.column
                    }) {
                        skipped += 1;
                        index += 1;
                        continue;
                    }
                    let stem = template.render(&NamingContext {
                        source: &source,
                        candidate: input.candidate.as_ref(),
                        roi_name: &profile.name,
                        placement,
                        index,
                    })?;
                    let stable_hash = export_item_hash(
                        &source,
                        &input,
                        request.content,
                        Some(&profile.id),
                        placement,
                        index,
                    );
                    match resolve_file_name(
                        &stem,
                        extension,
                        &stable_hash,
                        request.conflict_strategy,
                        &mut occupied,
                    )? {
                        Some(file_name) => items.push(ExportPlanItem {
                            source_id: source.id.clone(),
                            candidate_id: input.candidate_id.clone(),
                            content: request.content,
                            roi_profile_id: Some(profile.id.clone()),
                            roi_name: Some(profile.name.clone()),
                            placement,
                            file_name,
                        }),
                        None => skipped += 1,
                    }
                    index += 1;
                }
            }
        }
    }
    let estimated_bytes = items
        .iter()
        .map(|item| item.placement.output_width as u64 * item.placement.output_height as u64 * 3)
        .sum();
    Ok(ExportPlan {
        output_dir: path_text(&output_dir),
        items,
        skipped,
        estimated_bytes,
    })
}

pub fn run_export(
    session: &ProjectSession,
    request: &ExportRequest,
) -> Result<ExportResult, ApplicationError> {
    let plan = plan_export(session, request)?;
    let output_dir = PathBuf::from(&plan.output_dir);
    fs::create_dir_all(&output_dir)?;
    let store = ProjectStore::open(session.database_path())?;
    let mut sources = HashMap::<String, StoredSourceAsset>::new();
    let mut profiles = HashMap::<String, HashMap<String, RoiProfile>>::new();
    for item in &plan.items {
        if sources.contains_key(&item.source_id) {
            continue;
        }
        let source = store
            .get_source(&item.source_id)?
            .ok_or_else(|| ApplicationError::SourceNotFound(item.source_id.clone()))?;
        if item.content == ExportContent::Tiles {
            profiles.insert(
                item.source_id.clone(),
                list_effective_roi_profiles(session, &item.source_id)?
                    .into_iter()
                    .map(|profile| (profile.id.clone(), profile))
                    .collect(),
            );
        }
        sources.insert(item.source_id.clone(), source);
    }
    let export_id = Uuid::new_v4().to_string();
    let manifest_path = output_dir.join(format!("manifest-{export_id}.jsonl"));
    let manifest_temporary = manifest_path.with_extension("jsonl.tmp");
    let mut manifest = File::create(&manifest_temporary)?;
    let mut result = ExportResult {
        export_id,
        written: 0,
        skipped: plan.skipped,
        manifest_path: path_text(&manifest_path),
        failures: Vec::new(),
    };
    for item in plan.items {
        let attempt = (|| -> Result<(), ApplicationError> {
            let source = sources
                .get(&item.source_id)
                .ok_or_else(|| ApplicationError::SourceNotFound(item.source_id.clone()))?;
            let input = resolve_one_input(&store, source, item.candidate_id.as_deref())?;
            let image = media::load_oriented_image(&input.path)?;
            let profile = item
                .roi_profile_id
                .as_deref()
                .and_then(|profile_id| profiles.get(&item.source_id)?.get(profile_id));
            let rendered = match item.content {
                ExportContent::Frames => image,
                ExportContent::Tiles => {
                    let profile = profile.ok_or(ApplicationError::NoRoiProfiles)?;
                    render_tile(&image, profile.roi, item.placement, profile.render_config)?
                }
            };
            let encoded = encode_image(&rendered, request.format)?;
            let target = output_dir.join(&item.file_name);
            if target.exists() {
                return Err(ApplicationError::ExportConflict(target));
            }
            write_bytes_atomic(&target, &encoded)?;
            let content_hash = hex::encode(Sha256::digest(&encoded));
            let record = serde_json::json!({
                "schemaVersion": 1,
                "exportId": result.export_id,
                "fileName": item.file_name,
                "contentSha256": content_hash,
                "sourceId": source.id,
                "sourcePath": source.absolute_path,
                "candidateId": item.candidate_id,
                "videoOffsetMs": input.candidate.as_ref().map(|candidate| candidate.video_offset_ms),
                "exportContent": item.content,
                "roiProfileId": item.roi_profile_id,
                "roiName": item.roi_name,
                "roi": profile.map(|profile| profile.roi),
                "tile": (item.content == ExportContent::Tiles).then_some(item.placement),
                "renderConfig": profile.map(|profile| profile.render_config),
            });
            serde_json::to_writer(&mut manifest, &record)?;
            manifest.write_all(b"\n")?;
            Ok(())
        })();
        match attempt {
            Ok(()) => result.written += 1,
            Err(error) => result.failures.push(ImportFailure {
                path: item.file_name,
                error: error.to_string(),
            }),
        }
    }
    manifest.flush()?;
    fs::rename(&manifest_temporary, &manifest_path)?;
    Ok(result)
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

#[derive(Debug, Clone)]
struct ProcessingInput {
    candidate_id: Option<String>,
    candidate: Option<StoredCandidateImage>,
    path: PathBuf,
    width: u32,
    height: u32,
}

fn roi_profile_from_stored(
    stored: StoredRoiProfile,
    inherited: bool,
) -> Result<RoiProfile, ApplicationError> {
    Ok(RoiProfile {
        id: stored.id,
        scope: if stored.scope_kind == "source_group" {
            RoiScope::SourceGroup
        } else {
            RoiScope::Source
        },
        scope_value: stored.scope_value,
        name: stored.name,
        roi: Roi {
            x: stored.x,
            y: stored.y,
            width: stored.width,
            height: stored.height,
        },
        render_config: serde_json::from_str(&stored.render_config_json)?,
        inherited,
        updated_at: stored.updated_at,
    })
}

fn resolve_preview_input(
    store: &ProjectStore,
    source: &StoredSourceAsset,
    candidate_id: Option<&str>,
) -> Result<ProcessingInput, ApplicationError> {
    if source.kind == SourceKind::Image {
        return resolve_one_input(store, source, None);
    }
    if let Some(candidate_id) = candidate_id {
        return resolve_one_input(store, source, Some(candidate_id));
    }
    let candidate = store
        .list_candidates(&source.id, 0, 1)?
        .into_iter()
        .next()
        .ok_or(ApplicationError::NoCandidateImages)?;
    processing_input_from_candidate(source, candidate)
}

fn resolve_export_inputs(
    store: &ProjectStore,
    source: &StoredSourceAsset,
    candidate_id: Option<&str>,
) -> Result<Vec<ProcessingInput>, ApplicationError> {
    if source.kind == SourceKind::Image || candidate_id.is_some() {
        return Ok(vec![resolve_one_input(store, source, candidate_id)?]);
    }
    let candidates = store.list_candidates(&source.id, 0, 100_000)?;
    if candidates.is_empty() {
        return Err(ApplicationError::NoCandidateImages);
    }
    candidates
        .into_iter()
        .map(|candidate| processing_input_from_candidate(source, candidate))
        .collect()
}

fn resolve_export_sources(
    store: &ProjectStore,
    selected_source: &StoredSourceAsset,
    scope: ExportSourceScope,
) -> Result<Vec<StoredSourceAsset>, ApplicationError> {
    if scope == ExportSourceScope::Current {
        return Ok(vec![selected_source.clone()]);
    }
    Ok(store
        .list_sources(0, 1_000_000)?
        .into_iter()
        .filter(|source| {
            source.kind == selected_source.kind
                && source.source_group == selected_source.source_group
        })
        .collect())
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

#[derive(Debug)]
struct NamingTemplate {
    parts: Vec<NamingPart>,
}

#[derive(Debug)]
enum NamingPart {
    Text(String),
    Field(NamingField),
}

#[derive(Debug)]
enum NamingField {
    Source,
    SourceGroup,
    SourceIdentifier,
    TimestampMs,
    Roi,
    Row,
    Column,
    Width,
    Height,
    Index,
}

impl NamingTemplate {
    fn parse(value: &str) -> Result<Self, ApplicationError> {
        if value.trim().is_empty() {
            return Err(ApplicationError::InvalidNamingTemplate(
                "命名模板不能为空".to_owned(),
            ));
        }
        let mut parts = Vec::new();
        let mut rest = value;
        while let Some(open) = rest.find('{') {
            if open > 0 {
                parts.push(NamingPart::Text(rest[..open].to_owned()));
            }
            let after_open = &rest[open + 1..];
            let close = after_open.find('}').ok_or_else(|| {
                ApplicationError::InvalidNamingTemplate("缺少右花括号".to_owned())
            })?;
            let field = match &after_open[..close] {
                "source" => NamingField::Source,
                "source_group" => NamingField::SourceGroup,
                "source_identifier" => NamingField::SourceIdentifier,
                "timestamp_ms" => NamingField::TimestampMs,
                "roi" => NamingField::Roi,
                "row" => NamingField::Row,
                "col" => NamingField::Column,
                "width" => NamingField::Width,
                "height" => NamingField::Height,
                "index" => NamingField::Index,
                field => {
                    return Err(ApplicationError::InvalidNamingTemplate(format!(
                        "未知字段：{field}"
                    )));
                }
            };
            parts.push(NamingPart::Field(field));
            rest = &after_open[close + 1..];
        }
        if rest.contains('}') {
            return Err(ApplicationError::InvalidNamingTemplate(
                "存在未配对的右花括号".to_owned(),
            ));
        }
        if !rest.is_empty() {
            parts.push(NamingPart::Text(rest.to_owned()));
        }
        Ok(Self { parts })
    }

    fn render(&self, context: &NamingContext<'_>) -> Result<String, ApplicationError> {
        let mut result = String::new();
        for part in &self.parts {
            match part {
                NamingPart::Text(text) => result.push_str(text),
                NamingPart::Field(field) => match field {
                    NamingField::Source => result.push_str(
                        Path::new(&context.source.file_name)
                            .file_stem()
                            .unwrap_or_default()
                            .to_string_lossy()
                            .as_ref(),
                    ),
                    NamingField::SourceGroup => result.push_str(&context.source.source_group),
                    NamingField::SourceIdentifier => {
                        result.push_str(&context.source.source_identifier)
                    }
                    NamingField::TimestampMs => result.push_str(
                        &context
                            .candidate
                            .map(|candidate| candidate.video_offset_ms)
                            .unwrap_or(0)
                            .to_string(),
                    ),
                    NamingField::Roi => result.push_str(context.roi_name),
                    NamingField::Row => result.push_str(&(context.placement.row + 1).to_string()),
                    NamingField::Column => {
                        result.push_str(&(context.placement.column + 1).to_string())
                    }
                    NamingField::Width => {
                        result.push_str(&context.placement.output_width.to_string())
                    }
                    NamingField::Height => {
                        result.push_str(&context.placement.output_height.to_string())
                    }
                    NamingField::Index => result.push_str(&context.index.to_string()),
                },
            }
        }
        if result.is_empty() {
            return Err(ApplicationError::InvalidNamingTemplate(
                "模板结果为空".to_owned(),
            ));
        }
        Ok(result)
    }
}

struct NamingContext<'a> {
    source: &'a StoredSourceAsset,
    candidate: Option<&'a StoredCandidateImage>,
    roi_name: &'a str,
    placement: TilePlacement,
    index: u64,
}

fn existing_file_names(output_dir: &Path) -> Result<HashSet<String>, ApplicationError> {
    if !output_dir.exists() {
        return Ok(HashSet::new());
    }
    Ok(fs::read_dir(output_dir)?
        .filter_map(Result::ok)
        .filter(|entry| entry.path().is_file())
        .map(|entry| entry.file_name().to_string_lossy().to_ascii_lowercase())
        .collect())
}

fn export_item_hash(
    source: &StoredSourceAsset,
    input: &ProcessingInput,
    content: ExportContent,
    roi_profile_id: Option<&str>,
    placement: TilePlacement,
    index: u64,
) -> String {
    let mut hasher = Sha256::new();
    hasher.update(source.id.as_bytes());
    hasher.update(input.candidate_id.as_deref().unwrap_or("source").as_bytes());
    hasher.update(match content {
        ExportContent::Frames => b"frames".as_slice(),
        ExportContent::Tiles => b"tiles".as_slice(),
    });
    hasher.update(roi_profile_id.unwrap_or("full-frame").as_bytes());
    hasher.update(placement.row.to_le_bytes());
    hasher.update(placement.column.to_le_bytes());
    hasher.update(index.to_le_bytes());
    hex::encode(hasher.finalize())
}

fn full_frame_placement(width: u32, height: u32) -> TilePlacement {
    TilePlacement {
        row: 0,
        column: 0,
        source_x: 0,
        source_y: 0,
        source_width: width,
        source_height: height,
        output_width: width,
        output_height: height,
        padded: false,
    }
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
                excluded_tiles: Vec::new(),
            },
        )
        .unwrap();
        assert_eq!(plan.items.len(), 1);
        assert_eq!(plan.skipped, 1);
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
        let result = run_export(&session, &request).unwrap();
        assert_eq!(result.written, 4);
        assert!(result.failures.is_empty());
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
            let frame_path = root.join(format!("frame-{index}.png"));
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
            "format": "png",
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
