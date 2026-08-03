use std::{
    collections::HashMap,
    path::PathBuf,
    sync::{Arc, Mutex},
};

use application::{
    CandidateDeletionResult, CaptureResult, ChangeAnalysisResult, ComponentHealth, ExportPlan,
    ExportRequest, ExportResult, GroupSamplingEstimate, ImportProgress, ImportResult, M0Status,
    OperationControl, OperationProgress, OperationState, ProjectSession, ProjectSummary,
    ReviewAction, ReviewAnalysisConfig, ReviewWorkspace, RoiProfile, SamplingConfig,
    SamplingEstimate, SamplingExecutionResult, SaveRoiProfile, SourceDeletionProgress,
    SourceDeletionResult, TilePreview, VideoPreview, analyze_changes, capture_manual_frame,
    complete_pending_hashes_limited, create_video_selection,
    delete_candidates as delete_project_candidates,
    delete_roi_profile as delete_project_roi_profile,
    delete_sources_with_control as delete_project_sources_with_control, delete_video_selection,
    estimate_group_sampling, estimate_sampling, execute_group_sampling_with_control,
    execute_sampling_with_control, execute_video_preview,
    import_sources_with_control as import_project_sources_with_control,
    list_all_source_ids as list_project_source_ids, list_candidates as list_project_candidates,
    list_effective_roi_profiles, list_review_workspace_at as list_project_review_workspace_at,
    list_sources as list_project_sources, list_video_selections,
    plan_export as plan_project_export, plan_video_preview, preview_source_tiles,
    read_recent_project, redo_review_action as redo_project_review_action,
    refresh_source_status as refresh_project_source_status, refresh_source_statuses,
    relink_source as relink_project_source,
    run_export_with_control as run_project_export_with_control,
    run_review_analysis_at as run_project_review_analysis_at,
    save_roi_profile as save_project_roi_profile, undo_review_action as undo_project_review_action,
    update_review_items as update_project_review_items, video_frame_timestamps,
    write_recent_project,
};
use domain::{EdgeStrategy, JobState, Roi, TileConfig};
use image::{DynamicImage, ImageBuffer, Luma};
use image_pipeline::{deterministic_brightness, global_ssim, perceptual_hash, plan_tiles};
use job_engine::JobStateMachine;
use storage::{StoredCandidateImage, StoredSourceAsset, StoredVideoSelection};
use tauri::{AppHandle, Emitter, Manager, State};

#[derive(Clone, serde::Serialize)]
#[serde(rename_all = "camelCase")]
struct VideoPreviewProgress {
    source_id: String,
    percent: u8,
}

#[derive(Clone)]
struct AppState {
    session: Arc<Mutex<Option<ProjectSession>>>,
    project_database: Arc<Mutex<Option<PathBuf>>>,
    recent_config: PathBuf,
    operations: Arc<Mutex<HashMap<String, OperationControl>>>,
}

fn begin_operation(state: &AppState, kind: &str) -> Result<OperationControl, String> {
    let mut operations = state.operations.lock().map_err(lock_error)?;
    if let Some(active) = operations.keys().next() {
        return Err(format!("已有 {active} 长任务正在运行，请先完成或取消"));
    }
    let control = OperationControl::new();
    operations.insert(kind.to_owned(), control.clone());
    Ok(control)
}

fn finish_operation(state: &AppState, kind: &str) {
    if let Ok(mut operations) = state.operations.lock() {
        operations.remove(kind);
    }
}

#[tauri::command]
fn control_operation(
    state: State<'_, AppState>,
    kind: String,
    action: String,
) -> Result<String, String> {
    let operations = state.operations.lock().map_err(lock_error)?;
    let control = operations
        .get(&kind)
        .ok_or_else(|| "任务已经结束".to_owned())?;
    match action.as_str() {
        "pause" => {
            control.pause();
        }
        "resume" => {
            control.resume();
        }
        "cancel" => {
            control.cancel();
        }
        _ => return Err("未知任务控制命令".to_owned()),
    }
    Ok(match control.state() {
        OperationState::Running => "running",
        OperationState::Paused => "paused",
        OperationState::Cancelled => "cancelling",
    }
    .to_owned())
}

