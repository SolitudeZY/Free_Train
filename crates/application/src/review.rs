use super::*;

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
    run_review_analysis_at(session.database_path(), config)
}

pub fn run_review_analysis_at(
    database_path: impl AsRef<Path>,
    config: &ReviewAnalysisConfig,
) -> Result<ReviewWorkspace, ApplicationError> {
    validate_review_config(config)?;
    let database_path = database_path.as_ref();
    let store = ProjectStore::open(database_path)?;
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
    list_review_workspace_at(database_path)
}

pub fn list_review_workspace(
    session: &ProjectSession,
) -> Result<ReviewWorkspace, ApplicationError> {
    list_review_workspace_at(session.database_path())
}

pub fn list_review_workspace_at(
    database_path: impl AsRef<Path>,
) -> Result<ReviewWorkspace, ApplicationError> {
    let store = ProjectStore::open(database_path)?;
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
    let can_undo = store.latest_undoable_review_audit()?.is_some();
    let can_redo = store.latest_redo_review_audit()?.is_some();
    Ok(ReviewWorkspace {
        items,
        summary,
        can_undo,
        can_redo,
    })
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
                .clone()
                .ok_or(ApplicationError::ReviewAssetHasNoSimilarityGroup)?;
            let before_group = store
                .list_review_assets()?
                .into_iter()
                .filter(|item| item.similarity_group_id.as_deref() == Some(group_id.as_str()))
                .collect::<Vec<_>>();
            if !store.set_group_representative(&group_id, asset_key, &now)? {
                continue;
            }
            let after_group = store
                .list_review_assets()?
                .into_iter()
                .filter(|item| item.similarity_group_id.as_deref() == Some(group_id.as_str()))
                .collect::<Vec<_>>();
            store.insert_review_audit(
                &Uuid::new_v4().to_string(),
                asset_key,
                "make_representative_group",
                &serde_json::to_string(&before_group)?,
                &serde_json::to_string(&after_group)?,
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

pub fn undo_review_action(session: &ProjectSession) -> Result<ReviewWorkspace, ApplicationError> {
    let store = ProjectStore::open(session.database_path())?;
    let event = store
        .latest_undoable_review_audit()?
        .ok_or(ApplicationError::NoReviewUndoAvailable)?;
    apply_review_audit_snapshot(&store, &event, false)?;
    store.mark_review_audit_undone(&event.id, &Utc::now().to_rfc3339())?;
    list_review_workspace(session)
}

pub fn redo_review_action(session: &ProjectSession) -> Result<ReviewWorkspace, ApplicationError> {
    let store = ProjectStore::open(session.database_path())?;
    let event = store
        .latest_redo_review_audit()?
        .ok_or(ApplicationError::NoReviewRedoAvailable)?;
    apply_review_audit_snapshot(&store, &event, true)?;
    store.mark_review_audit_redone(&event.id)?;
    list_review_workspace(session)
}

fn apply_review_audit_snapshot(
    store: &ProjectStore,
    event: &StoredReviewAuditEvent,
    use_after: bool,
) -> Result<(), ApplicationError> {
    let snapshot_json = if use_after {
        &event.after_json
    } else {
        &event.before_json
    };
    if event.action == "make_representative_group" {
        let snapshot: Vec<StoredReviewAsset> = serde_json::from_str(snapshot_json)?;
        let representative = snapshot
            .iter()
            .find(|item| item.representative)
            .ok_or(ApplicationError::ReviewAssetHasNoSimilarityGroup)?;
        let group_id = representative
            .similarity_group_id
            .as_deref()
            .ok_or(ApplicationError::ReviewAssetHasNoSimilarityGroup)?;
        store.set_group_representative(
            group_id,
            &representative.asset_key,
            &Utc::now().to_rfc3339(),
        )?;
        return Ok(());
    }
    let snapshot: StoredReviewAsset = serde_json::from_str(snapshot_json)?;
    store.set_review_state(
        &event.asset_key,
        snapshot.manual_decision.as_deref(),
        snapshot.locked,
        &Utc::now().to_rfc3339(),
    )?;
    Ok(())
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
