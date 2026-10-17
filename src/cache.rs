use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::thread::JoinHandle;
use std::time::UNIX_EPOCH;
use std::{
    collections::HashMap,
    io::{BufRead, BufReader},
    path::{Path, PathBuf},
};

use anyhow::{Context, Result};
use rusqlite::{params, params_from_iter, types::Value, Connection, OptionalExtension};

use crate::domain::{Entry, EntryId, Tag, TagId};

const SCHEMA_VERSION: &str = "4";

/// Fast metadata signature for the canonical JSONL inputs.
/// Store writes also remove the cache; this signature detects external edits.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct SourceSignature {
    tags: String,
    entries: String,
}

impl SourceSignature {
    pub(crate) fn read(tags_path: &Path, entries_path: &Path) -> Result<Self> {
        Ok(Self {
            tags: file_signature(tags_path)?,
            entries: file_signature(entries_path)?,
        })
    }
}

fn file_signature(path: &Path) -> Result<String> {
    match std::fs::metadata(path) {
        Ok(metadata) => {
            let modified = metadata
                .modified()
                .ok()
                .and_then(|time| time.duration_since(UNIX_EPOCH).ok())
                .map(|duration| duration.as_nanos())
                .unwrap_or_default();
            Ok(format!("{}:{modified}", metadata.len()))
        }
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok("missing".to_string()),
        Err(error) => Err(error).with_context(|| format!("Failed to stat {}", path.display())),
    }
}

#[derive(Debug, Clone, Copy)]
pub(crate) enum ArchiverFilter<'a> {
    Any,
    WithoutArchiver,
    Named(&'a str),
}

pub struct CacheReport {
    pub rebuilt: bool,
    pub entries: u64,
    pub tags: u64,
    pub bytes: u64,
}

/// Kept with sentinel pipes (`|id0|id1|`) so `split('|')` parsing stays
/// unambiguous for empty and missing tags.
fn tags_column(tag_ids: &[TagId]) -> String {
    let mut column = String::new();
    for tag_id in tag_ids {
        column.push('|');
        column.push_str(&tag_id.to_string());
    }
    column.push('|');
    column
}

fn parse_tags_column(column: &str) -> Result<Vec<TagId>> {
    column
        .split('|')
        .filter(|id| !id.is_empty())
        .map(str::parse::<TagId>)
        .collect::<std::result::Result<Vec<_>, _>>()
        .context("Invalid tag ID in SQLite cache")
}

pub(crate) struct SqliteCache {
    path: PathBuf,
    connection: Mutex<Option<Connection>>,
    rebuild_in_flight: Arc<AtomicBool>,
    rebuild_handle: Mutex<Option<JoinHandle<()>>>,
}

impl SqliteCache {
    pub(crate) fn new(data_dir: &Path) -> Self {
        let path = data_dir.join("cache.sqlite3");
        // Orphaned temporaries from interrupted rebuilds are never read back.
        if let Ok(files) = std::fs::read_dir(data_dir) {
            for file in files.flatten() {
                let name = file.file_name();
                let name = name.to_string_lossy();
                if name.starts_with("cache.sqlite3.tmp-") {
                    let _ = std::fs::remove_file(file.path());
                }
            }
        }
        Self {
            path,
            connection: Mutex::new(None),
            rebuild_in_flight: Arc::new(AtomicBool::new(false)),
            rebuild_handle: Mutex::new(None),
        }
    }

    /// Blocks until a background rebuild (if any) has settled.
    pub(crate) fn wait_for_rebuild(&self) {
        if let Some(handle) = self.rebuild_handle.lock().unwrap().take() {
            let _ = handle.join();
        }
    }

    pub(crate) fn invalidate(&self) {
        // Close the pooled connection before unlinking: otherwise it keeps
        // serving rows from the replaced (old-inode) cache file.
        *self.connection.lock().unwrap() = None;
        for suffix in ["", "-journal", "-wal", "-shm"] {
            let mut path = self.path.as_os_str().to_os_string();
            path.push(suffix);
            let _ = std::fs::remove_file(PathBuf::from(path));
        }
    }