fn current_project_database(state: &AppState) -> Result<PathBuf, String> {
    state
        .project_database
        .lock()
        .map_err(lock_error)?
        .clone()
        .ok_or_else(no_project)
}

#[tauri::command]
fn get_m0_status() -> M0Status {
    let mut components = Vec::new();

    components.push(match storage::probe_in_memory() {
        Ok(probe) => ComponentHealth::ready(
            "sqlite",
            "SQLite 项目库",
            format!(
                "SQLite {}，迁移版本 {}",
                probe.sqlite_version, probe.schema_version
            ),
        ),
        Err(error) => ComponentHealth::blocked("sqlite", "SQLite 项目库", error.to_string()),
    });

    components.push(match media::probe_ffprobe("ffprobe") {
        Ok(version) if version.is_full_gpl_build => ComponentHealth::warning(
            "ffprobe",
            "FFmpeg / ffprobe",
            format!(
                "{}；当前为开发机 full/GPL 构建，发行前需替换",
                version.first_line
            ),
        ),
        Ok(version) => ComponentHealth::ready("ffprobe", "FFmpeg / ffprobe", version.first_line),
        Err(error) => ComponentHealth::blocked("ffprobe", "FFmpeg / ffprobe", error.to_string()),
    });

    let image = DynamicImage::ImageLuma8(ImageBuffer::from_fn(64, 64, |x, y| {
        Luma([((x * 3 + y * 5) % 256) as u8])
    }));
    let tile_result = plan_tiles(
        Roi {
            x: 0,
            y: 0,
            width: 1920,
            height: 1080,
        },
        TileConfig {
            tile_width: 640,
            tile_height: 640,
            overlap_x: 64,
            overlap_y: 64,
            edge_strategy: EdgeStrategy::ShiftToEdge,
        },
    );
    let augmentation_result = deterministic_brightness(&image, 20260722, -12, 12);
    components.push(match (tile_result, augmentation_result) {
        (Ok(tiles), Ok((augmented, offset))) => {
            let hash = perceptual_hash(&image);
            let similarity = global_ssim(&image, &augmented);
            ComponentHealth::ready(
                "image_pipeline",
                "Rust 图片流水线",
                format!(
                    "{} 个切片，pHash {hash:016x}，增强偏移 {offset}，SSIM {similarity:.4}",
                    tiles.len()
                ),
            )
        }
        (Err(error), _) | (_, Err(error)) => {
            ComponentHealth::blocked("image_pipeline", "Rust 图片流水线", error.to_string())
        }
    });

    let mut job = JobStateMachine::default();
    let job_probe = [JobState::Estimated, JobState::Queued, JobState::Running]
        .into_iter()
        .try_for_each(|state| job.transition(state));
    components.push(match job_probe {
        Ok(()) => ComponentHealth::ready(
            "job_engine",
            "任务状态机",
            format!("当前验证状态：{:?}", job.state()),
        ),
        Err(error) => ComponentHealth::blocked("job_engine", "任务状态机", error.to_string()),
    });

    components.push(ComponentHealth::ready(
        "webview2",
        "WebView2 界面运行时",
        "当前窗口已由 WebView2 成功加载",
    ));

    M0Status {
        app_version: env!("CARGO_PKG_VERSION").to_owned(),
        target: format!("{}-{}", std::env::consts::OS, std::env::consts::ARCH),
        components,
    }
}

#[tauri::command]
fn create_project(
    app: AppHandle,
    state: State<'_, AppState>,
    parent_dir: String,
    name: String,
) -> Result<ProjectSummary, String> {
    let session = ProjectSession::create(parent_dir, &name).map_err(error_text)?;
    session.backup_database().map_err(error_text)?;
    register_project_scope(&app, &session).map_err(error_text)?;
    let summary = session.summary.clone();
    let database_path = session.database_path();
    write_recent_project(&state.recent_config, &summary.path).map_err(error_text)?;
    *state.session.lock().map_err(lock_error)? = Some(session);
    *state.project_database.lock().map_err(lock_error)? = Some(database_path);
    Ok(summary)
}

