use std::{
    collections::HashSet,
    path::{Path, PathBuf},
};

use domain::{SourceKind, SourceStatus};
use rusqlite::{Connection, OptionalExtension, params};
use serde::{Deserialize, Serialize};
use thiserror::Error;

const MIGRATION_1: &str = r#"
CREATE TABLE IF NOT EXISTS schema_migrations (
    version INTEGER PRIMARY KEY,
    applied_at TEXT NOT NULL DEFAULT CURRENT_TIMESTAMP
);
CREATE TABLE IF NOT EXISTS project_meta (
    key TEXT PRIMARY KEY,
    value TEXT NOT NULL
);
CREATE TABLE IF NOT EXISTS source_assets (
    id TEXT PRIMARY KEY,
    absolute_path TEXT NOT NULL UNIQUE,
    file_name TEXT NOT NULL,
    relative_folder TEXT NOT NULL,
    source_group TEXT NOT NULL,
    source_identifier TEXT NOT NULL,
    kind TEXT NOT NULL,
    status TEXT NOT NULL,
    size_bytes INTEGER NOT NULL,
    modified_unix_ms INTEGER NOT NULL,
    quick_fingerprint TEXT NOT NULL,
    sha256 TEXT,
    width INTEGER,
    height INTEGER,
    duration_ms INTEGER,
    codec TEXT,
    frame_rate TEXT,
    capture_time TEXT,
    capture_time_source TEXT,
    orientation INTEGER,
    thumbnail_path TEXT,
    error TEXT,
    imported_at TEXT NOT NULL,
    last_checked_at TEXT NOT NULL
);
CREATE INDEX IF NOT EXISTS idx_source_assets_group ON source_assets(source_group, source_identifier);
CREATE INDEX IF NOT EXISTS idx_source_assets_status ON source_assets(status);
"#;

const MIGRATION_2: &str = r#"
CREATE TABLE IF NOT EXISTS video_selections (
    id TEXT PRIMARY KEY,
    source_id TEXT NOT NULL,
    start_ms INTEGER NOT NULL,
    end_ms INTEGER NOT NULL,
    label TEXT NOT NULL,
    protected INTEGER NOT NULL DEFAULT 0,
    created_at TEXT NOT NULL,
    FOREIGN KEY(source_id) REFERENCES source_assets(id) ON DELETE CASCADE,
    CHECK(start_ms >= 0 AND end_ms > start_ms)
);
CREATE INDEX IF NOT EXISTS idx_video_selections_source ON video_selections(source_id, start_ms);

CREATE TABLE IF NOT EXISTS candidate_images (
    id TEXT PRIMARY KEY,
    source_id TEXT NOT NULL,
    video_offset_ms INTEGER NOT NULL,
    source_frame_number INTEGER,
    selection_method TEXT NOT NULL,
    parameters_json TEXT NOT NULL,
    image_path TEXT NOT NULL,
    thumbnail_path TEXT NOT NULL,
    width INTEGER NOT NULL,
    height INTEGER NOT NULL,
    pinned INTEGER NOT NULL DEFAULT 0,
    created_at TEXT NOT NULL,
    FOREIGN KEY(source_id) REFERENCES source_assets(id) ON DELETE CASCADE,
    UNIQUE(source_id, video_offset_ms)
);
CREATE INDEX IF NOT EXISTS idx_candidate_images_source ON candidate_images(source_id, video_offset_ms);
"#;

const MIGRATION_3: &str = r#"
CREATE TABLE IF NOT EXISTS roi_profiles (
    id TEXT PRIMARY KEY,
    scope_kind TEXT NOT NULL,
    scope_value TEXT NOT NULL,
    name TEXT NOT NULL,
    x INTEGER NOT NULL,
    y INTEGER NOT NULL,
    width INTEGER NOT NULL,
    height INTEGER NOT NULL,
    render_config_json TEXT NOT NULL,
    created_at TEXT NOT NULL,
    updated_at TEXT NOT NULL,
    CHECK(scope_kind IN ('source_group', 'source')),
    CHECK(width > 0 AND height > 0),
    UNIQUE(scope_kind, scope_value, name)
);
CREATE INDEX IF NOT EXISTS idx_roi_profiles_scope
ON roi_profiles(scope_kind, scope_value, name);
"#;

const MIGRATION_4: &str = r#"
CREATE TABLE IF NOT EXISTS quality_assessments (
    asset_key TEXT PRIMARY KEY,
    source_id TEXT NOT NULL,
    candidate_id TEXT,
    width INTEGER NOT NULL,
    height INTEGER NOT NULL,
    aspect_ratio REAL NOT NULL,
    sharpness REAL NOT NULL,
    underexposed_ratio REAL NOT NULL,
    overexposed_ratio REAL NOT NULL,
    entropy REAL NOT NULL,
    low_information REAL NOT NULL,
    content_sha256 TEXT NOT NULL,
    perceptual_hash TEXT NOT NULL,
    algorithm_version TEXT NOT NULL,
    analyzed_at TEXT NOT NULL,
    decode_error TEXT,
    FOREIGN KEY(source_id) REFERENCES source_assets(id) ON DELETE CASCADE,
    FOREIGN KEY(candidate_id) REFERENCES candidate_images(id) ON DELETE CASCADE
);
CREATE INDEX IF NOT EXISTS idx_quality_assessments_source
ON quality_assessments(source_id, candidate_id);