    /// Start a rebuild in the background; nothing waits for it. The next miss
    /// in this process re-checks the flag, and later processes find the file.
    pub(crate) fn rebuild_in_background(
        &self,
        signature: SourceSignature,
        tags_path: PathBuf,
        entries_path: PathBuf,
    ) {
        if self.rebuild_in_flight.swap(true, Ordering::AcqRel) {
            return;
        }
        let flag = Arc::clone(&self.rebuild_in_flight);
        let path = self.path.clone();
        let handle = std::thread::Builder::new()
            .name("cache-rebuild".to_string())
            .spawn(move || {
                // Never leave the flag stuck on an unexpected panic.
                let _ = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                    let cache = Self {
                        path,
                        connection: Mutex::new(None),
                        rebuild_in_flight: Arc::clone(&flag),
                        rebuild_handle: Mutex::new(None),
                    };
                    let _ = cache.rebuild(&signature, &tags_path, &entries_path);
                }));
                flag.store(false, Ordering::Release);
            });
        match handle {
            Ok(handle) => *self.rebuild_handle.lock().unwrap() = Some(handle),
            Err(_) => self.rebuild_in_flight.store(false, Ordering::Release),
        }
    }

    pub(crate) fn warm(
        &self,
        signature: &SourceSignature,
        tags_path: &Path,
        entries_path: &Path,
    ) -> Result<CacheReport> {
        let rebuilt = !self.is_fresh(signature);
        if rebuilt {
            self.rebuild(signature, tags_path, entries_path)?;
            // The renamed-in file supersedes anything the pool may hold.
            *self.connection.lock().unwrap() = None;
        }
        let connection = Connection::open(&self.path)?;
        let entries =
            connection.query_row("SELECT COUNT(*) FROM entries", [], |row| {
                row.get::<_, i64>(0)
            })? as u64;
        let tags = connection.query_row("SELECT COUNT(*) FROM tags", [], |row| {
            row.get::<_, i64>(0)
        })? as u64;
        let bytes = std::fs::metadata(&self.path).map(|meta| meta.len()).unwrap_or(0);
        Ok(CacheReport {
            rebuilt,
            entries,
            tags,
            bytes,
        })
    }

    /// True when the on-disk cache exists, is current-schema, and was built
    /// from exactly this source signature.
    fn is_fresh(&self, signature: &SourceSignature) -> bool {
        if !self.path.exists() {
            return false;
        }
        let Ok(connection) = Connection::open(&self.path) else {
            return false;
        };
        let state = connection
            .query_row(
                "SELECT value FROM cache_meta WHERE key = 'schema_version'",
                [],
                |row| row.get::<_, String>(0),
            )
            .optional();
        let Ok(state) = state else {
            return false;
        };
        if state.as_deref() != Some(SCHEMA_VERSION) {
            return false;
        }
        let (Ok(cached_tags), Ok(cached_entries)) = (
            read_meta(&connection, "tags_signature"),
            read_meta(&connection, "entries_signature"),
        ) else {
            return false;
        };
        cached_tags.as_deref() == Some(signature.tags.as_str())
            && cached_entries.as_deref() == Some(signature.entries.as_str())
    }

    pub(crate) fn query_entries(
        &self,
        signature: &SourceSignature,
        tag_ids: &[TagId],
        archiver: ArchiverFilter<'_>,
    ) -> Result<Option<Vec<Entry>>> {
        let mut pooled = self.connection.lock().unwrap();
        let connection = match pooled.as_ref() {
            Some(connection) => connection,
            None => {
                if !self.path.exists() {
                    return Ok(None);
                }
                match Connection::open(&self.path)
                    .with_context(|| format!("Failed to open cache {}", self.path.display()))
                {
                    Ok(connection) => pooled.insert(connection),
                    Err(error) => {
                        *pooled = None;
                        return Err(error);
                    }
                }
            }
        };
        let state = match connection
            .query_row(
                "SELECT value FROM cache_meta WHERE key = 'schema_version'",
                [],
                |row| row.get::<_, String>(0),
            )
            .optional()
        {
            Ok(state) => state,
            // Corrupt or unreadable cache: reopen from scratch next time.
            Err(error) => {
                *pooled = None;
                return Err(error.into());
            }
        };
        if state.as_deref() != Some(SCHEMA_VERSION) {
            // Old generation on disk; let a future query see a rebuilt file
            // instead of pinning this obsolete inode in the pool.
            *pooled = None;
            return Ok(None);
        }
        let cached_tags = read_meta(connection, "tags_signature")?;
        let cached_entries = read_meta(connection, "entries_signature")?;
        if cached_tags.as_deref() != Some(signature.tags.as_str())
            || cached_entries.as_deref() != Some(signature.entries.as_str())
        {
            *pooled = None;
            return Ok(None);
        }

        let mut conditions = Vec::new();
        let mut values = Vec::new();
        match archiver {
            ArchiverFilter::Any => {}
            ArchiverFilter::WithoutArchiver => conditions.push("e.archiver IS NULL".to_string()),
            ArchiverFilter::Named(name) => {
                values.push(Value::Text(name.to_string()));
                conditions.push(format!("e.archiver = ?{}", values.len()));
            }
        }
        let unique_tag_ids: Vec<TagId> = {
            let mut ids = tag_ids.to_vec();
            ids.sort_by_key(|id| id.0);
            ids.dedup();
            ids
        };
        if !unique_tag_ids.is_empty() {
            // The tags table maps external IDs to small internal integers once
            // per query, so the hot subquery below never joins.
            let mut internal_ids = Vec::with_capacity(unique_tag_ids.len());
            for id in &unique_tag_ids {
                let internal_id = connection
                    .query_row(
                        "SELECT internal_tag_id FROM tags WHERE id = ?1",
                        [id.to_string()],
                        |row| row.get::<_, i64>(0),
                    )
                    .optional()?;
                match internal_id {
                    Some(internal_id) => internal_ids.push(internal_id),
                    // An unknown tag in an AND-of-all-tags filter matches nothing.
                    None => return Ok(Some(Vec::new())),
                }
            }
            let first = values.len() + 1;
            let placeholders = (first..first + internal_ids.len())
                .map(|index| format!("?{index}"))
                .collect::<Vec<_>>()
                .join(",");
            for internal_id in internal_ids {
                values.push(Value::Integer(internal_id));
            }
            conditions.push(format!(
                "e.internal_entry_id IN (SELECT et.internal_entry_id FROM entry_tags et \
                 WHERE et.internal_tag_id IN ({placeholders}) \
                 GROUP BY et.internal_entry_id \
                 HAVING COUNT(DISTINCT et.internal_tag_id) = {})",
                unique_tag_ids.len()
            ));
        }

        let where_clause = if conditions.is_empty() {
            String::new()
        } else {
            format!("WHERE {}", conditions.join(" AND "))
        };
        let sql = format!(
            "SELECT e.id, e.url, e.archiver, e.archive_path, e.description, e.tags \
             FROM entries e {where_clause} ORDER BY e.ordinal"
        );
        let mut statement = connection.prepare_cached(&sql)?;
        let rows = statement.query_map(params_from_iter(values), |row| {
            Ok((
                row.get::<_, String>(0)?,
                row.get::<_, String>(1)?,
                row.get::<_, Option<String>>(2)?,
                row.get::<_, Option<String>>(3)?,
                row.get::<_, String>(4)?,
                row.get::<_, String>(5)?,
            ))
        })?;

        let mut entries = Vec::new();
        for row in rows {
            let (id, url, archiver, archive_path, description, tags) = row?;
            entries.push(Entry {
                id: id
                    .parse::<EntryId>()
                    .context("Invalid entry ID in SQLite cache")?,
                url,
                archiver,
                archive_path,
                description,
                tag_ids: parse_tags_column(&tags)?,
            });
        }
        Ok(Some(entries))
    }

    pub(crate) fn rebuild(
        &self,
        signature: &SourceSignature,
        tags_path: &Path,
        entries_path: &Path,
    ) -> Result<()> {
        if let Err(first_error) = self.rebuild_once(signature, tags_path, entries_path) {
            self.rebuild_once(signature, tags_path, entries_path)
                .with_context(|| {
                    format!("Failed to rebuild SQLite cache after: {first_error:#}")
                })?;
        }
        Ok(())
    }

    fn rebuild_once(
        &self,
        signature: &SourceSignature,
        tags_path: &Path,
        entries_path: &Path,
    ) -> Result<()> {
        let temporary_path = self.temporary_path();
        let result = self.rebuild_into(&temporary_path, signature, tags_path, entries_path);
        if let Err(error) = result {
            remove_sqlite_files(&temporary_path);
            return Err(error);
        }

        if SourceSignature::read(tags_path, entries_path)? != *signature {
            remove_sqlite_files(&temporary_path);
            anyhow::bail!("JSONL source files changed during cache rebuild");
        }

        if let Err(error) = std::fs::rename(&temporary_path, &self.path) {
            remove_sqlite_files(&temporary_path);
            return Err(error).with_context(|| {
                format!(
                    "Failed to replace cache {} with {}",
                    self.path.display(),
                    temporary_path.display()
                )
            });
        }
        remove_sqlite_sidecars(&self.path);
        Ok(())
    }

    fn temporary_path(&self) -> PathBuf {
        let mut path = self.path.as_os_str().to_os_string();
        path.push(format!(".tmp-{}", uuid::Uuid::now_v7()));
        PathBuf::from(path)
    }

    fn rebuild_into(
        &self,
        path: &Path,
        signature: &SourceSignature,
        tags_path: &Path,
        entries_path: &Path,
    ) -> Result<()> {
        let mut connection = Connection::open(path)
            .with_context(|| format!("Failed to create cache {}", path.display()))?;
        // The temporary file is discarded wholesale on error and only becomes
        // authoritative through the atomic rename in `rebuild_once`, so
        // journaling and synchronous fixes only slow the build down.
        connection.execute_batch(
            "PRAGMA journal_mode = OFF;
             PRAGMA synchronous = OFF;
             PRAGMA cache_size = -131072;
             CREATE TABLE cache_meta (key TEXT PRIMARY KEY, value TEXT NOT NULL);
             CREATE TABLE tags (
                 internal_tag_id INTEGER PRIMARY KEY, id TEXT NOT NULL UNIQUE,
                 name TEXT NOT NULL, specifier TEXT
             );
             CREATE TABLE entries (
                 internal_entry_id INTEGER PRIMARY KEY, id TEXT NOT NULL UNIQUE,
                 ordinal INTEGER NOT NULL, url TEXT NOT NULL,
                 archiver TEXT, archive_path TEXT, description TEXT NOT NULL,
                 tags TEXT NOT NULL
             );
             CREATE TABLE entry_tags (
                 internal_entry_id INTEGER NOT NULL, internal_tag_id INTEGER NOT NULL
             );",
        )?;

        let transaction = connection.transaction()?;
        let tag_ids = insert_tags(&transaction, tags_path)?;
        {
            let mut insert_entry = transaction.prepare(
                "INSERT INTO entries (id, ordinal, url, archiver, archive_path, description, tags) \
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)",
            )?;
            let mut insert_tag = transaction
                .prepare("INSERT INTO entry_tags (internal_entry_id, internal_tag_id) VALUES (?1, ?2)")?;
            let file = std::fs::File::open(entries_path)
                .with_context(|| format!("Failed to open {}", entries_path.display()))?;
            let mut reader = BufReader::new(file);
            let mut line = String::new();
            let mut ordinal: i64 = 0;
            loop {
                line.clear();
                let read = reader.read_line(&mut line).with_context(|| {
                    format!("Failed to read {} near line {}", entries_path.display(), ordinal + 1)
                })?;
                if read == 0 {
                    break;
                }
                if line.trim().is_empty() {
                    continue;
                }
                let entry: Entry = serde_json::from_str(&line).with_context(|| {
                    format!(
                        "Failed to parse {} at line {}",
                        entries_path.display(),
                        ordinal + 1
                    )
                })?;
                insert_entry.execute(params![
                    entry.id.to_string(),
                    ordinal,
                    entry.url,
                    entry.archiver,
                    entry.archive_path,
                    entry.description,
                    tags_column(&entry.tag_ids),
                ])?;
                let internal_entry_id = transaction.last_insert_rowid();
                for tag_id in &entry.tag_ids {
                    let internal_tag_id = tag_ids.get(tag_id).with_context(|| {
                        format!("Entry {} references unknown tag {}", entry.id, tag_id)
                    })?;
                    insert_tag.execute(params![internal_entry_id, internal_tag_id])?;
                }
                ordinal += 1;
            }
        }
        transaction.execute(
            "INSERT INTO cache_meta (key, value) VALUES ('schema_version', ?1)",
            [SCHEMA_VERSION],
        )?;
        transaction.execute(
            "INSERT INTO cache_meta (key, value) VALUES ('tags_signature', ?1)",
            [&signature.tags],
        )?;
        transaction.execute(
            "INSERT INTO cache_meta (key, value) VALUES ('entries_signature', ?1)",
            [&signature.entries],
        )?;
        transaction.execute_batch(
            "CREATE INDEX idx_entry_tags_tag_entry ON entry_tags(internal_tag_id, internal_entry_id);
             CREATE INDEX idx_entries_archiver_ordinal ON entries(archiver, ordinal);",
        )?;
        transaction.commit()?;
        Ok(())
    }
}