#[tauri::command]
fn open_project(
    app: AppHandle,
    state: State<'_, AppState>,
    path: String,
) -> Result<ProjectSummary, String> {
    {
        let guard = state.session.lock().map_err(lock_error)?;
        if let Some(session) = guard.as_ref()
            && PathBuf::from(&path)
                .canonicalize()
                .is_ok_and(|candidate| candidate == session.project_dir())
        {
            return Ok(session.summary.clone());
        }
    }
    let session = ProjectSession::open(path).map_err(error_text)?;
    session.backup_database().map_err(error_text)?;
    register_project_scope(&app, &session).map_err(error_text)?;
    let summary = session.summary.clone();
    let database_path = session.database_path();
    write_recent_project(&state.recent_config, &summary.path).map_err(error_text)?;
    *state.session.lock().map_err(lock_error)? = Some(session);
    *state.project_database.lock().map_err(lock_error)? = Some(database_path);
    Ok(summary)
}

#[tauri::command]
fn close_project(state: State<'_, AppState>) -> Result<(), String> {
    *state.session.lock().map_err(lock_error)? = None;
    *state.project_database.lock().map_err(lock_error)? = None;
    Ok(())
}

#[tauri::command]
fn get_current_project(state: State<'_, AppState>) -> Result<Option<ProjectSummary>, String> {
    Ok(state
        .session
        .lock()
        .map_err(lock_error)?
        .as_ref()
        .map(|session| session.summary.clone()))
}

#[tauri::command]
fn get_recent_project(state: State<'_, AppState>) -> Result<Option<String>, String> {
    read_recent_project(&state.recent_config).map_err(error_text)
}

#[tauri::command]
async fn import_sources(
    app: AppHandle,
    state: State<'_, AppState>,
    paths: Vec<String>,
) -> Result<ImportResult, String> {
    let state = state.inner().clone();
    let control = begin_operation(&state, "import")?;
    let operation_state = state.clone();
    let (result, database_path) = tauri::async_runtime::spawn_blocking(move || {
        let outcome = (|| {
            let ffprobe = external_tool(&app, "ffprobe");
            let ffmpeg = external_tool(&app, "ffmpeg");
            let mut guard = state.session.lock().map_err(lock_error)?;
            let session = guard.as_mut().ok_or_else(no_project)?;
            let result = import_project_sources_with_control(
                session,
                &paths,
                &ffprobe,
                &ffmpeg,
                Some(&control),
                |progress: ImportProgress| {
                    let _ = app.emit("import-progress", progress);
                },
            )
            .map_err(error_text)?;
            register_import_scope(&app, &paths).map_err(error_text)?;
            register_project_scope(&app, session).map_err(error_text)?;
            Ok::<_, String>((result, session.database_path()))
        })();
        finish_operation(&operation_state, "import");
        outcome
    })
    .await
    .map_err(|error| error.to_string())??;
    if !result.cancelled {
        tauri::async_runtime::spawn_blocking(move || {
            let _ = complete_pending_hashes_limited(&database_path, 256);
        });
    }
    Ok(result)
}

#[tauri::command]
fn list_sources(
    app: AppHandle,
    state: State<'_, AppState>,
    offset: u32,
    limit: u32,
) -> Result<Vec<StoredSourceAsset>, String> {
    let guard = state.session.lock().map_err(lock_error)?;
    let session = guard.as_ref().ok_or_else(no_project)?;
    let sources = list_project_sources(session, offset, limit).map_err(error_text)?;
    register_asset_scope(&app, &sources).map_err(error_text)?;
    Ok(sources)
}

#[tauri::command]
fn list_all_source_ids(state: State<'_, AppState>) -> Result<Vec<String>, String> {
    let guard = state.session.lock().map_err(lock_error)?;
    let session = guard.as_ref().ok_or_else(no_project)?;
    list_project_source_ids(session).map_err(error_text)
}

#[tauri::command]
async fn refresh_source_status(state: State<'_, AppState>) -> Result<u64, String> {
    let state = state.inner().clone();
    tauri::async_runtime::spawn_blocking(move || {
        let mut guard = state.session.lock().map_err(lock_error)?;
        let session = guard.as_mut().ok_or_else(no_project)?;
        refresh_source_statuses(session).map_err(error_text)
    })
    .await
    .map_err(|error| error.to_string())?
}

