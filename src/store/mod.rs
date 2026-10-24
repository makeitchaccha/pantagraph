use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};

use crate::db;
use crate::domain::{Entry, Tag, TagId};
use crate::query::{Query, ResolvedQuery};
use anyhow::{Context, Result};
use rusqlite::{params, Connection};

pub(crate) enum ArchiverFilter<'a> {
    Any,
    WithoutArchiver,
    Named(&'a str),
}

fn to_archiver_filter<'a>(archiver: Option<Option<&'a str>>) -> ArchiverFilter<'a> {
    match archiver {
        None => ArchiverFilter::Any,
        Some(None) => ArchiverFilter::WithoutArchiver,
        Some(Some(name)) => ArchiverFilter::Named(name),
    }
}

/// Render a meta attribute condition as a SQL boolean expression over
/// table alias `alias` (e.g. `e`).
///
/// `value = None` checks that the key is present with any value;
/// `Some(v)` matches the exact value. Each call appends its own
/// correlated subquery alias via `counter`, so the clause can be reused
/// multiple times in one statement.
fn meta_predicate(
    alias: &str,
    key: &str,
    value: Option<&str>,
    params: &mut Vec<rusqlite::types::Value>,
    counter: &mut usize,
) -> String {
    *counter += 1;
    let n = *counter;
    params.push(rusqlite::types::Value::Text(key.to_string()));
    let key_param = params.len();
    let value_clause = match value {
        Some(v) => {
            params.push(rusqlite::types::Value::Text(v.to_string()));
            format!(" AND em{n}.value = ?{}", params.len())
        }
        None => String::new(),
    };
    format!(
        "EXISTS (SELECT 1 FROM entry_meta em{n}
         JOIN meta_keys mk{n} ON mk{n}.internal_id = em{n}.key_id
         WHERE em{n}.entry_id = {alias}.internal_id
           AND mk{n}.key = ?{key_param}{value_clause})"
    )
}

/// Render an [`ArchiverFilter`] as a SQL boolean expression over table
/// alias `alias`, expressed through the `archiver` meta key.
///
/// `params` receives one parameter per placeholder in the returned clause.
fn archiver_filter_predicate(
    filter: ArchiverFilter<'_>,
    alias: &str,
    params: &mut Vec<rusqlite::types::Value>,
    counter: &mut usize,
) -> String {
    match filter {
        ArchiverFilter::Any => "1".to_string(),
        ArchiverFilter::WithoutArchiver => {
            format!("NOT {}", meta_predicate(alias, "archiver", None, params, counter))
        }
        ArchiverFilter::Named(name) => meta_predicate(alias, "archiver", Some(name), params, counter),
    }
}

fn parse_uuid_id<T: std::str::FromStr>(s: &str) -> rusqlite::Result<T>
where
    T::Err: std::error::Error + Send + Sync + 'static,
{
    s.parse::<T>().map_err(|e| {
        rusqlite::Error::FromSqlConversionFailure(0, rusqlite::types::Type::Text, Box::new(e))
    })
}

fn collect_tag_ids(query: &ResolvedQuery, out: &mut HashSet<TagId>) {
    match query {
        Query::Tag(tag_id) => {
            out.insert(*tag_id);
        }
        Query::Meta { .. } => {}
        Query::And(queries) | Query::Or(queries) => {
            for q in queries {
                collect_tag_ids(q, out);
            }
        }
        Query::Not(inner) => collect_tag_ids(inner, out),
    }
}