CREATE TABLE IF NOT EXISTS review_assets (
    asset_key TEXT PRIMARY KEY,
    source_id TEXT NOT NULL,
    candidate_id TEXT,
    automatic_status TEXT NOT NULL DEFAULT 'keep',
    automatic_reasons_json TEXT NOT NULL DEFAULT '[]',
    manual_decision TEXT,
    locked INTEGER NOT NULL DEFAULT 0,
    similarity_group_id TEXT,
    similarity_score REAL,
    representative INTEGER NOT NULL DEFAULT 0,
    locked_conflict INTEGER NOT NULL DEFAULT 0,
    updated_at TEXT NOT NULL,
    CHECK(automatic_status IN ('keep', 'suggest_exclude', 'warning', 'error')),
    CHECK(manual_decision IS NULL OR manual_decision IN ('keep', 'exclude')),
    FOREIGN KEY(source_id) REFERENCES source_assets(id) ON DELETE CASCADE,
    FOREIGN KEY(candidate_id) REFERENCES candidate_images(id) ON DELETE CASCADE
);
CREATE INDEX IF NOT EXISTS idx_review_assets_status
ON review_assets(manual_decision, automatic_status, similarity_group_id);

CREATE TABLE IF NOT EXISTS review_audit_events (
    id TEXT PRIMARY KEY,
    asset_key TEXT NOT NULL,
    action TEXT NOT NULL,
    before_json TEXT NOT NULL,
    after_json TEXT NOT NULL,
    actor TEXT NOT NULL,
    created_at TEXT NOT NULL,
    FOREIGN KEY(asset_key) REFERENCES review_assets(asset_key) ON DELETE CASCADE
);
CREATE INDEX IF NOT EXISTS idx_review_audit_asset
ON review_audit_events(asset_key, created_at);
"#;