#[tauri::command]
async fn check_source_status(
    state: State<'_, AppState>,
    source_id: String,
) -> Result<StoredSourceAsset, String> {
    let state = state.inner().clone();
    tauri::async_runtime::spawn_blocking(move || {
        let mut guard = state.session.lock().map_err(lock_error)?;
        let session = guard.as_mut().ok_or_else(no_project)?;
        refresh_project_source_status(session, &source_id).map_err(error_text)
    })
    .await
    .map_err(|error| error.to_string())?
}

#[tauri::command]
fn relink_source(
    app: AppHandle,
    state: State<'_, AppState>,
    source_id: String,
    new_path: String,
) -> Result<StoredSourceAsset, String> {
    let mut guard = state.session.lock().map_err(lock_error)?;
    let session = guard.as_mut().ok_or_else(no_project)?;
    let source = relink_project_source(session, &source_id, new_path).map_err(error_text)?;
    register_asset_scope(&app, std::slice::from_ref(&source)).map_err(error_text)?;
    Ok(source)
}

#[tauri::command]
async fn get_video_frame_timestamps(
    app: AppHandle,
    state: State<'_, AppState>,
    source_id: String,
) -> Result<Vec<u64>, String> {
    let state = state.inner().clone();
    tauri::async_runtime::spawn_blocking(move || {
        let ffprobe = external_tool(&app, "ffprobe");
        let mut guard = state.session.lock().map_err(lock_error)?;
        let session = guard.as_mut().ok_or_else(no_project)?;
        video_frame_timestamps(session, &source_id, &ffprobe).map_err(error_text)
    })
    .await
    .map_err(|error| error.to_string())?
}

#[tauri::command]
async fn prepare_video_preview(
    app: AppHandle,
    state: State<'_, AppState>,
    source_id: String,
    force_transcode: bool,
) -> Result<VideoPreview, String> {
    let plan = {
        let guard = state.session.lock().map_err(lock_error)?;
        let session = guard.as_ref().ok_or_else(no_project)?;
        plan_video_preview(session, &source_id, force_transcode).map_err(error_text)?
    };
    let ffmpeg = external_tool(&app, "ffmpeg");
    let progress_app = app.clone();
    let progress_source_id = source_id.clone();
    let preview = tauri::async_runtime::spawn_blocking(move || {
        execute_video_preview(plan, &ffmpeg, |percent| {
            let _ = progress_app.emit(
                "video-preview-progress",
                VideoPreviewProgress {
                    source_id: progress_source_id.clone(),
                    percent,
                },
            );
        })
        .map_err(error_text)
    })
    .await
    .map_err(|error| error.to_string())??;
    app.asset_protocol_scope()
        .allow_file(PathBuf::from(&preview.path))
        .map_err(error_text)?;
    Ok(preview)
}

#[tauri::command]
fn get_video_selections(
    state: State<'_, AppState>,
    source_id: String,
) -> Result<Vec<StoredVideoSelection>, String> {
    let guard = state.session.lock().map_err(lock_error)?;
    let session = guard.as_ref().ok_or_else(no_project)?;
    list_video_selections(session, &source_id).map_err(error_text)
}

#[tauri::command]
fn add_video_selection(
    state: State<'_, AppState>,
    source_id: String,
    start_ms: u64,
    end_ms: u64,
    protected: bool,
) -> Result<StoredVideoSelection, String> {
    let mut guard = state.session.lock().map_err(lock_error)?;
    let session = guard.as_mut().ok_or_else(no_project)?;
    create_video_selection(session, &source_id, start_ms, end_ms, protected).map_err(error_text)
}

#[tauri::command]
fn remove_video_selection(
    state: State<'_, AppState>,
    selection_id: String,
) -> Result<bool, String> {
    let guard = state.session.lock().map_err(lock_error)?;
    let session = guard.as_ref().ok_or_else(no_project)?;
    delete_video_selection(session, &selection_id).map_err(error_text)
}

