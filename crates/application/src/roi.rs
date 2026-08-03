use super::*;

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
    result.sort_by_key(|item| item.name.to_lowercase());
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