const MIGRATION_5: &str = r#"
CREATE TABLE IF NOT EXISTS review_redo_events (
    audit_id TEXT PRIMARY KEY,
    moved_at TEXT NOT NULL,
    FOREIGN KEY(audit_id) REFERENCES review_audit_events(id) ON DELETE CASCADE
);
"#;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StorageProbe {
    pub sqlite_version: String,
    pub schema_version: i64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct StoredSourceAsset {
    pub id: String,
    pub absolute_path: String,
    pub file_name: String,
    pub relative_folder: String,
    pub source_group: String,
    pub source_identifier: String,
    pub kind: SourceKind,
    pub status: SourceStatus,
    pub size_bytes: u64,
    pub modified_unix_ms: i64,
    pub quick_fingerprint: String,
    pub sha256: Option<String>,
    pub width: Option<u32>,
    pub height: Option<u32>,
    pub duration_ms: Option<u64>,
    pub codec: Option<String>,
    pub frame_rate: Option<String>,
    pub capture_time: Option<String>,
    pub capture_time_source: Option<String>,
    pub orientation: Option<u32>,
    pub thumbnail_path: Option<String>,
    pub error: Option<String>,
    pub imported_at: String,
    pub last_checked_at: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct StoredVideoSelection {
    pub id: String,
    pub source_id: String,
    pub start_ms: u64,
    pub end_ms: u64,
    pub label: String,
    pub protected: bool,
    pub created_at: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct StoredCandidateImage {
    pub id: String,
    pub source_id: String,
    pub video_offset_ms: u64,
    pub source_frame_number: Option<u64>,
    pub selection_method: String,
    pub parameters_json: String,
    pub image_path: String,
    pub thumbnail_path: String,
    pub width: u32,
    pub height: u32,
    pub pinned: bool,
    pub created_at: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct StoredRoiProfile {
    pub id: String,
    pub scope_kind: String,
    pub scope_value: String,
    pub name: String,
    pub x: u32,
    pub y: u32,
    pub width: u32,
    pub height: u32,
    pub render_config_json: String,
    pub created_at: String,
    pub updated_at: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct StoredQualityAssessment {
    pub asset_key: String,
    pub source_id: String,
    pub candidate_id: Option<String>,
    pub width: u32,
    pub height: u32,
    pub aspect_ratio: f64,
    pub sharpness: f64,
    pub underexposed_ratio: f64,
    pub overexposed_ratio: f64,
    pub entropy: f64,
    pub low_information: f64,
    pub content_sha256: String,
    pub perceptual_hash: String,
    pub algorithm_version: String,
    pub analyzed_at: String,
    pub decode_error: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct StoredReviewAsset {
    pub asset_key: String,
    pub source_id: String,
    pub candidate_id: Option<String>,
    pub automatic_status: String,
    pub automatic_reasons_json: String,
    pub manual_decision: Option<String>,
    pub locked: bool,
    pub similarity_group_id: Option<String>,
    pub similarity_score: Option<f64>,
    pub representative: bool,
    pub locked_conflict: bool,
    pub updated_at: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct StoredReviewAuditEvent {
    pub id: String,
    pub asset_key: String,
    pub action: String,
    pub before_json: String,
    pub after_json: String,
}

pub struct ProjectStore {
    database_path: PathBuf,
}

impl ProjectStore {
    pub fn open(database_path: impl AsRef<Path>) -> Result<Self, StorageError> {
        let store = Self {
            database_path: database_path.as_ref().to_path_buf(),
        };
        let connection = store.connection()?;
        apply_migrations(&connection)?;
        Ok(store)
    }

    fn connection(&self) -> Result<Connection, StorageError> {
        let connection = Connection::open(&self.database_path)?;
        connection.pragma_update(None, "foreign_keys", "ON")?;
        connection.pragma_update(None, "journal_mode", "WAL")?;
        connection.busy_timeout(std::time::Duration::from_secs(5))?;
        Ok(connection)
    }

    pub fn set_meta(&self, key: &str, value: &str) -> Result<(), StorageError> {
        self.connection()?.execute(
            "INSERT INTO project_meta(key, value) VALUES (?1, ?2)\n             ON CONFLICT(key) DO UPDATE SET value = excluded.value",
            params![key, value],
        )?;
        Ok(())
    }

    pub fn get_meta(&self, key: &str) -> Result<Option<String>, StorageError> {
        Ok(self
            .connection()?
            .query_row(
                "SELECT value FROM project_meta WHERE key = ?1",
                params![key],
                |row| row.get(0),
            )
            .optional()?)
    }

    pub fn upsert_source(&self, asset: &StoredSourceAsset) -> Result<bool, StorageError> {
        let connection = self.connection()?;
        upsert_source_on(&connection, asset)
    }

    pub fn upsert_sources(&self, assets: &[StoredSourceAsset]) -> Result<Vec<bool>, StorageError> {
        let mut connection = self.connection()?;
        let transaction = connection.transaction()?;
        let mut existed = Vec::with_capacity(assets.len());
        for asset in assets {
            existed.push(upsert_source_on(&transaction, asset)?);
        }
        transaction.commit()?;
        Ok(existed)
    }

    pub fn list_sources(
        &self,
        offset: u32,
        limit: u32,
    ) -> Result<Vec<StoredSourceAsset>, StorageError> {
        let connection = self.connection()?;
        let mut statement = connection.prepare(
            r#"SELECT id, absolute_path, file_name, relative_folder, source_group,
               source_identifier, kind, status, size_bytes, modified_unix_ms,
               quick_fingerprint, sha256, width, height, duration_ms, codec,
               frame_rate, capture_time, capture_time_source, orientation,
               thumbnail_path, error, imported_at, last_checked_at
               FROM source_assets
               ORDER BY source_group COLLATE NOCASE, source_identifier COLLATE NOCASE,
                        file_name COLLATE NOCASE
               LIMIT ?1 OFFSET ?2"#,
        )?;
        let rows = statement.query_map(params![limit, offset], row_to_asset)?;
        Ok(rows.collect::<Result<Vec<_>, _>>()?)
    }

    pub fn list_source_ids(&self) -> Result<Vec<String>, StorageError> {
        let connection = self.connection()?;
        let mut statement = connection.prepare(
            "SELECT id FROM source_assets
             ORDER BY source_group COLLATE NOCASE, source_identifier COLLATE NOCASE,
                      file_name COLLATE NOCASE",
        )?;
        let rows = statement.query_map([], |row| row.get(0))?;
        Ok(rows.collect::<Result<Vec<_>, _>>()?)
    }

    pub fn get_source(&self, id: &str) -> Result<Option<StoredSourceAsset>, StorageError> {
        let connection = self.connection()?;
        let mut statement = connection.prepare(
            r#"SELECT id, absolute_path, file_name, relative_folder, source_group,
               source_identifier, kind, status, size_bytes, modified_unix_ms,
               quick_fingerprint, sha256, width, height, duration_ms, codec,
               frame_rate, capture_time, capture_time_source, orientation,
               thumbnail_path, error, imported_at, last_checked_at
               FROM source_assets WHERE id = ?1"#,
        )?;
        Ok(statement.query_row(params![id], row_to_asset).optional()?)
    }

    pub fn get_source_by_path(
        &self,
        absolute_path: &str,
    ) -> Result<Option<StoredSourceAsset>, StorageError> {
        let connection = self.connection()?;
        let mut statement = connection.prepare(
            r#"SELECT id, absolute_path, file_name, relative_folder, source_group,
               source_identifier, kind, status, size_bytes, modified_unix_ms,
               quick_fingerprint, sha256, width, height, duration_ms, codec,
               frame_rate, capture_time, capture_time_source, orientation,
               thumbnail_path, error, imported_at, last_checked_at
               FROM source_assets WHERE absolute_path = ?1"#,
        )?;
        Ok(statement
            .query_row(params![absolute_path], row_to_asset)
            .optional()?)
    }

    pub fn find_sources_by_fingerprint(
        &self,
        kind: SourceKind,
        size_bytes: u64,
        quick_fingerprint: &str,
    ) -> Result<Vec<StoredSourceAsset>, StorageError> {
        let connection = self.connection()?;
        let mut statement = connection.prepare(
            r#"SELECT id, absolute_path, file_name, relative_folder, source_group,
               source_identifier, kind, status, size_bytes, modified_unix_ms,
               quick_fingerprint, sha256, width, height, duration_ms, codec,
               frame_rate, capture_time, capture_time_source, orientation,
               thumbnail_path, error, imported_at, last_checked_at
               FROM source_assets
               WHERE kind = ?1 AND size_bytes = ?2 AND quick_fingerprint = ?3"#,
        )?;
        let rows = statement.query_map(
            params![kind_text(kind), size_bytes, quick_fingerprint],
            row_to_asset,
        )?;
        Ok(rows.collect::<Result<Vec<_>, _>>()?)
    }

    pub fn update_source_status(
        &self,
        id: &str,
        status: SourceStatus,
        error: Option<&str>,
        checked_at: &str,
    ) -> Result<(), StorageError> {
        self.connection()?.execute(
            "UPDATE source_assets SET status=?2, error=?3, last_checked_at=?4 WHERE id=?1",
            params![id, status_text(status), error, checked_at],
        )?;
        Ok(())
    }

    pub fn update_source_sha256(&self, id: &str, sha256: &str) -> Result<(), StorageError> {
        self.connection()?.execute(
            "UPDATE source_assets SET sha256=?2 WHERE id=?1",
            params![id, sha256],
        )?;
        Ok(())
    }

    pub fn insert_video_selection(
        &self,
        selection: &StoredVideoSelection,
    ) -> Result<(), StorageError> {
        self.connection()?.execute(
            "INSERT INTO video_selections(id, source_id, start_ms, end_ms, label, protected, created_at)\n             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)",
            params![selection.id, selection.source_id, selection.start_ms, selection.end_ms, selection.label, selection.protected, selection.created_at],
        )?;
        Ok(())
    }

    pub fn list_video_selections(
        &self,
        source_id: &str,
    ) -> Result<Vec<StoredVideoSelection>, StorageError> {
        let connection = self.connection()?;
        let mut statement = connection.prepare(
            "SELECT id, source_id, start_ms, end_ms, label, protected, created_at\n             FROM video_selections WHERE source_id=?1 ORDER BY start_ms, end_ms",
        )?;
        let rows = statement.query_map(params![source_id], |row| {
            Ok(StoredVideoSelection {
                id: row.get(0)?,
                source_id: row.get(1)?,
                start_ms: row.get(2)?,
                end_ms: row.get(3)?,
                label: row.get(4)?,
                protected: row.get(5)?,
                created_at: row.get(6)?,
            })
        })?;
        Ok(rows.collect::<Result<Vec<_>, _>>()?)
    }

    pub fn delete_video_selection(&self, id: &str) -> Result<bool, StorageError> {
        Ok(self
            .connection()?
            .execute("DELETE FROM video_selections WHERE id=?1", params![id])?
            > 0)
    }

    pub fn upsert_candidate(&self, candidate: &StoredCandidateImage) -> Result<bool, StorageError> {
        let connection = self.connection()?;
        let existed = connection
            .query_row(
                "SELECT 1 FROM candidate_images WHERE source_id=?1 AND video_offset_ms=?2",
                params![candidate.source_id, candidate.video_offset_ms],
                |_| Ok(()),
            )
            .optional()?
            .is_some();
        connection.execute(
            r#"INSERT INTO candidate_images(
                id, source_id, video_offset_ms, source_frame_number, selection_method,
                parameters_json, image_path, thumbnail_path, width, height, pinned, created_at
            ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12)
            ON CONFLICT(source_id, video_offset_ms) DO UPDATE SET
                source_frame_number=COALESCE(candidate_images.source_frame_number, excluded.source_frame_number),
                selection_method=CASE WHEN excluded.pinned=1 THEN excluded.selection_method ELSE candidate_images.selection_method END,
                parameters_json=CASE WHEN excluded.pinned=1 THEN excluded.parameters_json ELSE candidate_images.parameters_json END,
                pinned=MAX(candidate_images.pinned, excluded.pinned)"#,
            params![candidate.id, candidate.source_id, candidate.video_offset_ms,
                candidate.source_frame_number, candidate.selection_method, candidate.parameters_json,
                candidate.image_path, candidate.thumbnail_path, candidate.width, candidate.height,
                candidate.pinned, candidate.created_at],
        )?;
        Ok(existed)
    }

    pub fn get_candidate_at(
        &self,
        source_id: &str,
        video_offset_ms: u64,
    ) -> Result<Option<StoredCandidateImage>, StorageError> {
        let connection = self.connection()?;
        let mut statement = connection.prepare(
            "SELECT id, source_id, video_offset_ms, source_frame_number, selection_method,\n                    parameters_json, image_path, thumbnail_path, width, height, pinned, created_at\n             FROM candidate_images WHERE source_id=?1 AND video_offset_ms=?2",
        )?;
        Ok(statement
            .query_row(params![source_id, video_offset_ms], row_to_candidate)
            .optional()?)
    }

    pub fn get_candidate_near(
        &self,
        source_id: &str,
        video_offset_ms: u64,
        tolerance_ms: u64,
    ) -> Result<Option<StoredCandidateImage>, StorageError> {
        let start = video_offset_ms.saturating_sub(tolerance_ms);
        let end = video_offset_ms.saturating_add(tolerance_ms);
        let connection = self.connection()?;
        let mut statement = connection.prepare(
            "SELECT id, source_id, video_offset_ms, source_frame_number, selection_method,
                    parameters_json, image_path, thumbnail_path, width, height, pinned, created_at
             FROM candidate_images
             WHERE source_id=?1 AND video_offset_ms BETWEEN ?2 AND ?3
             ORDER BY ABS(video_offset_ms - ?4), video_offset_ms
             LIMIT 1",
        )?;
        Ok(statement
            .query_row(
                params![source_id, start, end, video_offset_ms],
                row_to_candidate,
            )
            .optional()?)
    }

    pub fn get_candidate(&self, id: &str) -> Result<Option<StoredCandidateImage>, StorageError> {
        let connection = self.connection()?;
        let mut statement = connection.prepare(
            "SELECT id, source_id, video_offset_ms, source_frame_number, selection_method,
                    parameters_json, image_path, thumbnail_path, width, height, pinned, created_at
             FROM candidate_images WHERE id=?1",
        )?;
        Ok(statement
            .query_row(params![id], row_to_candidate)
            .optional()?)
    }

    pub fn list_candidates(
        &self,
        source_id: &str,
        offset: u32,
        limit: u32,
    ) -> Result<Vec<StoredCandidateImage>, StorageError> {
        let connection = self.connection()?;
        let mut statement = connection.prepare(
            "SELECT id, source_id, video_offset_ms, source_frame_number, selection_method,\n                    parameters_json, image_path, thumbnail_path, width, height, pinned, created_at\n             FROM candidate_images WHERE source_id=?1 ORDER BY video_offset_ms LIMIT ?2 OFFSET ?3",
        )?;
        let rows = statement.query_map(params![source_id, limit, offset], row_to_candidate)?;
        Ok(rows.collect::<Result<Vec<_>, _>>()?)
    }

    pub fn candidate_count(&self) -> Result<u64, StorageError> {
        Ok(self
            .connection()?
            .query_row("SELECT COUNT(*) FROM candidate_images", [], |row| {
                row.get(0)
            })?)
    }

    pub fn delete_candidates(
        &self,
        source_id: &str,
        candidate_ids: Option<&[String]>,
    ) -> Result<Vec<StoredCandidateImage>, StorageError> {
        let requested = candidate_ids.map(|ids| ids.iter().collect::<HashSet<_>>());
        let candidates = self
            .list_candidates(source_id, 0, u32::MAX)?
            .into_iter()
            .filter(|candidate| {
                requested
                    .as_ref()
                    .is_none_or(|ids| ids.contains(&candidate.id))
            })
            .collect::<Vec<_>>();
        if candidates.is_empty() {
            return Ok(candidates);
        }
        let mut connection = self.connection()?;
        let transaction = connection.transaction()?;
        for candidate in &candidates {
            transaction.execute(
                "DELETE FROM candidate_images WHERE id=?1 AND source_id=?2",
                params![candidate.id, source_id],
            )?;
        }
        transaction.commit()?;
        Ok(candidates)
    }

    pub fn delete_source(&self, source_id: &str) -> Result<bool, StorageError> {
        let mut connection = self.connection()?;
        let transaction = connection.transaction()?;
        transaction.execute(
            "DELETE FROM roi_profiles WHERE scope_kind='source' AND scope_value=?1",
            params![source_id],
        )?;
        let deleted =
            transaction.execute("DELETE FROM source_assets WHERE id=?1", params![source_id])?;
        transaction.commit()?;
        Ok(deleted > 0)
    }

    pub fn upsert_roi_profile(&self, profile: &StoredRoiProfile) -> Result<(), StorageError> {
        let mut connection = self.connection()?;
        let transaction = connection.transaction()?;
        transaction.execute("DELETE FROM roi_profiles WHERE id=?1", params![profile.id])?;
        transaction.execute(
            r#"INSERT INTO roi_profiles(
                id, scope_kind, scope_value, name, x, y, width, height,
                render_config_json, created_at, updated_at
            ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11)
            ON CONFLICT(scope_kind, scope_value, name) DO UPDATE SET
                id=excluded.id,
                x=excluded.x,
                y=excluded.y,
                width=excluded.width,
                height=excluded.height,
                render_config_json=excluded.render_config_json,
                updated_at=excluded.updated_at"#,
            params![
                profile.id,
                profile.scope_kind,
                profile.scope_value,
                profile.name,
                profile.x,
                profile.y,
                profile.width,
                profile.height,
                profile.render_config_json,
                profile.created_at,
                profile.updated_at,
            ],
        )?;
        transaction.commit()?;
        Ok(())
    }

    pub fn list_roi_profiles(
        &self,
        scope_kind: &str,
        scope_value: &str,
    ) -> Result<Vec<StoredRoiProfile>, StorageError> {
        let connection = self.connection()?;
        let mut statement = connection.prepare(
            "SELECT id, scope_kind, scope_value, name, x, y, width, height,
                    render_config_json, created_at, updated_at
             FROM roi_profiles
             WHERE scope_kind=?1 AND scope_value=?2
             ORDER BY name, id",
        )?;
        let rows = statement.query_map(params![scope_kind, scope_value], row_to_roi_profile)?;
        Ok(rows.collect::<Result<Vec<_>, _>>()?)
    }

    pub fn delete_roi_profile(&self, id: &str) -> Result<bool, StorageError> {
        Ok(self
            .connection()?
            .execute("DELETE FROM roi_profiles WHERE id=?1", params![id])?
            > 0)
    }

    pub fn reset_review_analysis(&self) -> Result<(), StorageError> {
        let mut connection = self.connection()?;
        let transaction = connection.transaction()?;
        transaction.execute("DELETE FROM quality_assessments", [])?;
        transaction.execute(
            "UPDATE review_assets SET automatic_status='keep', automatic_reasons_json='[]',
             similarity_group_id=NULL, similarity_score=NULL, representative=0,
             locked_conflict=0",
            [],
        )?;
        transaction.commit()?;
        Ok(())
    }

    pub fn upsert_quality_assessment(
        &self,
        assessment: &StoredQualityAssessment,
    ) -> Result<(), StorageError> {
        self.connection()?.execute(
            r#"INSERT INTO quality_assessments(
                asset_key, source_id, candidate_id, width, height, aspect_ratio,
                sharpness, underexposed_ratio, overexposed_ratio, entropy,
                low_information, content_sha256, perceptual_hash, algorithm_version,
                analyzed_at, decode_error
            ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14, ?15, ?16)
            ON CONFLICT(asset_key) DO UPDATE SET
                source_id=excluded.source_id, candidate_id=excluded.candidate_id,
                width=excluded.width, height=excluded.height,
                aspect_ratio=excluded.aspect_ratio, sharpness=excluded.sharpness,
                underexposed_ratio=excluded.underexposed_ratio,
                overexposed_ratio=excluded.overexposed_ratio, entropy=excluded.entropy,
                low_information=excluded.low_information,
                content_sha256=excluded.content_sha256,
                perceptual_hash=excluded.perceptual_hash,
                algorithm_version=excluded.algorithm_version,
                analyzed_at=excluded.analyzed_at, decode_error=excluded.decode_error"#,
            params![
                assessment.asset_key,
                assessment.source_id,
                assessment.candidate_id,
                assessment.width,
                assessment.height,
                assessment.aspect_ratio,
                assessment.sharpness,
                assessment.underexposed_ratio,
                assessment.overexposed_ratio,
                assessment.entropy,
                assessment.low_information,
                assessment.content_sha256,
                assessment.perceptual_hash,
                assessment.algorithm_version,
                assessment.analyzed_at,
                assessment.decode_error,
            ],
        )?;
        Ok(())
    }

    pub fn upsert_review_asset(&self, asset: &StoredReviewAsset) -> Result<(), StorageError> {
        self.connection()?.execute(
            r#"INSERT INTO review_assets(
                asset_key, source_id, candidate_id, automatic_status,
                automatic_reasons_json, manual_decision, locked,
                similarity_group_id, similarity_score, representative,
                locked_conflict, updated_at
            ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12)
            ON CONFLICT(asset_key) DO UPDATE SET
                source_id=excluded.source_id, candidate_id=excluded.candidate_id,
                automatic_status=excluded.automatic_status,
                automatic_reasons_json=excluded.automatic_reasons_json,
                similarity_group_id=excluded.similarity_group_id,
                similarity_score=excluded.similarity_score,
                representative=excluded.representative,
                locked_conflict=excluded.locked_conflict,
                updated_at=excluded.updated_at"#,
            params![
                asset.asset_key,
                asset.source_id,
                asset.candidate_id,
                asset.automatic_status,
                asset.automatic_reasons_json,
                asset.manual_decision,
                asset.locked,
                asset.similarity_group_id,
                asset.similarity_score,
                asset.representative,
                asset.locked_conflict,
                asset.updated_at,
            ],
        )?;
        Ok(())
    }

    pub fn list_quality_assessments(&self) -> Result<Vec<StoredQualityAssessment>, StorageError> {
        let connection = self.connection()?;
        let mut statement = connection.prepare(
            "SELECT asset_key, source_id, candidate_id, width, height, aspect_ratio,
                    sharpness, underexposed_ratio, overexposed_ratio, entropy,
                    low_information, content_sha256, perceptual_hash, algorithm_version,
                    analyzed_at, decode_error
             FROM quality_assessments ORDER BY asset_key",
        )?;
        let rows = statement.query_map([], row_to_quality_assessment)?;
        Ok(rows.collect::<Result<Vec<_>, _>>()?)
    }

    pub fn list_review_assets(&self) -> Result<Vec<StoredReviewAsset>, StorageError> {
        let connection = self.connection()?;
        let mut statement = connection.prepare(
            "SELECT asset_key, source_id, candidate_id, automatic_status,
                    automatic_reasons_json, manual_decision, locked,
                    similarity_group_id, similarity_score, representative,
                    locked_conflict, updated_at
             FROM review_assets ORDER BY asset_key",
        )?;
        let rows = statement.query_map([], row_to_review_asset)?;
        Ok(rows.collect::<Result<Vec<_>, _>>()?)
    }

    pub fn get_review_asset(
        &self,
        asset_key: &str,
    ) -> Result<Option<StoredReviewAsset>, StorageError> {
        let connection = self.connection()?;
        Ok(connection
            .query_row(
                "SELECT asset_key, source_id, candidate_id, automatic_status,
                        automatic_reasons_json, manual_decision, locked,
                        similarity_group_id, similarity_score, representative,
                        locked_conflict, updated_at
                 FROM review_assets WHERE asset_key=?1",
                params![asset_key],
                row_to_review_asset,
            )
            .optional()?)
    }

    pub fn set_review_state(
        &self,
        asset_key: &str,
        manual_decision: Option<&str>,
        locked: bool,
        updated_at: &str,
    ) -> Result<bool, StorageError> {
        Ok(self.connection()?.execute(
            "UPDATE review_assets SET manual_decision=?2, locked=?3, updated_at=?4 WHERE asset_key=?1",
            params![asset_key, manual_decision, locked, updated_at],
        )? > 0)
    }

    pub fn set_group_representative(
        &self,
        group_id: &str,
        asset_key: &str,
        updated_at: &str,
    ) -> Result<bool, StorageError> {
        let mut connection = self.connection()?;
        let transaction = connection.transaction()?;
        let belongs = transaction.query_row(
            "SELECT EXISTS(SELECT 1 FROM review_assets WHERE asset_key=?1 AND similarity_group_id=?2)",
            params![asset_key, group_id],
            |row| row.get::<_, bool>(0),
        )?;
        if !belongs {
            return Ok(false);
        }
        transaction.execute(
            "UPDATE review_assets SET representative=CASE WHEN asset_key=?2 THEN 1 ELSE 0 END,
             automatic_status=CASE WHEN asset_key=?2 THEN 'keep' WHEN locked=1 THEN 'warning' ELSE 'suggest_exclude' END,
             updated_at=?3 WHERE similarity_group_id=?1",
            params![group_id, asset_key, updated_at],
        )?;
        transaction.commit()?;
        Ok(true)
    }

    pub fn insert_review_audit(
        &self,
        id: &str,
        asset_key: &str,
        action: &str,
        before_json: &str,
        after_json: &str,
        actor: &str,
        created_at: &str,
    ) -> Result<(), StorageError> {
        let mut connection = self.connection()?;
        let transaction = connection.transaction()?;
        transaction.execute("DELETE FROM review_redo_events", [])?;
        transaction.execute(
            "INSERT INTO review_audit_events(id, asset_key, action, before_json, after_json, actor, created_at)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)",
            params![id, asset_key, action, before_json, after_json, actor, created_at],
        )?;
        transaction.commit()?;
        Ok(())
    }

    pub fn latest_undoable_review_audit(
        &self,
    ) -> Result<Option<StoredReviewAuditEvent>, StorageError> {
        let connection = self.connection()?;
        Ok(connection.query_row(
            "SELECT id, asset_key, action, before_json, after_json
             FROM review_audit_events
             WHERE action != 'make_representative'
               AND NOT EXISTS(SELECT 1 FROM review_redo_events WHERE audit_id=review_audit_events.id)
             ORDER BY rowid DESC LIMIT 1",
            [],
            row_to_review_audit,
        ).optional()?)
    }

    pub fn latest_redo_review_audit(&self) -> Result<Option<StoredReviewAuditEvent>, StorageError> {
        let connection = self.connection()?;
        Ok(connection
            .query_row(
                "SELECT review_audit_events.id, asset_key, action, before_json, after_json
             FROM review_redo_events JOIN review_audit_events ON audit_id=review_audit_events.id
             ORDER BY review_redo_events.rowid DESC LIMIT 1",
                [],
                row_to_review_audit,
            )
            .optional()?)
    }

    pub fn mark_review_audit_undone(
        &self,
        audit_id: &str,
        moved_at: &str,
    ) -> Result<(), StorageError> {
        self.connection()?.execute(
            "INSERT OR REPLACE INTO review_redo_events(audit_id, moved_at) VALUES (?1, ?2)",
            params![audit_id, moved_at],
        )?;
        Ok(())
    }

    pub fn mark_review_audit_redone(&self, audit_id: &str) -> Result<(), StorageError> {
        self.connection()?.execute(
            "DELETE FROM review_redo_events WHERE audit_id=?1",
            params![audit_id],
        )?;
        Ok(())
    }

    pub fn counts(&self) -> Result<(u64, u64), StorageError> {
        let connection = self.connection()?;
        Ok(connection.query_row(
            "SELECT COUNT(*), SUM(CASE WHEN status != 'online' THEN 1 ELSE 0 END) FROM source_assets",
            [],
            |row| Ok((row.get(0)?, row.get::<_, Option<u64>>(1)?.unwrap_or(0))),
        )?)
    }

    pub fn backup_to(&self, destination: impl AsRef<Path>) -> Result<(), StorageError> {
        let destination = destination.as_ref();
        let temporary = destination.with_extension("tmp");
        if temporary.is_file() {
            std::fs::remove_file(&temporary)?;
        }
        self.connection()?.execute(
            "VACUUM INTO ?1",
            params![temporary.to_string_lossy().into_owned()],
        )?;
        if destination.is_file() {
            std::fs::remove_file(destination)?;
        }
        std::fs::rename(temporary, destination)?;
        Ok(())
    }
}

fn apply_migrations(connection: &Connection) -> Result<(), StorageError> {
    connection.execute_batch(MIGRATION_1)?;
    connection.execute(
        "INSERT OR IGNORE INTO schema_migrations(version) VALUES (?1)",
        params![1_i64],
    )?;
    connection.execute_batch(MIGRATION_2)?;
    connection.execute(
        "INSERT OR IGNORE INTO schema_migrations(version) VALUES (?1)",
        params![2_i64],
    )?;
    connection.execute_batch(MIGRATION_3)?;
    connection.execute(
        "INSERT OR IGNORE INTO schema_migrations(version) VALUES (?1)",
        params![3_i64],
    )?;
    connection.execute_batch(MIGRATION_4)?;
    connection.execute(
        "INSERT OR IGNORE INTO schema_migrations(version) VALUES (?1)",
        params![4_i64],
    )?;
    connection.execute_batch(MIGRATION_5)?;
    connection.execute(
        "INSERT OR IGNORE INTO schema_migrations(version) VALUES (?1)",
        params![5_i64],
    )?;
    Ok(())
}

fn kind_text(kind: SourceKind) -> &'static str {
    match kind {
        SourceKind::Image => "image",
        SourceKind::Video => "video",
    }
}

fn status_text(status: SourceStatus) -> &'static str {
    match status {
        SourceStatus::Online => "online",
        SourceStatus::Offline => "offline",
        SourceStatus::Error => "error",
    }
}

