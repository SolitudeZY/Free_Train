use super::*;
use image_pipeline::{
    AUGMENTATION_RECIPE_SCHEMA_VERSION, AppliedAugmentation, AugmentationRecipe, ExportFormat,
    RUST_AUGMENTATION_EXECUTOR_VERSION, encode_image, execute_augmentation_recipe,
};
use storage::{StoredAugmentationPreview, StoredAugmentationRecipe};

const AUGMENTATION_EXECUTOR_NAME: &str = "free-train-rust";

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AugmentationPreviewRequest {
    pub input_asset_key: String,
    pub recipe: AugmentationRecipe,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AugmentationPreview {
    pub id: String,
    pub recipe_id: String,
    pub recipe_name: String,
    pub input_asset_key: String,
    pub variant_index: u32,
    pub seed: String,
    pub lineage_group_id: String,
    pub executor_name: String,
    pub executor_version: String,
    pub recipe_schema_version: u32,
    pub applied_parameters: Vec<AppliedAugmentation>,
    pub image_path: String,
    pub content_sha256: String,
}

pub fn list_augmentation_recipes(
    session: &ProjectSession,
) -> Result<Vec<AugmentationRecipe>, ApplicationError> {
    ProjectStore::open(session.database_path())?
        .list_augmentation_recipes()?
        .into_iter()
        .map(|stored| serde_json::from_str(&stored.recipe_json).map_err(ApplicationError::from))
        .collect()
}

pub fn generate_augmentation_previews(
    session: &ProjectSession,
    request: &AugmentationPreviewRequest,
) -> Result<Vec<AugmentationPreview>, ApplicationError> {
    request.recipe.validate()?;
    let store = ProjectStore::open(session.database_path())?;
    let prepared = store
        .list_prepared_images()?
        .into_iter()
        .find(|image| image.asset_key == request.input_asset_key)
        .ok_or(ApplicationError::AugmentationInputNotPrepared)?;
    if store
        .get_review_asset(&request.input_asset_key)?
        .is_some_and(|review| review.manual_decision.as_deref() == Some("exclude"))
    {
        return Err(ApplicationError::AugmentationInputExcluded);
    }

    let image = image::open(&prepared.image_path)?;
    let now = Utc::now().to_rfc3339();
    let recipe_json = serde_json::to_string(&request.recipe)?;
    store.upsert_augmentation_recipe(&StoredAugmentationRecipe {
        id: request.recipe.id.clone(),
        name: request.recipe.name.clone(),
        schema_version: request.recipe.schema_version,
        recipe_json,
        created_at: now.clone(),
        updated_at: now.clone(),
    })?;

    let recipe_fingerprint = hex::encode(Sha256::digest(serde_json::to_vec(&request.recipe)?));
    let cache_dir = session
        .project_dir()
        .join("cache")
        .join("augmentation-previews")
        .join(&recipe_fingerprint[..16]);
    fs::create_dir_all(&cache_dir)?;
    let lineage_group_id = stable_identifier("lineage", &[request.input_asset_key.as_bytes()]);
    let mut previews = Vec::with_capacity(request.recipe.variant_count as usize);
    let mut stored = Vec::with_capacity(request.recipe.variant_count as usize);
    for variant_index in 0..request.recipe.variant_count {
        let seed = derive_variant_seed(
            request.recipe.base_seed,
            &request.input_asset_key,
            variant_index,
        );
        let (rendered, applied_parameters) =
            execute_augmentation_recipe(&image, &request.recipe, seed)?;
        let encoded = encode_image(&rendered, ExportFormat::Jpeg)?;
        let content_sha256 = hex::encode(Sha256::digest(&encoded));
        let id = stable_identifier(
            "augmentation-preview",
            &[
                request.recipe.id.as_bytes(),
                request.input_asset_key.as_bytes(),
                variant_index.to_string().as_bytes(),
                seed.to_string().as_bytes(),
            ],
        );
        let image_path = cache_dir.join(format!("{id}.jpg"));
        write_bytes_atomic(&image_path, &encoded)?;
        let preview = AugmentationPreview {
            id: id.clone(),
            recipe_id: request.recipe.id.clone(),
            recipe_name: request.recipe.name.clone(),
            input_asset_key: request.input_asset_key.clone(),
            variant_index,
            seed: seed.to_string(),
            lineage_group_id: lineage_group_id.clone(),
            executor_name: AUGMENTATION_EXECUTOR_NAME.to_owned(),
            executor_version: RUST_AUGMENTATION_EXECUTOR_VERSION.to_owned(),
            recipe_schema_version: AUGMENTATION_RECIPE_SCHEMA_VERSION,
            applied_parameters: applied_parameters.clone(),
            image_path: path_text(&image_path),
            content_sha256: content_sha256.clone(),
        };
        stored.push(StoredAugmentationPreview {
            id,
            recipe_id: preview.recipe_id.clone(),
            input_asset_key: preview.input_asset_key.clone(),
            variant_index,
            seed: seed.to_string(),
            lineage_group_id: lineage_group_id.clone(),
            executor_name: preview.executor_name.clone(),
            executor_version: preview.executor_version.clone(),
            recipe_schema_version: preview.recipe_schema_version,
            applied_parameters_json: serde_json::to_string(&applied_parameters)?,
            image_path: preview.image_path.clone(),
            content_sha256,
            created_at: now.clone(),
        });
        previews.push(preview);
    }
    let stale = store.replace_augmentation_previews(
        &request.recipe.id,
        &request.input_asset_key,
        &stored,
    )?;
    let active_paths = stored
        .iter()
        .map(|preview| preview.image_path.as_str())
        .collect::<HashSet<_>>();
    for preview in stale {
        if !active_paths.contains(preview.image_path.as_str()) {
            let _ = fs::remove_file(preview.image_path);
        }
    }
    Ok(previews)
}

fn derive_variant_seed(base_seed: u64, asset_key: &str, variant_index: u32) -> u64 {
    let digest = Sha256::digest(
        [
            base_seed.to_le_bytes().as_slice(),
            asset_key.as_bytes(),
            variant_index.to_le_bytes().as_slice(),
        ]
        .concat(),
    );
    u64::from_le_bytes(
        digest[..8]
            .try_into()
            .expect("SHA-256 prefix is eight bytes"),
    )
}

fn stable_identifier(namespace: &str, parts: &[&[u8]]) -> String {
    let mut hasher = Sha256::new();
    hasher.update(namespace.as_bytes());
    for part in parts {
        hasher.update((part.len() as u64).to_le_bytes());
        hasher.update(part);
    }
    hex::encode(hasher.finalize())
}

#[cfg(test)]
mod tests {
    use super::*;
    use domain::{SourceKind, SourceStatus};
    use image::{ImageBuffer, Rgb};
    use image_pipeline::{AUGMENTATION_RECIPE_SCHEMA_VERSION, AugmentationOperation};
    use storage::{StoredPreparedImage, StoredRoiProfile, StoredSourceAsset};

    #[test]
    fn variant_seed_is_stable_and_separated_by_asset_and_index() {
        let first = derive_variant_seed(42, "prepared:a", 0);
        assert_eq!(first, derive_variant_seed(42, "prepared:a", 0));
        assert_ne!(first, derive_variant_seed(42, "prepared:a", 1));
        assert_ne!(first, derive_variant_seed(42, "prepared:b", 0));
    }

    #[test]
    fn preview_generation_is_repeatable_and_persists_provenance() {
        let root = std::env::temp_dir().join(format!("free-train-augmentation-{}", Uuid::new_v4()));
        fs::create_dir_all(&root).unwrap();
        let session = ProjectSession::create(&root, "preview-test").unwrap();
        let source_path = root.join("source.png");
        let prepared_path = session.project_dir().join("prepared.jpg");
        ImageBuffer::from_fn(32, 24, |x, y| Rgb([(x * 7) as u8, (y * 9) as u8, 80]))
            .save(&source_path)
            .unwrap();
        image::open(&source_path)
            .unwrap()
            .save(&prepared_path)
            .unwrap();
        let source_hash = full_hash(&source_path).unwrap();
        let store = ProjectStore::open(session.database_path()).unwrap();
        store
            .upsert_source(&StoredSourceAsset {
                id: "source-1".to_owned(),
                absolute_path: path_text(&source_path),
                file_name: "source.png".to_owned(),
                relative_folder: String::new(),
                source_group: "camera-a".to_owned(),
                source_identifier: "source".to_owned(),
                kind: SourceKind::Image,
                status: SourceStatus::Online,
                size_bytes: fs::metadata(&source_path).unwrap().len(),
                modified_unix_ms: 0,
                quick_fingerprint: "fixture".to_owned(),
                sha256: Some(source_hash.clone()),
                width: Some(32),
                height: Some(24),
                duration_ms: None,
                codec: None,
                frame_rate: None,
                capture_time: None,
                capture_time_source: None,
                orientation: None,
                thumbnail_path: None,
                error: None,
                imported_at: "2026-01-01T00:00:00Z".to_owned(),
                last_checked_at: "2026-01-01T00:00:00Z".to_owned(),
            })
            .unwrap();
        store
            .upsert_roi_profile(&StoredRoiProfile {
                id: "roi-1".to_owned(),
                scope_kind: "source".to_owned(),
                scope_value: "source-1".to_owned(),
                name: "track".to_owned(),
                x: 0,
                y: 0,
                width: 32,
                height: 24,
                render_config_json: "{}".to_owned(),
                created_at: "2026-01-01T00:00:00Z".to_owned(),
                updated_at: "2026-01-01T00:00:00Z".to_owned(),
            })
            .unwrap();
        store
            .replace_prepared_images_for_source(
                "source-1",
                &[StoredPreparedImage {
                    asset_key: "prepared:fixture".to_owned(),
                    source_id: "source-1".to_owned(),
                    candidate_id: None,
                    roi_profile_id: "roi-1".to_owned(),
                    roi_name: "track".to_owned(),
                    tile_row: 0,
                    tile_column: 0,
                    image_path: path_text(&prepared_path),
                    thumbnail_path: path_text(&prepared_path),
                    width: 32,
                    height: 24,
                    parameter_fingerprint: "fixture".to_owned(),
                    created_at: "2026-01-01T00:00:00Z".to_owned(),
                }],
            )
            .unwrap();
        let request = AugmentationPreviewRequest {
            input_asset_key: "prepared:fixture".to_owned(),
            recipe: AugmentationRecipe {
                id: "recipe-test".to_owned(),
                name: "测试配方".to_owned(),
                schema_version: AUGMENTATION_RECIPE_SCHEMA_VERSION,
                base_seed: 77,
                variant_count: 3,
                operations: vec![
                    AugmentationOperation::Brightness {
                        enabled: true,
                        probability: 1.0,
                        min_offset: -10,
                        max_offset: 10,
                    },
                    AugmentationOperation::HorizontalFlip {
                        enabled: true,
                        probability: 0.5,
                    },
                ],
            },
        };
        let first = generate_augmentation_previews(&session, &request).unwrap();
        let second = generate_augmentation_previews(&session, &request).unwrap();
        assert_eq!(
            first
                .iter()
                .map(|item| &item.content_sha256)
                .collect::<Vec<_>>(),
            second
                .iter()
                .map(|item| &item.content_sha256)
                .collect::<Vec<_>>()
        );
        assert_eq!(store.list_augmentation_previews().unwrap().len(), 3);
        assert_eq!(store.list_augmentation_recipes().unwrap().len(), 1);
        assert_eq!(source_hash, full_hash(&source_path).unwrap());
        drop(store);
        drop(session);
        let _ = fs::remove_dir_all(root);
    }
}