#[tauri::command]
fn get_candidates(
    state: State<'_, AppState>,
    source_id: String,
    offset: u32,
    limit: u32,
) -> Result<Vec<StoredCandidateImage>, String> {
    let guard = state.session.lock().map_err(lock_error)?;
    let session = guard.as_ref().ok_or_else(no_project)?;
    list_project_candidates(session, &source_id, offset, limit).map_err(error_text)
}

#[tauri::command]
async fn remove_candidates(
    state: State<'_, AppState>,
    source_id: String,
    candidate_ids: Option<Vec<String>>,
) -> Result<CandidateDeletionResult, String> {
    let state = state.inner().clone();
    tauri::async_runtime::spawn_blocking(move || {
        let mut guard = state.session.lock().map_err(lock_error)?;
        let session = guard.as_mut().ok_or_else(no_project)?;
        delete_project_candidates(session, &source_id, candidate_ids.as_deref()).map_err(error_text)
    })
    .await
    .map_err(|error| error.to_string())?
}

#[tauri::command]
async fn remove_sources(
    app: AppHandle,
    state: State<'_, AppState>,
    source_ids: Vec<String>,
) -> Result<SourceDeletionResult, String> {
    let state = state.inner().clone();
    let control = begin_operation(&state, "source_removal")?;
    let operation_state = state.clone();
    tauri::async_runtime::spawn_blocking(move || {
        let outcome = (|| {
            let mut guard = state.session.lock().map_err(lock_error)?;
            let session = guard.as_mut().ok_or_else(no_project)?;
            delete_project_sources_with_control(
                session,
                &source_ids,
                Some(&control),
                |progress: SourceDeletionProgress| {
                    let _ = app.emit("source-removal-progress", progress);
                },
            )
            .map_err(error_text)
        })();
        finish_operation(&operation_state, "source_removal");
        outcome
    })
    .await
    .map_err(|error| error.to_string())?
}

#[tauri::command]
async fn run_review_analysis(
    state: State<'_, AppState>,
    config: ReviewAnalysisConfig,
) -> Result<ReviewWorkspace, String> {
    let database_path = current_project_database(state.inner())?;
    tauri::async_runtime::spawn_blocking(move || {
        run_project_review_analysis_at(database_path, &config).map_err(error_text)
    })
    .await
    .map_err(|error| error.to_string())?
}

#[tauri::command]
async fn get_review_workspace(state: State<'_, AppState>) -> Result<ReviewWorkspace, String> {
    let database_path = current_project_database(state.inner())?;
    tauri::async_runtime::spawn_blocking(move || {
        list_project_review_workspace_at(database_path).map_err(error_text)
    })
    .await
    .map_err(|error| error.to_string())?
}

#[tauri::command]
async fn update_review_items(
    state: State<'_, AppState>,
    asset_keys: Vec<String>,
    action: ReviewAction,
) -> Result<ReviewWorkspace, String> {
    let state = state.inner().clone();
    tauri::async_runtime::spawn_blocking(move || {
        let guard = state.session.lock().map_err(lock_error)?;
        let session = guard.as_ref().ok_or_else(no_project)?;
        update_project_review_items(session, &asset_keys, action).map_err(error_text)
    })
    .await
    .map_err(|error| error.to_string())?
}

#[tauri::command]
fn undo_review_action(state: State<'_, AppState>) -> Result<ReviewWorkspace, String> {
    let guard = state.session.lock().map_err(lock_error)?;
    let session = guard.as_ref().ok_or_else(no_project)?;
    undo_project_review_action(session).map_err(error_text)
}

#[tauri::command]
fn redo_review_action(state: State<'_, AppState>) -> Result<ReviewWorkspace, String> {
    let guard = state.session.lock().map_err(lock_error)?;
    let session = guard.as_ref().ok_or_else(no_project)?;
    redo_project_review_action(session).map_err(error_text)
}

#[tauri::command]
async fn capture_video_frame(
    app: AppHandle,
    state: State<'_, AppState>,
    source_id: String,
    timestamp_ms: u64,
) -> Result<CaptureResult, String> {
    let state = state.inner().clone();
    tauri::async_runtime::spawn_blocking(move || {
        let ffprobe = external_tool(&app, "ffprobe");
        let ffmpeg = external_tool(&app, "ffmpeg");
        let mut guard = state.session.lock().map_err(lock_error)?;
        let session = guard.as_mut().ok_or_else(no_project)?;
        capture_manual_frame(session, &source_id, timestamp_ms, &ffprobe, &ffmpeg)
            .map_err(error_text)
    })
    .await
    .map_err(|error| error.to_string())?
}