fn upsert_source_on(
    connection: &Connection,
    asset: &StoredSourceAsset,
) -> Result<bool, StorageError> {
    let existed = connection
        .query_row(
            "SELECT 1 FROM source_assets WHERE id = ?1",
            params![asset.id],
            |_| Ok(()),
        )
        .optional()?
        .is_some();
    connection.execute(
        r#"INSERT INTO source_assets (
            id, absolute_path, file_name, relative_folder, source_group, source_identifier,
            kind, status, size_bytes, modified_unix_ms, quick_fingerprint, sha256, width,
            height, duration_ms, codec, frame_rate, capture_time, capture_time_source,
            orientation, thumbnail_path, error, imported_at, last_checked_at
        ) VALUES (
            ?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14, ?15, ?16,
            ?17, ?18, ?19, ?20, ?21, ?22, ?23, ?24
        ) ON CONFLICT(id) DO UPDATE SET
            absolute_path=excluded.absolute_path, file_name=excluded.file_name,
            relative_folder=excluded.relative_folder,
            source_group=excluded.source_group, source_identifier=excluded.source_identifier,
            kind=excluded.kind, status=excluded.status, size_bytes=excluded.size_bytes,
            modified_unix_ms=excluded.modified_unix_ms,
            quick_fingerprint=excluded.quick_fingerprint,
            sha256=COALESCE(excluded.sha256, source_assets.sha256), width=excluded.width,
            height=excluded.height, duration_ms=excluded.duration_ms, codec=excluded.codec,
            frame_rate=excluded.frame_rate, capture_time=excluded.capture_time,
            capture_time_source=excluded.capture_time_source, orientation=excluded.orientation,
            thumbnail_path=excluded.thumbnail_path, error=excluded.error,
            last_checked_at=excluded.last_checked_at"#,
        params![
            asset.id,
            asset.absolute_path,
            asset.file_name,
            asset.relative_folder,
            asset.source_group,
            asset.source_identifier,
            kind_text(asset.kind),
            status_text(asset.status),
            asset.size_bytes,
            asset.modified_unix_ms,
            asset.quick_fingerprint,
            asset.sha256,
            asset.width,
            asset.height,
            asset.duration_ms,
            asset.codec,
            asset.frame_rate,
            asset.capture_time,
            asset.capture_time_source,
            asset.orientation,
            asset.thumbnail_path,
            asset.error,
            asset.imported_at,
            asset.last_checked_at,
        ],
    )?;
    Ok(existed)
}

