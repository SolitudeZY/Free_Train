use super::*;
use std::collections::BTreeMap;

const CANDIDATE_TIMESTAMP_TOLERANCE_MS: u64 = 1;

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

pub fn capture_manual_frame(
    session: &mut ProjectSession,
    source_id: &str,
    requested_timestamp_ms: u64,
    _ffprobe: &Path,
    ffmpeg: &Path,
) -> Result<CaptureResult, ApplicationError> {
    let source = require_online_video(session, source_id)?;
    let duration_ms = source
        .duration_ms
        .ok_or(ApplicationError::MissingDuration)?;
    let timestamp_ms = requested_timestamp_ms.min(duration_ms.saturating_sub(1));
    let result = create_candidate(
        session,
        CandidateRequest {
            source: &source,
            timestamp_ms,
            source_frame_number: None,
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
    _ffprobe: &Path,
) -> Result<SamplingEstimate, ApplicationError> {
    let source = require_online_video(session, source_id)?;
    let duration = source
        .duration_ms
        .ok_or(ApplicationError::MissingDuration)?;
    let ranges = ProjectStore::open(session.database_path())?.list_video_selections(source_id)?;
    let timestamps_ms = plan_source_sampling_times(&source, duration, &ranges, config)?;
    let known_candidates = ProjectStore::open(session.database_path())?
        .list_candidates(source_id, 0, 100_000)?
        .into_iter()
        .map(|candidate| (candidate.video_offset_ms, candidate))
        .collect::<BTreeMap<_, _>>();
    let existing_count = timestamps_ms
        .iter()
        .filter(|timestamp| find_candidate_near(&known_candidates, **timestamp).is_some())
        .count() as u64;
    Ok(SamplingEstimate {
        estimated_count: timestamps_ms.len() as u64,
        existing_count,
        estimated_new_count: timestamps_ms.len() as u64 - existing_count,
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
    execute_sampling_with_progress(session, source_id, config, ffprobe, ffmpeg, |_| {})
}

pub fn execute_sampling_with_progress<F>(
    session: &mut ProjectSession,
    source_id: &str,
    config: &SamplingConfig,
    _ffprobe: &Path,
    ffmpeg: &Path,
    report_progress: F,
) -> Result<SamplingExecutionResult, ApplicationError>
where
    F: FnMut(OperationProgress),
{
    execute_sampling_with_control(
        session,
        source_id,
        config,
        _ffprobe,
        ffmpeg,
        None,
        report_progress,
    )
}

pub fn execute_sampling_with_control<F>(
    session: &mut ProjectSession,
    source_id: &str,
    config: &SamplingConfig,
    _ffprobe: &Path,
    ffmpeg: &Path,
    control: Option<&OperationControl>,
    mut report_progress: F,
) -> Result<SamplingExecutionResult, ApplicationError>
where
    F: FnMut(OperationProgress),
{
    let source = require_online_video(session, source_id)?;
    let duration = source
        .duration_ms
        .ok_or(ApplicationError::MissingDuration)?;
    let ranges = ProjectStore::open(session.database_path())?.list_video_selections(source_id)?;
    let timestamps = plan_source_sampling_times(&source, duration, &ranges, config)?;
    let parameters = serde_json::to_string(config)?;
    let method = sampling_method_text(config.mode);
    let mut result = SamplingExecutionResult {
        planned: timestamps.len() as u64,
        created: 0,
        existing: 0,
        failures: Vec::new(),
        cancelled: false,
    };
    if let Some(batch_result) = try_execute_sampling_batch(
        session,
        &source,
        config,
        &ranges,
        &timestamps,
        &parameters,
        method,
        ffmpeg,
        control,
        &mut report_progress,
    )? {
        return Ok(batch_result);
    }
    report_progress(sampling_progress(&result, 0));
    for (planned_index, timestamp_ms) in timestamps.into_iter().enumerate() {
        if control.is_some_and(|control| !control.checkpoint()) {
            result.cancelled = true;
            break;
        }
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
                timestamp_ms,
                source_frame_number: (config.mode == SamplingMode::FrameInterval)
                    .then_some(planned_index as u64 * config.frame_interval),
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
        report_progress(sampling_progress(&result, planned_index as u64 + 1));
    }
    session.refresh_summary()?;
    Ok(result)
}

#[allow(clippy::too_many_arguments)]
fn try_execute_sampling_batch<F>(
    session: &mut ProjectSession,
    source: &StoredSourceAsset,
    config: &SamplingConfig,
    ranges: &[StoredVideoSelection],
    timestamps: &[u64],
    parameters: &str,
    method: &str,
    ffmpeg: &Path,
    control: Option<&OperationControl>,
    report_progress: &mut F,
) -> Result<Option<SamplingExecutionResult>, ApplicationError>
where
    F: FnMut(OperationProgress),
{
    if timestamps.len() < 8
        || !matches!(
            config.mode,
            SamplingMode::FixedInterval | SamplingMode::FrameInterval
        )
    {
        return Ok(None);
    }
    let filter = match config.mode {
        SamplingMode::FixedInterval => {
            format!("fps={:.12}", 1_000.0 / config.interval_ms as f64)
        }
        SamplingMode::FrameInterval => {
            format!("select=not(mod(n\\,{}))", config.frame_interval)
        }
        _ => return Ok(None),
    };
    let batch_dir = session
        .project_dir()
        .join("cache")
        .join("sampling-batches")
        .join(Uuid::new_v4().to_string());
    let total = timestamps.len() as u64;
    report_progress(OperationProgress {
        phase: "正在顺序解码视频".to_owned(),
        completed: 0,
        total,
        succeeded: 0,
        existing: 0,
        failed: 0,
    });
    let extracted = loop {
        let attempt = media::extract_video_frames_batch_controlled(
            ffmpeg,
            &source.absolute_path,
            &filter,
            &batch_dir,
            source
                .duration_ms
                .ok_or(ApplicationError::MissingDuration)?,
            |percent| {
                report_progress(OperationProgress {
                    phase: "正在顺序解码视频".to_owned(),
                    completed: total.saturating_mul(percent as u64).saturating_mul(80) / 10_000,
                    total,
                    succeeded: 0,
                    existing: 0,
                    failed: 0,
                });
                control.is_none_or(|control| control.state() == OperationState::Running)
            },
        );
        match attempt {
            Ok(paths) if paths.len() == timestamps.len() => break paths,
            Err(media::MediaError::Cancelled)
                if control.is_some_and(|control| control.state() == OperationState::Paused) =>
            {
                report_progress(OperationProgress {
                    phase: "抽帧已暂停；继续后重新开始当前解码批次".to_owned(),
                    completed: 0,
                    total,
                    succeeded: 0,
                    existing: 0,
                    failed: 0,
                });
                if control.is_some_and(|control| !control.checkpoint()) {
                    return Ok(Some(cancelled_sampling_result(total, report_progress)));
                }
            }
            Err(media::MediaError::Cancelled) => {
                return Ok(Some(cancelled_sampling_result(total, report_progress)));
            }
            Ok(_) | Err(_) => {
                let _ = fs::remove_dir_all(&batch_dir);
                report_progress(OperationProgress {
                    phase: "批量解码不可用，正在精确逐帧抽取".to_owned(),
                    completed: 0,
                    total,
                    succeeded: 0,
                    existing: 0,
                    failed: 0,
                });
                return Ok(None);
            }
        }
    };

    let store = ProjectStore::open(session.database_path())?;
    let mut known_candidates = store
        .list_candidates(&source.id, 0, 100_000)?
        .into_iter()
        .map(|candidate| (candidate.video_offset_ms, candidate))
        .collect::<BTreeMap<_, _>>();
    let image_dir = session
        .project_dir()
        .join("cache")
        .join("candidates")
        .join(&source.id);
    let thumbnail_dir = session
        .project_dir()
        .join("cache")
        .join("candidate-thumbnails")
        .join(&source.id);
    fs::create_dir_all(&image_dir)?;
    fs::create_dir_all(&thumbnail_dir)?;
    let mut result = SamplingExecutionResult {
        planned: total,
        created: 0,
        existing: 0,
        failures: Vec::new(),
        cancelled: false,
    };
    for (index, (timestamp_ms, extracted_path)) in
        timestamps.iter().copied().zip(extracted).enumerate()
    {
        if control.is_some_and(|control| !control.checkpoint()) {
            result.cancelled = true;
            break;
        }
        let pinned = config.pin_results
            || ranges.iter().any(|selection| {
                selection.protected
                    && timestamp_ms >= selection.start_ms
                    && timestamp_ms < selection.end_ms
            });
        let attempt = (|| -> Result<bool, ApplicationError> {
            if let Some(candidate) = find_candidate_near(&known_candidates, timestamp_ms).cloned() {
                if pinned && !candidate.pinned {
                    let mut updated = candidate;
                    updated.pinned = true;
                    updated.selection_method = method.to_owned();
                    updated.parameters_json = parameters.to_owned();
                    store.upsert_candidate(&updated)?;
                    known_candidates.insert(updated.video_offset_ms, updated);
                }
                let _ = fs::remove_file(&extracted_path);
                return Ok(false);
            }
            let id = Uuid::new_v4().to_string();
            let image_path = image_dir.join(format!("{id}.jpg"));
            let thumbnail_path = thumbnail_dir.join(format!("{id}.jpg"));
            let thumbnail_temporary = thumbnail_dir.join(format!("{id}.tmp.jpg"));
            fs::rename(&extracted_path, &image_path)?;
            if let Err(error) = media::create_image_thumbnail(&image_path, &thumbnail_temporary) {
                let _ = fs::remove_file(&thumbnail_temporary);
                let _ = fs::remove_file(&image_path);
                return Err(error.into());
            }
            fs::rename(&thumbnail_temporary, &thumbnail_path)?;
            let info = media::inspect_image(&image_path)?;
            let candidate = StoredCandidateImage {
                id,
                source_id: source.id.clone(),
                video_offset_ms: timestamp_ms,
                source_frame_number: (config.mode == SamplingMode::FrameInterval)
                    .then_some(index as u64 * config.frame_interval),
                selection_method: method.to_owned(),
                parameters_json: parameters.to_owned(),
                image_path: path_text(&image_path),
                thumbnail_path: path_text(&thumbnail_path),
                width: info.width,
                height: info.height,
                pinned,
                created_at: Utc::now().to_rfc3339(),
            };
            store.upsert_candidate(&candidate)?;
            known_candidates.insert(timestamp_ms, candidate);
            Ok(true)
        })();
        match attempt {
            Ok(true) => result.created += 1,
            Ok(false) => result.existing += 1,
            Err(error) => result.failures.push(ImportFailure {
                path: format!("{} @ {timestamp_ms} ms", source.file_name),
                error: error.to_string(),
            }),
        }
        let postprocess = (index as u64 + 1).saturating_mul(20) / 100;
        report_progress(OperationProgress {
            phase: "正在写入候选图片与缩略图".to_owned(),
            completed: (total.saturating_mul(80) / 100 + postprocess).min(total),
            total,
            succeeded: result.created,
            existing: result.existing,
            failed: result.failures.len() as u64,
        });
    }
    let _ = fs::remove_dir_all(&batch_dir);
    session.refresh_summary()?;
    if result.cancelled {
        report_progress(OperationProgress {
            phase: "抽帧已取消".to_owned(),
            completed: (result.created + result.existing + result.failures.len() as u64).min(total),
            total,
            succeeded: result.created,
            existing: result.existing,
            failed: result.failures.len() as u64,
        });
    } else {
        report_progress(sampling_progress(&result, total));
    }
    Ok(Some(result))
}

fn cancelled_sampling_result<F>(total: u64, report_progress: &mut F) -> SamplingExecutionResult
where
    F: FnMut(OperationProgress),
{
    let result = SamplingExecutionResult {
        planned: total,
        created: 0,
        existing: 0,
        failures: Vec::new(),
        cancelled: true,
    };
    report_progress(OperationProgress {
        phase: "抽帧已取消".to_owned(),
        completed: 0,
        total,
        succeeded: 0,
        existing: 0,
        failed: 0,
    });
    result
}

fn sampling_progress(result: &SamplingExecutionResult, completed: u64) -> OperationProgress {
    OperationProgress {
        phase: "正在逐帧生成候选图片".to_owned(),
        completed,
        total: result.planned,
        succeeded: result.created,
        existing: result.existing,
        failed: result.failures.len() as u64,
    }
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
    let mut existing_count = 0_u64;
    for source in &sources {
        if let Ok(estimate) = estimate_sampling(session, &source.id, config, ffprobe) {
            estimated_count += estimate.estimated_count;
            existing_count += estimate.existing_count;
        }
    }
    Ok(GroupSamplingEstimate {
        source_count: sources.len() as u64,
        estimated_count,
        existing_count,
        estimated_new_count: estimated_count.saturating_sub(existing_count),
    })
}

pub fn execute_group_sampling(
    session: &mut ProjectSession,
    source_group: &str,
    config: &SamplingConfig,
    ffprobe: &Path,
    ffmpeg: &Path,
) -> Result<SamplingExecutionResult, ApplicationError> {
    execute_group_sampling_with_progress(session, source_group, config, ffprobe, ffmpeg, |_| {})
}

pub fn execute_group_sampling_with_progress<F>(
    session: &mut ProjectSession,
    source_group: &str,
    config: &SamplingConfig,
    ffprobe: &Path,
    ffmpeg: &Path,
    report_progress: F,
) -> Result<SamplingExecutionResult, ApplicationError>
where
    F: FnMut(OperationProgress),
{
    execute_group_sampling_with_control(
        session,
        source_group,
        config,
        ffprobe,
        ffmpeg,
        None,
        report_progress,
    )
}

pub fn execute_group_sampling_with_control<F>(
    session: &mut ProjectSession,
    source_group: &str,
    config: &SamplingConfig,
    ffprobe: &Path,
    ffmpeg: &Path,
    control: Option<&OperationControl>,
    mut report_progress: F,
) -> Result<SamplingExecutionResult, ApplicationError>
where
    F: FnMut(OperationProgress),
{
    validate_group_sampling_mode(config.mode)?;
    let sources = ProjectStore::open(session.database_path())?
        .list_sources(0, 1_000_000)?
        .into_iter()
        .filter(|source| source.kind == SourceKind::Video && source.source_group == source_group)
        .collect::<Vec<_>>();
    let work = sources
        .into_iter()
        .map(|source| {
            let count = estimate_sampling(session, &source.id, config, ffprobe)
                .map(|estimate| estimate.estimated_count)
                .unwrap_or(0);
            (source, count)
        })
        .collect::<Vec<_>>();
    let total = work.iter().map(|(_, count)| *count).sum();
    let mut aggregate = SamplingExecutionResult {
        planned: 0,
        created: 0,
        existing: 0,
        failures: Vec::new(),
        cancelled: false,
    };
    let mut completed = 0_u64;
    report_progress(OperationProgress {
        phase: "正在执行来源组抽帧".to_owned(),
        completed,
        total,
        succeeded: 0,
        existing: 0,
        failed: 0,
    });
    for (source, source_total) in work {
        if control.is_some_and(|control| !control.checkpoint()) {
            aggregate.cancelled = true;
            break;
        }
        let base_completed = completed;
        let base_created = aggregate.created;
        let base_existing = aggregate.existing;
        let base_failed = aggregate.failures.len() as u64;
        match execute_sampling_with_control(
            session,
            &source.id,
            config,
            ffprobe,
            ffmpeg,
            control,
            |progress| {
                report_progress(OperationProgress {
                    phase: format!("正在抽帧：{}", source.file_name),
                    completed: (base_completed + progress.completed).min(total),
                    total,
                    succeeded: base_created + progress.succeeded,
                    existing: base_existing + progress.existing,
                    failed: base_failed + progress.failed,
                });
            },
        ) {
            Ok(result) => {
                aggregate.planned += result.planned;
                aggregate.created += result.created;
                aggregate.existing += result.existing;
                aggregate.failures.extend(result.failures);
                aggregate.cancelled |= result.cancelled;
            }
            Err(error) => aggregate.failures.push(ImportFailure {
                path: source.absolute_path,
                error: error.to_string(),
            }),
        }
        if aggregate.cancelled {
            break;
        }
        completed = completed.saturating_add(source_total).min(total);
    }
    report_progress(OperationProgress {
        phase: if aggregate.cancelled {
            "来源组抽帧已取消"
        } else {
            "来源组抽帧完成"
        }
        .to_owned(),
        completed: if aggregate.cancelled {
            completed
        } else {
            total
        },
        total,
        succeeded: aggregate.created,
        existing: aggregate.existing,
        failed: aggregate.failures.len() as u64,
    });
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

fn plan_source_sampling_times(
    source: &StoredSourceAsset,
    duration_ms: u64,
    selections: &[StoredVideoSelection],
    config: &SamplingConfig,
) -> Result<Vec<u64>, ApplicationError> {
    if config.mode == SamplingMode::FrameInterval {
        return plan_nominal_frame_interval_times(
            duration_ms,
            source.frame_rate.as_deref(),
            config.frame_interval,
        );
    }
    plan_sampling_times(&[], duration_ms, selections, config)
}

fn plan_nominal_frame_interval_times(
    duration_ms: u64,
    frame_rate: Option<&str>,
    frame_interval: u64,
) -> Result<Vec<u64>, ApplicationError> {
    if duration_ms == 0 || frame_interval == 0 {
        return Err(ApplicationError::InvalidSamplingConfiguration);
    }
    let value = frame_rate.ok_or(ApplicationError::MissingFrameRate)?;
    let (numerator, denominator) = value.split_once('/').map_or((value, "1"), |parts| parts);
    let numerator = numerator
        .parse::<u64>()
        .ok()
        .filter(|value| *value > 0)
        .ok_or(ApplicationError::MissingFrameRate)?;
    let denominator = denominator
        .parse::<u64>()
        .ok()
        .filter(|value| *value > 0)
        .ok_or(ApplicationError::MissingFrameRate)?;
    let mut timestamps = Vec::new();
    let mut frame_number = 0_u64;
    loop {
        let scaled = u128::from(frame_number)
            .saturating_mul(u128::from(denominator))
            .saturating_mul(1_000);
        let timestamp = ((scaled + u128::from(numerator / 2)) / u128::from(numerator))
            .min(u128::from(u64::MAX)) as u64;
        if timestamp >= duration_ms {
            break;
        }
        if timestamps.len() >= 100_000 {
            return Err(ApplicationError::SamplingPlanTooLarge);
        }
        if timestamps.last().copied() != Some(timestamp) {
            timestamps.push(timestamp);
        }
        frame_number = frame_number
            .checked_add(frame_interval)
            .ok_or(ApplicationError::SamplingPlanTooLarge)?;
    }
    Ok(timestamps)
}

pub fn plan_sampling_times(
    frame_timestamps: &[u64],
    duration_ms: u64,
    selections: &[StoredVideoSelection],
    config: &SamplingConfig,
) -> Result<Vec<u64>, ApplicationError> {
    if duration_ms == 0
        || (config.mode == SamplingMode::FrameInterval && frame_timestamps.is_empty())
    {
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
    let mut snapped = if frame_timestamps.is_empty() {
        requested
    } else {
        requested
            .into_iter()
            .filter_map(|timestamp| nearest_timestamp(frame_timestamps, timestamp))
            .collect::<Vec<_>>()
    };
    snapped.sort_unstable();
    snapped.dedup();
    if snapped.len() > 100_000 {
        return Err(ApplicationError::SamplingPlanTooLarge);
    }
    Ok(snapped)
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
    timestamp_ms: u64,
    source_frame_number: Option<u64>,
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
    if let Some(candidate) = store.get_candidate_near(
        &request.source.id,
        request.timestamp_ms,
        CANDIDATE_TIMESTAMP_TOLERANCE_MS,
    )? {
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
    let candidate = StoredCandidateImage {
        id,
        source_id: request.source.id.clone(),
        video_offset_ms: request.timestamp_ms,
        source_frame_number: request.source_frame_number,
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

fn find_candidate_near(
    candidates: &BTreeMap<u64, StoredCandidateImage>,
    timestamp_ms: u64,
) -> Option<&StoredCandidateImage> {
    let start = timestamp_ms.saturating_sub(CANDIDATE_TIMESTAMP_TOLERANCE_MS);
    let end = timestamp_ms.saturating_add(CANDIDATE_TIMESTAMP_TOLERANCE_MS);
    candidates
        .range(start..=end)
        .min_by_key(|(candidate_timestamp, _)| candidate_timestamp.abs_diff(timestamp_ms))
        .map(|(_, candidate)| candidate)
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

#[cfg(test)]
mod tests {
    use super::*;

    fn test_root(label: &str) -> PathBuf {
        std::env::temp_dir().join(format!("free-train-{label}-{}", Uuid::new_v4()))
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

    #[test]
    fn fixed_interval_plan_does_not_require_a_full_frame_timeline() {
        let config = SamplingConfig {
            mode: SamplingMode::FixedInterval,
            interval_ms: 1_000,
            frame_interval: 1,
            target_count: 1,
            range_ids: Vec::new(),
            custom_timestamps_ms: Vec::new(),
            pin_results: false,
        };
        let planned = plan_sampling_times(&[], 3_100, &[], &config).unwrap();
        assert_eq!(planned, vec![0, 1_000, 2_000, 3_000]);
    }

    #[test]
    fn frame_interval_plan_uses_nominal_rate_without_expanding_every_frame() {
        let planned = plan_nominal_frame_interval_times(3_100, Some("30000/1001"), 30).unwrap();
        assert_eq!(planned, vec![0, 1_001, 2_002, 3_003]);
    }

    #[test]
    fn fixed_interval_execution_batches_regular_frames_and_reports_progress() {
        if media::probe_ffprobe("ffprobe").is_err() {
            return;
        }
        let root = test_root("batch-sampling");
        fs::create_dir_all(&root).unwrap();
        let source_path = root.join("source.mp4");
        let output = std::process::Command::new("ffmpeg")
            .args([
                "-v",
                "error",
                "-y",
                "-f",
                "lavfi",
                "-i",
                "testsrc=size=320x180:rate=30:duration=9",
                "-c:v",
                "mpeg4",
            ])
            .arg(&source_path)
            .output()
            .unwrap();
        if !output.status.success() {
            let _ = fs::remove_dir_all(root);
            return;
        }
        let mut session = ProjectSession::create(&root, "project").unwrap();
        import_sources(
            &mut session,
            &[path_text(&source_path)],
            Path::new("ffprobe"),
            Path::new("ffmpeg"),
        )
        .unwrap();
        let source = list_sources(&session, 0, 10).unwrap().remove(0);
        let config = SamplingConfig {
            mode: SamplingMode::FixedInterval,
            interval_ms: 1_000,
            frame_interval: 1,
            target_count: 1,
            range_ids: Vec::new(),
            custom_timestamps_ms: Vec::new(),
            pin_results: false,
        };
        let mut progress = Vec::new();
        let result = execute_sampling_with_progress(
            &mut session,
            &source.id,
            &config,
            Path::new("ffprobe"),
            Path::new("ffmpeg"),
            |event| progress.push(event),
        )
        .unwrap();
        assert_eq!(result.planned, 9);
        assert_eq!(result.created, 9);
        assert_eq!(
            list_candidates(&session, &source.id, 0, 10).unwrap().len(),
            9
        );
        assert_eq!(progress.last().map(|event| event.completed), Some(9));

        let store = ProjectStore::open(session.database_path()).unwrap();
        let mut shifted = list_candidates(&session, &source.id, 0, 20)
            .unwrap()
            .into_iter()
            .find(|candidate| candidate.video_offset_ms == 2_000)
            .unwrap();
        store
            .delete_candidates(&source.id, Some(std::slice::from_ref(&shifted.id)))
            .unwrap();
        shifted.id = Uuid::new_v4().to_string();
        shifted.video_offset_ms = 1_999;
        store.upsert_candidate(&shifted).unwrap();

        let resumed = execute_sampling(
            &mut session,
            &source.id,
            &config,
            Path::new("ffprobe"),
            Path::new("ffmpeg"),
        )
        .unwrap();
        assert_eq!(resumed.created, 0);
        assert_eq!(resumed.existing, 9);
        assert_eq!(
            list_candidates(&session, &source.id, 0, 20).unwrap().len(),
            9
        );
        drop(session);
        fs::remove_dir_all(root).unwrap();
    }
}