#[tauri::command]
async fn estimate_video_sampling(
    app: AppHandle,
    state: State<'_, AppState>,
    source_id: String,
    config: SamplingConfig,
) -> Result<SamplingEstimate, String> {
    let state = state.inner().clone();
    tauri::async_runtime::spawn_blocking(move || {
        let ffprobe = external_tool(&app, "ffprobe");
        let mut guard = state.session.lock().map_err(lock_error)?;
        let session = guard.as_mut().ok_or_else(no_project)?;
        estimate_sampling(session, &source_id, &config, &ffprobe).map_err(error_text)
    })
    .await
    .map_err(|error| error.to_string())?
}

#[tauri::command]
async fn run_video_sampling(
    app: AppHandle,
    state: State<'_, AppState>,
    source_id: String,
    config: SamplingConfig,
) -> Result<SamplingExecutionResult, String> {
    let state = state.inner().clone();
    let control = begin_operation(&state, "sampling")?;
    let operation_state = state.clone();
    tauri::async_runtime::spawn_blocking(move || {
        let outcome = (|| {
            let ffprobe = external_tool(&app, "ffprobe");
            let ffmpeg = external_tool(&app, "ffmpeg");
            let mut guard = state.session.lock().map_err(lock_error)?;
            let session = guard.as_mut().ok_or_else(no_project)?;
            execute_sampling_with_control(
                session,
                &source_id,
                &config,
                &ffprobe,
                &ffmpeg,
                Some(&control),
                |progress: OperationProgress| {
                    let _ = app.emit("sampling-progress", progress);
                },
            )
            .map_err(error_text)
        })();
        finish_operation(&operation_state, "sampling");
        outcome
    })
    .await
    .map_err(|error| error.to_string())?
}

#[tauri::command]
async fn analyze_video_changes(
    app: AppHandle,
    state: State<'_, AppState>,
    source_id: String,
    analysis_fps: f64,
    threshold: f64,
    min_interval_ms: u64,
    max_interval_ms: u64,
) -> Result<ChangeAnalysisResult, String> {
    let state = state.inner().clone();
    tauri::async_runtime::spawn_blocking(move || {
        let ffmpeg = external_tool(&app, "ffmpeg");
        let mut guard = state.session.lock().map_err(lock_error)?;
        let session = guard.as_mut().ok_or_else(no_project)?;
        analyze_changes(
            session,
            &source_id,
            analysis_fps,
            threshold,
            min_interval_ms,
            max_interval_ms,
            &ffmpeg,
        )
        .map_err(error_text)
    })
    .await
    .map_err(|error| error.to_string())?
}

#[tauri::command]
async fn estimate_source_group_sampling(
    app: AppHandle,
    state: State<'_, AppState>,
    source_group: String,
    config: SamplingConfig,
) -> Result<GroupSamplingEstimate, String> {
    let state = state.inner().clone();
    tauri::async_runtime::spawn_blocking(move || {
        let ffprobe = external_tool(&app, "ffprobe");
        let mut guard = state.session.lock().map_err(lock_error)?;
        let session = guard.as_mut().ok_or_else(no_project)?;
        estimate_group_sampling(session, &source_group, &config, &ffprobe).map_err(error_text)
    })
    .await
    .map_err(|error| error.to_string())?
}

#[tauri::command]
async fn run_source_group_sampling(
    app: AppHandle,
    state: State<'_, AppState>,
    source_group: String,
    config: SamplingConfig,
) -> Result<SamplingExecutionResult, String> {
    let state = state.inner().clone();
    let control = begin_operation(&state, "sampling")?;
    let operation_state = state.clone();
    tauri::async_runtime::spawn_blocking(move || {
        let outcome = (|| {
            let ffprobe = external_tool(&app, "ffprobe");
            let ffmpeg = external_tool(&app, "ffmpeg");
            let mut guard = state.session.lock().map_err(lock_error)?;
            let session = guard.as_mut().ok_or_else(no_project)?;
            execute_group_sampling_with_control(
                session,
                &source_group,
                &config,
                &ffprobe,
                &ffmpeg,
                Some(&control),
                |progress: OperationProgress| {
                    let _ = app.emit("sampling-progress", progress);
                },
            )
            .map_err(error_text)
        })();
        finish_operation(&operation_state, "sampling");
        outcome
    })
    .await
    .map_err(|error| error.to_string())?
}