fn row_to_asset(row: &rusqlite::Row<'_>) -> rusqlite::Result<StoredSourceAsset> {
    let kind: String = row.get(6)?;
    let status: String = row.get(7)?;
    Ok(StoredSourceAsset {
        id: row.get(0)?,
        absolute_path: row.get(1)?,
        file_name: row.get(2)?,
        relative_folder: row.get(3)?,
        source_group: row.get(4)?,
        source_identifier: row.get(5)?,
        kind: if kind == "video" {
            SourceKind::Video
        } else {
            SourceKind::Image
        },
        status: match status.as_str() {
            "offline" => SourceStatus::Offline,
            "error" => SourceStatus::Error,
            _ => SourceStatus::Online,
        },
        size_bytes: row.get(8)?,
        modified_unix_ms: row.get(9)?,
        quick_fingerprint: row.get(10)?,
        sha256: row.get(11)?,
        width: row.get(12)?,
        height: row.get(13)?,
        duration_ms: row.get(14)?,
        codec: row.get(15)?,
        frame_rate: row.get(16)?,
        capture_time: row.get(17)?,
        capture_time_source: row.get(18)?,
        orientation: row.get(19)?,
        thumbnail_path: row.get(20)?,
        error: row.get(21)?,
        imported_at: row.get(22)?,
        last_checked_at: row.get(23)?,
    })
}

