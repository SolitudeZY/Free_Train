use super::*;

struct PendingPreparedFiles {
    paths: Vec<PathBuf>,
    committed: bool,
}

impl PendingPreparedFiles {
    fn new() -> Self {
        Self {
            paths: Vec::new(),
            committed: false,
        }
    }

    fn track(&mut self, path: &Path) {
        self.paths.push(path.to_path_buf());
    }

    fn commit(&mut self) {
        self.committed = true;
    }
}

impl Drop for PendingPreparedFiles {
    fn drop(&mut self) {
        if self.committed {
            return;
        }
        for path in &self.paths {
            let _ = fs::remove_file(path);
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PrepareImagesResult {
    pub source_count: u64,
    pub prepared: u64,
    pub reused: u64,
    pub removed: u64,
    pub failures: Vec<ImportFailure>,
    pub cancelled: bool,
}

pub fn prepare_review_images(
    session: &ProjectSession,
    source_ids: &[String],
) -> Result<PrepareImagesResult, ApplicationError> {
    prepare_review_images_with_control(session, source_ids, None, |_| {})
}

pub fn prepare_review_images_with_control<F>(
    session: &ProjectSession,
    source_ids: &[String],
    control: Option<&OperationControl>,
    mut report_progress: F,
) -> Result<PrepareImagesResult, ApplicationError>
where
    F: FnMut(OperationProgress),
{
    let store = ProjectStore::open(session.database_path())?;
    let ids = source_ids.iter().collect::<HashSet<_>>();
    let sources = store
        .list_sources(0, 100_000)?
        .into_iter()
        .filter(|source| ids.is_empty() || ids.contains(&source.id))
        .collect::<Vec<_>>();
    let total = sources.len() as u64;
    let mut result = PrepareImagesResult {
        source_count: total,
        prepared: 0,
        reused: 0,
        removed: 0,
        failures: Vec::new(),
        cancelled: false,
    };
    report_progress(prepare_progress(&result, 0, total));
    for (index, source) in sources.into_iter().enumerate() {
        if control.is_some_and(|control| !control.checkpoint()) {
            result.cancelled = true;
            break;
        }
        match prepare_one_source(session, &store, &source, control) {
            Ok((prepared, reused, removed)) => {
                result.prepared += prepared;
                result.reused += reused;
                result.removed += removed;
            }
            Err(ApplicationError::OperationCancelled) => {
                result.cancelled = true;
                break;
            }
            Err(error) => result.failures.push(ImportFailure {
                path: source.absolute_path,
                error: error.to_string(),
            }),
        }
        report_progress(prepare_progress(&result, index as u64 + 1, total));
    }
    Ok(result)
}

fn prepare_one_source(
    session: &ProjectSession,
    store: &ProjectStore,
    source: &StoredSourceAsset,
    control: Option<&OperationControl>,
) -> Result<(u64, u64, u64), ApplicationError> {
    let profiles = list_effective_roi_profiles(session, &source.id)?;
    if profiles.is_empty() {
        let augmentation_previews = store.list_augmentation_previews()?;
        let stale = store.replace_prepared_images_for_source(&source.id, &[])?;
        remove_prepared_cache(session, &stale);
        remove_augmentation_cache(session, &augmentation_previews, &stale);
        return Ok((0, 0, stale.len() as u64));
    }
    let inputs = if source.kind == SourceKind::Image {
        vec![resolve_one_input(store, source, None)?]
    } else {
        store
            .list_candidates(&source.id, 0, 100_000)?
            .into_iter()
            .map(|candidate| processing_input_from_candidate(source, candidate))
            .collect::<Result<Vec<_>, _>>()?
    };
    if inputs.is_empty() {
        return Err(ApplicationError::NoCandidateImages);
    }
    let existing = store
        .list_prepared_images()?
        .into_iter()
        .filter(|image| image.source_id == source.id)
        .map(|image| (image.asset_key.clone(), image))
        .collect::<HashMap<_, _>>();
    let cache_dir = session
        .project_dir()
        .join("cache")
        .join("prepared-images")
        .join(&source.id);
    fs::create_dir_all(&cache_dir)?;
    let mut records = Vec::new();
    let mut prepared = 0_u64;
    let mut reused = 0_u64;
    let mut pending_files = PendingPreparedFiles::new();
    for input in inputs {
        if control.is_some_and(|control| !control.checkpoint()) {
            return Err(ApplicationError::OperationCancelled);
        }
        let image = media::load_oriented_image(&input.path)?;
        for profile in &profiles {
            profile.roi.validate(image.width(), image.height())?;
            for placement in image_pipeline::plan_tiles(profile.roi, profile.render_config.tile)? {
                if control.is_some_and(|control| !control.checkpoint()) {
                    return Err(ApplicationError::OperationCancelled);
                }
                let parameter_fingerprint =
                    prepared_fingerprint(source, &input, profile, placement)?;
                let asset_key = format!("prepared:{parameter_fingerprint}");
                let image_path = cache_dir.join(format!("{parameter_fingerprint}.jpg"));
                let thumbnail_path = cache_dir.join(format!("{parameter_fingerprint}.thumb.jpg"));
                if existing.get(&asset_key).is_some_and(|stored| {
                    stored.parameter_fingerprint == parameter_fingerprint
                        && image_path.is_file()
                        && thumbnail_path.is_file()
                }) {
                    reused += 1;
                } else {
                    if !image_path.exists() {
                        pending_files.track(&image_path);
                    }
                    if !thumbnail_path.exists() {
                        pending_files.track(&thumbnail_path);
                    }
                    let rendered = image_pipeline::render_tile(
                        &image,
                        profile.roi,
                        placement,
                        profile.render_config,
                    )?;
                    write_bytes_atomic(&image_path, &encode_image(&rendered, ExportFormat::Jpeg)?)?;
                    let temporary = cache_dir.join(format!(
                        ".{parameter_fingerprint}.{}.thumb.tmp.jpg",
                        Uuid::new_v4()
                    ));
                    media::create_image_thumbnail(&image_path, &temporary)?;
                    fs::rename(&temporary, &thumbnail_path)?;
                    prepared += 1;
                }
                records.push(StoredPreparedImage {
                    asset_key,
                    source_id: source.id.clone(),
                    candidate_id: input.candidate_id.clone(),
                    roi_profile_id: profile.id.clone(),
                    roi_name: profile.name.clone(),
                    tile_row: placement.row,
                    tile_column: placement.column,
                    image_path: path_text(&image_path),
                    thumbnail_path: path_text(&thumbnail_path),
                    width: placement.output_width,
                    height: placement.output_height,
                    parameter_fingerprint,
                    created_at: Utc::now().to_rfc3339(),
                });
            }
        }
    }
    let augmentation_previews = store.list_augmentation_previews()?;
    let stale = store.replace_prepared_images_for_source(&source.id, &records)?;
    pending_files.commit();
    remove_prepared_cache(session, &stale);
    remove_augmentation_cache(session, &augmentation_previews, &stale);
    Ok((prepared, reused, stale.len() as u64))
}

fn prepared_fingerprint(
    source: &StoredSourceAsset,
    input: &ProcessingInput,
    profile: &RoiProfile,
    placement: TilePlacement,
) -> Result<String, ApplicationError> {
    let document = serde_json::json!({
        "version": 1,
        "sourceId": source.id,
        "sourceFingerprint": source.quick_fingerprint,
        "candidateId": input.candidate_id,
        "roiProfileId": profile.id,
        "roi": profile.roi,
        "renderConfig": profile.render_config,
        "placement": placement,
    });
    Ok(hex::encode(Sha256::digest(serde_json::to_vec(&document)?)))
}

pub(crate) fn remove_prepared_cache(session: &ProjectSession, stale: &[StoredPreparedImage]) {
    let cache_root = session.project_dir().join("cache");
    for image in stale {
        for path in [&image.image_path, &image.thumbnail_path] {
            let path = PathBuf::from(path);
            if path.starts_with(&cache_root) {
                let _ = fs::remove_file(path);
            }
        }
    }
}

pub(crate) fn remove_augmentation_cache(
    session: &ProjectSession,
    previews: &[storage::StoredAugmentationPreview],
    stale: &[StoredPreparedImage],
) {
    let stale_keys = stale
        .iter()
        .map(|image| image.asset_key.as_str())
        .collect::<HashSet<_>>();
    let cache_root = session.project_dir().join("cache");
    for preview in previews {
        if !stale_keys.contains(preview.input_asset_key.as_str()) {
            continue;
        }
        let path = PathBuf::from(&preview.image_path);
        if path.starts_with(&cache_root) {
            let _ = fs::remove_file(path);
        }
    }
}

fn prepare_progress(result: &PrepareImagesResult, completed: u64, total: u64) -> OperationProgress {
    OperationProgress {
        phase: "正在准备稳定切片审核资产".to_owned(),
        completed,
        total,
        succeeded: result.prepared,
        existing: result.reused,
        failed: result.failures.len() as u64,
    }
}