/// Translate query set semantics into a SQL boolean expression over `e`.
///
/// `params` receives one parameter per `?N` placeholder in the order the
/// placeholders appear (tag ids for positive tag references, key/value
/// text for meta predicates). Tag ids missing from `internal_ids` match
/// nothing, so `NOT` over them matches everything.
fn query_predicate(
    query: &ResolvedQuery,
    internal_ids: &HashMap<TagId, i64>,
    params: &mut Vec<rusqlite::types::Value>,
    counter: &mut usize,
) -> String {
    match query {
        Query::Tag(tag_id) => match internal_ids.get(tag_id) {
            Some(internal_id) => {
                *counter += 1;
                let n = *counter;
                params.push(rusqlite::types::Value::Integer(*internal_id));
                format!(
                    "EXISTS (SELECT 1 FROM entry_tags m{n}
                     WHERE m{n}.entry_id = e.internal_id
                       AND m{n}.tag_id = ?{})",
                    params.len()
                )
            }
            None => "0".to_string(),
        },
        // `meta:key` / `meta:key=value` → EXISTS over entry_meta. A key
        // with no rows matches nothing, so `!meta:key` matches everything.
        Query::Meta { key, value } => {
            meta_predicate("e", key, value.as_deref(), params, counter)
        }
        Query::And(queries) if queries.is_empty() => "1".to_string(),
        Query::Or(queries) if queries.is_empty() => "0".to_string(),
        Query::And(queries) => format!(
            "({})",
            queries
                .iter()
                .map(|q| query_predicate(q, internal_ids, params, counter))
                .collect::<Vec<_>>()
                .join(" AND ")
        ),
        Query::Or(queries) => format!(
            "({})",
            queries
                .iter()
                .map(|q| query_predicate(q, internal_ids, params, counter))
                .collect::<Vec<_>>()
                .join(" OR ")
        ),
        Query::Not(inner) => format!(
            "(NOT {})",
            query_predicate(inner, internal_ids, params, counter)
        ),
    }
}

/// Store backed by SQLite (primary source of truth).
pub struct Store {
    data_dir: PathBuf,
    db_path: PathBuf,
}

impl Store {
    pub fn new(data_dir: impl Into<PathBuf>) -> Result<Self> {
        let data_dir = data_dir.into();
        std::fs::create_dir_all(&data_dir)
            .with_context(|| format!("Failed to create data directory {}", data_dir.display()))?;
        let db_path = data_dir.join("store.db");
        let _conn = db::open_store_db(&db_path)?;
        Ok(Self { db_path, data_dir })
    }

    pub fn data_dir(&self) -> &Path {
        &self.data_dir
    }

    fn conn(&self) -> Result<Connection> {
        db::open_store_db(&self.db_path)
    }

    /// True when the store holds no tags and no entries.
    pub fn is_empty(&self) -> Result<bool> {
        let conn = self.conn()?;
        let tag_count: i64 = conn.query_row("SELECT COUNT(*) FROM tags", [], |r| r.get(0))?;
        let entry_count: i64 = conn.query_row("SELECT COUNT(*) FROM entries", [], |r| r.get(0))?;
        Ok(tag_count == 0 && entry_count == 0)
    }

    /// Delete all tags, entries, meta attributes, and their links.
    pub fn clear_all(&self) -> Result<()> {
        let conn = self.conn()?;
        let tx = conn.unchecked_transaction()?;
        tx.execute("DELETE FROM entry_meta", [])?;
        tx.execute("DELETE FROM meta_keys", [])?;
        tx.execute("DELETE FROM entry_tags", [])?;
        tx.execute("DELETE FROM entries", [])?;
        tx.execute("DELETE FROM tags", [])?;
        tx.commit()?;
        Ok(())
    }