fn row_to_candidate(row: &rusqlite::Row<'_>) -> rusqlite::Result<StoredCandidateImage> {
    Ok(StoredCandidateImage {
        id: row.get(0)?,
        source_id: row.get(1)?,
        video_offset_ms: row.get(2)?,
        source_frame_number: row.get(3)?,
        selection_method: row.get(4)?,
        parameters_json: row.get(5)?,
        image_path: row.get(6)?,
        thumbnail_path: row.get(7)?,
        width: row.get(8)?,
        height: row.get(9)?,
        pinned: row.get(10)?,
        created_at: row.get(11)?,
    })
}

fn row_to_roi_profile(row: &rusqlite::Row<'_>) -> rusqlite::Result<StoredRoiProfile> {
    Ok(StoredRoiProfile {
        id: row.get(0)?,
        scope_kind: row.get(1)?,
        scope_value: row.get(2)?,
        name: row.get(3)?,
        x: row.get(4)?,
        y: row.get(5)?,
        width: row.get(6)?,
        height: row.get(7)?,
        render_config_json: row.get(8)?,
        created_at: row.get(9)?,
        updated_at: row.get(10)?,
    })
}

fn row_to_quality_assessment(row: &rusqlite::Row<'_>) -> rusqlite::Result<StoredQualityAssessment> {
    Ok(StoredQualityAssessment {
        asset_key: row.get(0)?,
        source_id: row.get(1)?,
        candidate_id: row.get(2)?,
        width: row.get(3)?,
        height: row.get(4)?,
        aspect_ratio: row.get(5)?,
        sharpness: row.get(6)?,
        underexposed_ratio: row.get(7)?,
        overexposed_ratio: row.get(8)?,
        entropy: row.get(9)?,
        low_information: row.get(10)?,
        content_sha256: row.get(11)?,
        perceptual_hash: row.get(12)?,
        algorithm_version: row.get(13)?,
        analyzed_at: row.get(14)?,
        decode_error: row.get(15)?,
    })
}