#[tauri::command]
fn save_roi_profile(
    state: State<'_, AppState>,
    draft: SaveRoiProfile,
) -> Result<RoiProfile, String> {
    let guard = state.session.lock().map_err(lock_error)?;
    let session = guard.as_ref().ok_or_else(no_project)?;
    save_project_roi_profile(session, draft).map_err(error_text)
}

#[tauri::command]
fn get_roi_profiles(
    state: State<'_, AppState>,
    source_id: String,
) -> Result<Vec<RoiProfile>, String> {
    let guard = state.session.lock().map_err(lock_error)?;
    let session = guard.as_ref().ok_or_else(no_project)?;
    list_effective_roi_profiles(session, &source_id).map_err(error_text)
}

#[tauri::command]
fn delete_roi_profile(state: State<'_, AppState>, profile_id: String) -> Result<bool, String> {
    let guard = state.session.lock().map_err(lock_error)?;
    let session = guard.as_ref().ok_or_else(no_project)?;
    delete_project_roi_profile(session, &profile_id).map_err(error_text)
}

#[tauri::command]
async fn preview_tiles(
    app: AppHandle,
    state: State<'_, AppState>,
    source_id: String,
    candidate_id: Option<String>,
    limit: u32,
) -> Result<Vec<TilePreview>, String> {
    let state = state.inner().clone();
    let previews = tauri::async_runtime::spawn_blocking(move || {
        let guard = state.session.lock().map_err(lock_error)?;
        let session = guard.as_ref().ok_or_else(no_project)?;
        preview_source_tiles(session, &source_id, candidate_id.as_deref(), limit)
            .map_err(error_text)
    })
    .await
    .map_err(|error| error.to_string())??;
    for preview in &previews {
        let path = PathBuf::from(&preview.preview_path);
        if path.is_file() {
            app.asset_protocol_scope()
                .allow_file(path)
                .map_err(error_text)?;
        }
    }
    Ok(previews)
}

#[tauri::command]
fn plan_export(state: State<'_, AppState>, request: ExportRequest) -> Result<ExportPlan, String> {
    let guard = state.session.lock().map_err(lock_error)?;
    let session = guard.as_ref().ok_or_else(no_project)?;
    plan_project_export(session, &request).map_err(error_text)
}

#[tauri::command]
async fn run_export(
    app: AppHandle,
    state: State<'_, AppState>,
    request: ExportRequest,
) -> Result<ExportResult, String> {
    let state = state.inner().clone();
    let control = begin_operation(&state, "export")?;
    let operation_state = state.clone();
    tauri::async_runtime::spawn_blocking(move || {
        let outcome = (|| {
            let guard = state.session.lock().map_err(lock_error)?;
            let session = guard.as_ref().ok_or_else(no_project)?;
            run_project_export_with_control(
                session,
                &request,
                Some(&control),
                |progress: OperationProgress| {
                    let _ = app.emit("export-progress", progress);
                },
            )
            .map_err(error_text)
        })();
        finish_operation(&operation_state, "export");
        outcome
    })
    .await
    .map_err(|error| error.to_string())?
}

fn register_project_scope(app: &AppHandle, session: &ProjectSession) -> tauri::Result<()> {
    app.asset_protocol_scope()
        .allow_directory(session.project_dir().join("cache"), true)?;
    let sources = list_project_sources(session, 0, 100_000).unwrap_or_default();
    register_asset_scope(app, &sources)
}

fn register_import_scope(app: &AppHandle, paths: &[String]) -> tauri::Result<()> {
    let scope = app.asset_protocol_scope();
    for path in paths {
        let path = PathBuf::from(path);
        if path.is_dir() {
            scope.allow_directory(path, true)?;
        } else if path.is_file() {
            scope.allow_file(path)?;
        }
    }
    Ok(())
}

