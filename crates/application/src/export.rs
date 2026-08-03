use super::*;

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
    let manually_excluded = if request.review_scope == ExportReviewScope::Eligible {
        store
            .list_review_assets()?
            .into_iter()
            .filter(|asset| asset.manual_decision.as_deref() == Some("exclude"))
            .map(|asset| asset.asset_key)
            .collect::<HashSet<_>>()
    } else {
        HashSet::new()
    };
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
    run_export_with_progress(session, request, |_| {})
}

pub fn run_export_with_progress<F>(
    session: &ProjectSession,
    request: &ExportRequest,
    mut report_progress: F,
) -> Result<ExportResult, ApplicationError>
where
    F: FnMut(OperationProgress),
{
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
    let total = plan.items.len() as u64;
    report_progress(OperationProgress {
        phase: "正在导出图片".to_owned(),
        completed: 0,
        total,
        succeeded: 0,
        existing: result.skipped,
        failed: 0,
    });
    for (index, item) in plan.items.into_iter().enumerate() {
        let attempt = (|| -> Result<(), ApplicationError> {
            let source = sources
                .get(&item.source_id)
                .ok_or_else(|| ApplicationError::SourceNotFound(item.source_id.clone()))?;
            let input = resolve_one_input(&store, source, item.candidate_id.as_deref())?;
            let profile = item
                .roi_profile_id
                .as_deref()
                .and_then(|profile_id| profiles.get(&item.source_id)?.get(profile_id));
            let encoded = if item.content == ExportContent::Frames
                && item.candidate_id.is_some()
                && request.format == ExportFormat::Jpeg
                && input.path.extension().is_some_and(|extension| {
                    extension.eq_ignore_ascii_case("jpg") || extension.eq_ignore_ascii_case("jpeg")
                }) {
                fs::read(&input.path)?
            } else {
                let image = media::load_oriented_image(&input.path)?;
                let rendered = match item.content {
                    ExportContent::Frames => image,
                    ExportContent::Tiles => {
                        let profile = profile.ok_or(ApplicationError::NoRoiProfiles)?;
                        render_tile(&image, profile.roi, item.placement, profile.render_config)?
                    }
                };
                encode_image(&rendered, request.format)?
            };
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
        report_progress(OperationProgress {
            phase: "正在导出图片".to_owned(),
            completed: index as u64 + 1,
            total,
            succeeded: result.written,
            existing: result.skipped,
            failed: result.failures.len() as u64,
        });
    }
    manifest.flush()?;
    fs::rename(&manifest_temporary, &manifest_path)?;
    Ok(result)
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