    /// Bulk insert tags and entries in a single transaction.
    ///
    /// Entry `ordinal` is assigned by slice order (`0..n`), preserving the
    /// order of the source JSONL file. Entry `tag_ids` referencing unknown
    /// tags (possible in legacy JSONL files) are skipped rather than failing
    /// the import; duplicate links are ignored via `INSERT OR IGNORE`.
    pub fn import_bulk(&self, tags: &[Tag], entries: &[Entry]) -> Result<()> {
        let conn = self.conn()?;
        let tx = conn.unchecked_transaction()?;
        {
            let mut stmt = tx.prepare(
                "INSERT INTO tags (id, name, specifier, description, aliases) VALUES (?1, ?2, ?3, ?4, ?5)",
            )?;
            for tag in tags {
                let aliases =
                    serde_json::to_string(&tag.aliases).context("Failed to serialize aliases")?;
                stmt.execute(params![
                    tag.id.to_string(),
                    tag.name,
                    tag.specifier,
                    tag.description,
                    aliases
                ])?;
            }
        }
        {
            let mut entry_stmt = tx.prepare(
                "INSERT INTO entries (id, url, description, ordinal) VALUES (?1, ?2, ?3, ?4)",
            )?;
            let mut link_stmt = tx.prepare(
                "INSERT OR IGNORE INTO entry_tags (entry_id, tag_id)
                 SELECT e.internal_id, t.internal_id FROM entries e, tags t
                 WHERE e.id = ?1 AND t.id = ?2",
            )?;
            let mut meta_key_stmt =
                tx.prepare("INSERT OR IGNORE INTO meta_keys (key) VALUES (?1)")?;
            let mut meta_value_stmt = tx.prepare(
                "INSERT OR REPLACE INTO entry_meta (entry_id, key_id, value)
                 VALUES ((SELECT internal_id FROM entries WHERE id = ?1),
                         (SELECT internal_id FROM meta_keys WHERE key = ?2), ?3)",
            )?;
            for (ordinal, entry) in entries.iter().enumerate() {
                entry_stmt.execute(params![
                    entry.id.to_string(),
                    entry.url,
                    entry.description,
                    ordinal as i64
                ])?;
                for tag_id in &entry.tag_ids {
                    let tag_id_str = tag_id.to_string();
                    link_stmt.execute(params![entry.id.to_string(), tag_id_str])?;
                }
                for (key, value) in &entry.meta {
                    meta_key_stmt.execute(params![key])?;
                    meta_value_stmt.execute(params![entry.id.to_string(), key, value])?;
                }
            }
        }
        tx.commit()?;
        Ok(())
    }
}

mod entries;
mod meta;
mod query;
mod tags;

#[cfg(test)]
mod tests {
    use super::*;
    use crate::domain::{Entry, Tag, TagId};

    fn temp_store(tag: &str) -> (Store, PathBuf) {
        let dir = std::env::temp_dir().join(format!(
            "pantagraph_test_store_{tag}_{}",
            uuid::Uuid::now_v7()
        ));
        let store = Store::new(&dir).unwrap();
        (store, dir)
    }

    #[test]
    fn test_is_empty_clear_all_import_bulk() {
        let (store, dir) = temp_store("bulk");
        assert!(store.is_empty().unwrap());

        let tag = Tag::with_aliases("t", "d", vec!["a".to_string()]);
        let tag_id = tag.id;
        let entry = Entry::new(
            "https://x.test",
            "e",
            vec![tag_id, TagId::new()],
        );
        store
            .import_bulk(&[tag], std::slice::from_ref(&entry))
            .unwrap();

        assert!(!store.is_empty().unwrap());
        let tags = store.list_tags().unwrap();
        assert_eq!(tags.len(), 1);
        assert_eq!(tags[0].aliases, vec!["a".to_string()]);
        let entries = store.list_entries().unwrap();
        assert_eq!(entries.len(), 1);
        // known tag id linked; unknown tag id skipped
        assert_eq!(entries[0].tag_ids.len(), 1);
        assert_eq!(entries[0].tag_ids[0], tag_id);

        store.clear_all().unwrap();
        assert!(store.is_empty().unwrap());
        assert!(store.list_tags().unwrap().is_empty());
        assert!(store.list_entries().unwrap().is_empty());

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn test_import_bulk_preserves_ordinal_order() {
        let (store, dir) = temp_store("ordinal");
        let entries: Vec<Entry> = (0..10)
            .map(|i| {
                Entry::new(
                    format!("https://e/{i}"),
                    format!("d{i}"),
                    vec![],
                )
            })
            .collect();
        store.import_bulk(&[], &entries).unwrap();
        let listed = store.list_entries().unwrap();
        let urls: Vec<&str> = listed.iter().map(|e| e.url.as_str()).collect();
        let expected: Vec<String> = (0..10).map(|i| format!("https://e/{i}")).collect();
        let expected: Vec<&str> = expected.iter().map(|s| s.as_str()).collect();
        assert_eq!(urls, expected);
        let _ = std::fs::remove_dir_all(&dir);
    }
}