fn register_asset_scope(app: &AppHandle, sources: &[StoredSourceAsset]) -> tauri::Result<()> {
    let scope = app.asset_protocol_scope();
    let mut source_directories = std::collections::HashSet::new();
    for source in sources {
        let path = PathBuf::from(&source.absolute_path);
        if let Some(parent) = path.parent() {
            source_directories.insert(parent.to_path_buf());
        }
    }
    for directory in source_directories {
        if directory.is_dir() {
            scope.allow_directory(directory, false)?;
        }
    }
    Ok(())
}

fn external_tool(app: &AppHandle, name: &str) -> PathBuf {
    let executable = format!("{name}.exe");
    app.path()
        .resource_dir()
        .ok()
        .map(|directory| directory.join("ffmpeg").join("bin").join(&executable))
        .filter(|path| path.is_file())
        .unwrap_or_else(|| PathBuf::from(name))
}

fn error_text(error: impl std::fmt::Display) -> String {
    error.to_string()
}

fn lock_error<T>(_: std::sync::PoisonError<T>) -> String {
    "应用项目状态不可用".to_owned()
}

fn no_project() -> String {
    "请先创建或打开项目".to_owned()
}

#[cfg(test)]
#[allow(clippy::items_after_test_module)]
mod tests {
    use super::*;

    #[test]
    fn review_database_lookup_does_not_wait_for_long_session_operation() {
        let database_path = PathBuf::from(r"C:\free-train-test\project.sqlite");
        let state = AppState {
            session: Arc::new(Mutex::new(None)),
            project_database: Arc::new(Mutex::new(Some(database_path.clone()))),
            recent_config: PathBuf::new(),
            operations: Arc::new(Mutex::new(HashMap::new())),
        };
        let _session_guard = state.session.lock().unwrap();

        assert_eq!(current_project_database(&state).unwrap(), database_path);
    }

    #[test]
    fn operation_control_does_not_wait_for_the_project_session_lock() {
        let state = AppState {
            session: Arc::new(Mutex::new(None)),
            project_database: Arc::new(Mutex::new(None)),
            recent_config: PathBuf::new(),
            operations: Arc::new(Mutex::new(HashMap::new())),
        };
        let control = begin_operation(&state, "import").unwrap();
        let _session_guard = state.session.lock().unwrap();

        assert!(control.pause());
        assert_eq!(control.state(), OperationState::Paused);
        assert!(control.cancel());
        assert_eq!(control.state(), OperationState::Cancelled);
    }
}

#[cfg_attr(mobile, tauri::mobile_entry_point)]
pub fn run() {
    tauri::Builder::default()
        .plugin(tauri_plugin_dialog::init())
        .setup(|app| {
            let recent_config = app.path().app_config_dir()?.join("recent-project.json");
            app.manage(AppState {
                session: Arc::new(Mutex::new(None)),
                project_database: Arc::new(Mutex::new(None)),
                recent_config,
                operations: Arc::new(Mutex::new(HashMap::new())),
            });
            Ok(())
        })
        .invoke_handler(tauri::generate_handler![
            get_m0_status,
            create_project,
            open_project,
            close_project,
            get_current_project,
            get_recent_project,
            import_sources,
            control_operation,
            list_sources,
            list_all_source_ids,
            refresh_source_status,
            check_source_status,
            relink_source,
            get_video_frame_timestamps,
            prepare_video_preview,
            get_video_selections,
            add_video_selection,
            remove_video_selection,
            get_candidates,
            remove_candidates,
            remove_sources,
            run_review_analysis,
            get_review_workspace,
            update_review_items,
            undo_review_action,
            redo_review_action,
            capture_video_frame,
            estimate_video_sampling,
            run_video_sampling,
            analyze_video_changes,
            estimate_source_group_sampling,
            run_source_group_sampling,
            save_roi_profile,
            get_roi_profiles,
            delete_roi_profile,
            preview_tiles,
            plan_export,
            run_export,
        ])
        .run(tauri::generate_context!())
        .expect("Free-Train desktop application failed to start");
}
