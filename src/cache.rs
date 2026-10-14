use std::path::{Path, PathBuf};
use std::time::UNIX_EPOCH;

use anyhow::{Context, Result};
use rusqlite::{params, params_from_iter, types::Value, Connection, OptionalExtension};

use crate::domain::{Entry, EntryId, Tag, TagId};

const SCHEMA_VERSION: &str = "2";

/// Fast metadata signature for the canonical TOML inputs.
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

pub(crate) struct SqliteCache {
    path: PathBuf,
}

impl SqliteCache {
    pub(crate) fn new(data_dir: &Path) -> Self {
        Self {
            path: data_dir.join("cache.sqlite3"),
        }
    }

    pub(crate) fn invalidate(&self) {
        for suffix in ["", "-journal", "-wal", "-shm"] {
            let mut path = self.path.as_os_str().to_os_string();
            path.push(suffix);
            let _ = std::fs::remove_file(PathBuf::from(path));
        }
    }

    pub(crate) fn query_entries(
        &self,
        signature: &SourceSignature,
        tag_ids: &[TagId],
        archiver: ArchiverFilter<'_>,
    ) -> Result<Option<Vec<Entry>>> {
        if !self.path.exists() {
            return Ok(None);
        }

        let connection = Connection::open(&self.path)
            .with_context(|| format!("Failed to open cache {}", self.path.display()))?;
        let state = connection
            .query_row(
                "SELECT value FROM cache_meta WHERE key = 'schema_version'",
                [],
                |row| row.get::<_, String>(0),
            )
            .optional()?;
        if state.as_deref() != Some(SCHEMA_VERSION) {
            return Ok(None);
        }
        let cached_tags = read_meta(&connection, "tags_signature")?;
        let cached_entries = read_meta(&connection, "entries_signature")?;
        if cached_tags.as_deref() != Some(signature.tags.as_str())
            || cached_entries.as_deref() != Some(signature.entries.as_str())
        {
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
            let first = values.len() + 1;
            let placeholders = (first..first + unique_tag_ids.len())
                .map(|index| format!("?{index}"))
                .collect::<Vec<_>>()
                .join(",");
            for id in &unique_tag_ids {
                values.push(Value::Text(id.to_string()));
            }
            conditions.push(format!(
                "e.id IN (SELECT entry_id FROM entry_tags WHERE tag_id IN ({placeholders}) GROUP BY entry_id HAVING COUNT(DISTINCT tag_id) = {})",
                unique_tag_ids.len()
            ));
        }

        let where_clause = if conditions.is_empty() {
            String::new()
        } else {
            format!("WHERE {}", conditions.join(" AND "))
        };
        let sql = format!(
            "SELECT e.id, e.url, e.archiver, e.archive_path, e.description, \
             (SELECT GROUP_CONCAT(tag_id, '|') FROM (
                 SELECT tag_id FROM entry_tags WHERE entry_id = e.id ORDER BY ordinal
             )) \
             FROM entries e {where_clause} ORDER BY e.ordinal"
        );
        let mut statement = connection.prepare(&sql)?;
        let rows = statement.query_map(params_from_iter(values), |row| {
            Ok((
                row.get::<_, String>(0)?,
                row.get::<_, String>(1)?,
                row.get::<_, Option<String>>(2)?,
                row.get::<_, Option<String>>(3)?,
                row.get::<_, String>(4)?,
                row.get::<_, Option<String>>(5)?,
            ))
        })?;

        let mut entries = Vec::new();
        for row in rows {
            let (id, url, archiver, archive_path, description, tag_ids) = row?;
            let tag_ids = tag_ids
                .unwrap_or_default()
                .split('|')
                .filter(|id| !id.is_empty())
                .map(str::parse::<TagId>)
                .collect::<std::result::Result<Vec<_>, _>>()
                .context("Invalid tag ID in SQLite cache")?;
            entries.push(Entry {
                id: id
                    .parse::<EntryId>()
                    .context("Invalid entry ID in SQLite cache")?,
                url,
                archiver,
                archive_path,
                description,
                tag_ids,
            });
        }
        Ok(Some(entries))
    }

    pub(crate) fn rebuild(
        &self,
        signature: &SourceSignature,
        tags: &[Tag],
        entries: &[Entry],
    ) -> Result<()> {
        if let Err(first_error) = self.rebuild_once(signature, tags, entries) {
            self.invalidate();
            self.rebuild_once(signature, tags, entries)
                .with_context(|| {
                    format!("Failed to rebuild SQLite cache after: {first_error:#}")
                })?;
        }
        Ok(())
    }

    fn rebuild_once(
        &self,
        signature: &SourceSignature,
        tags: &[Tag],
        entries: &[Entry],
    ) -> Result<()> {
        let mut connection = Connection::open(&self.path)
            .with_context(|| format!("Failed to create cache {}", self.path.display()))?;
        connection.execute_batch(
            "PRAGMA journal_mode = DELETE;
             CREATE TABLE IF NOT EXISTS cache_meta (key TEXT PRIMARY KEY, value TEXT NOT NULL);
             CREATE TABLE IF NOT EXISTS tags (
                 id TEXT PRIMARY KEY, name TEXT NOT NULL, specifier TEXT
             );
             CREATE TABLE IF NOT EXISTS entries (
                 id TEXT PRIMARY KEY, ordinal INTEGER NOT NULL, url TEXT NOT NULL,
                 archiver TEXT, archive_path TEXT, description TEXT NOT NULL
             );
             CREATE TABLE IF NOT EXISTS entry_tags (
                 entry_id TEXT NOT NULL, ordinal INTEGER NOT NULL, tag_id TEXT NOT NULL,
                 PRIMARY KEY (entry_id, ordinal)
             );
             CREATE INDEX IF NOT EXISTS idx_entry_tags_tag_entry ON entry_tags(tag_id, entry_id);
             CREATE INDEX IF NOT EXISTS idx_entries_archiver_ordinal ON entries(archiver, ordinal);",
        )?;

        let transaction = connection.transaction()?;
        transaction.execute("DELETE FROM entry_tags", [])?;
        transaction.execute("DELETE FROM entries", [])?;
        transaction.execute("DELETE FROM tags", [])?;
        transaction.execute("DELETE FROM cache_meta", [])?;
        {
            let mut statement = transaction
                .prepare("INSERT INTO tags (id, name, specifier) VALUES (?1, ?2, ?3)")?;
            for tag in tags {
                statement.execute(params![tag.id.to_string(), tag.name, tag.specifier])?;
            }
        }
        {
            let mut insert_entry = transaction.prepare(
                "INSERT INTO entries (id, ordinal, url, archiver, archive_path, description) \
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
            )?;
            let mut insert_tag = transaction.prepare(
                "INSERT INTO entry_tags (entry_id, ordinal, tag_id) VALUES (?1, ?2, ?3)",
            )?;
            for (ordinal, entry) in entries.iter().enumerate() {
                insert_entry.execute(params![
                    entry.id.to_string(),
                    ordinal as i64,
                    entry.url,
                    entry.archiver,
                    entry.archive_path,
                    entry.description,
                ])?;
                for (tag_ordinal, tag_id) in entry.tag_ids.iter().enumerate() {
                    insert_tag.execute(params![
                        entry.id.to_string(),
                        tag_ordinal as i64,
                        tag_id.to_string()
                    ])?;
                }
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
        transaction.commit()?;
        Ok(())
    }
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