fn row_to_review_asset(row: &rusqlite::Row<'_>) -> rusqlite::Result<StoredReviewAsset> {
    Ok(StoredReviewAsset {
        asset_key: row.get(0)?,
        source_id: row.get(1)?,
        candidate_id: row.get(2)?,
        automatic_status: row.get(3)?,
        automatic_reasons_json: row.get(4)?,
        manual_decision: row.get(5)?,
        locked: row.get(6)?,
        similarity_group_id: row.get(7)?,
        similarity_score: row.get(8)?,
        representative: row.get(9)?,
        locked_conflict: row.get(10)?,
        updated_at: row.get(11)?,
    })
}

fn row_to_review_audit(row: &rusqlite::Row<'_>) -> rusqlite::Result<StoredReviewAuditEvent> {
    Ok(StoredReviewAuditEvent {
        id: row.get(0)?,
        asset_key: row.get(1)?,
        action: row.get(2)?,
        before_json: row.get(3)?,
        after_json: row.get(4)?,
    })
}

pub fn probe_in_memory() -> Result<StorageProbe, StorageError> {
    let connection = Connection::open_in_memory()?;
    apply_migrations(&connection)?;
    let sqlite_version = connection.query_row("SELECT sqlite_version()", [], |row| row.get(0))?;
    let schema_version = connection.query_row(
        "SELECT COALESCE(MAX(version), 0) FROM schema_migrations",
        [],
        |row| row.get(0),
    )?;
    Ok(StorageProbe {
        sqlite_version,
        schema_version,
    })
}