fn insert_tags(
    transaction: &rusqlite::Transaction<'_>,
    tags_path: &Path,
) -> Result<HashMap<TagId, i64>> {
    let file = std::fs::File::open(tags_path)
        .with_context(|| format!("Failed to open {}", tags_path.display()))?;
    let mut ids = HashMap::new();
    let mut statement =
        transaction.prepare("INSERT INTO tags (id, name, specifier) VALUES (?1, ?2, ?3)")?;
    for (index, line) in BufReader::new(file).lines().enumerate() {
        let line = line.with_context(|| {
            format!(
                "Failed to read {} at line {}",
                tags_path.display(),
                index + 1
            )
        })?;
        if line.trim().is_empty() {
            continue;
        }
        let tag: Tag = serde_json::from_str(&line).with_context(|| {
            format!(
                "Failed to parse {} at line {}",
                tags_path.display(),
                index + 1
            )
        })?;
        statement.execute(params![tag.id.to_string(), tag.name, tag.specifier])?;
        let internal_id = transaction.last_insert_rowid();
        ids.insert(tag.id, internal_id);
    }
    Ok(ids)
}

fn remove_sqlite_sidecars(path: &Path) {
    for suffix in ["-journal", "-wal", "-shm"] {
        let mut sidecar = path.as_os_str().to_os_string();
        sidecar.push(suffix);
        let _ = std::fs::remove_file(PathBuf::from(sidecar));
    }
}

fn remove_sqlite_files(path: &Path) {
    let _ = std::fs::remove_file(path);
    remove_sqlite_sidecars(path);
}

fn read_meta(connection: &Connection, key: &str) -> Result<Option<String>> {
    Ok(connection
        .query_row(
            "SELECT value FROM cache_meta WHERE key = ?1",
            [key],
            |row| row.get(0),
        )
        .optional()?)
}
