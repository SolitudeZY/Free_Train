use std::path::{Path, PathBuf};

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
        assert_eq!(probe.schema_version, 2);
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