#[derive(Debug, Error)]
pub enum StorageError {
    #[error("SQLite operation failed: {0}")]
    Sqlite(#[from] rusqlite::Error),
    #[error("backup file operation failed: {0}")]
    Io(#[from] std::io::Error),
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn applies_initial_schema() {
        let probe = probe_in_memory().expect("in-memory SQLite should initialize");
        assert_eq!(probe.schema_version, 5);
        assert!(!probe.sqlite_version.is_empty());
    }

    #[test]
    fn paginates_one_hundred_thousand_candidate_rows() {
        let mut connection = Connection::open_in_memory().unwrap();
        connection
            .execute_batch(
                "CREATE TABLE candidate_probe (id INTEGER PRIMARY KEY, source_id INTEGER NOT NULL, timestamp_ms INTEGER NOT NULL);",
            )
            .unwrap();
        let transaction = connection.transaction().unwrap();
        {
            let mut insert = transaction
                .prepare(
                    "INSERT INTO candidate_probe(id, source_id, timestamp_ms) VALUES (?1, ?2, ?3)",
                )
                .unwrap();
            for id in 1_i64..=100_000 {
                insert.execute(params![id, id % 100, id * 40]).unwrap();
            }
        }
        transaction.commit().unwrap();
        let page: Vec<i64> = connection
            .prepare("SELECT id FROM candidate_probe ORDER BY id LIMIT 50 OFFSET 99950")
            .unwrap()
            .query_map([], |row| row.get(0))
            .unwrap()
            .collect::<Result<_, _>>()
            .unwrap();
        assert_eq!(page.len(), 50);
        assert_eq!(page[0], 99_951);
        assert_eq!(page[49], 100_000);
    }
}
