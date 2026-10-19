use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};

use crate::db;
use crate::domain::{Entry, EntryId, Tag, TagId};
use crate::query::{Query, ResolvedQuery};
use anyhow::{Context, Result};
use rusqlite::{params, Connection, OptionalExtension};

#[derive(Debug, Clone, Copy)]
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

fn apply_archiver_filter(entries: &mut Vec<Entry>, filter: ArchiverFilter<'_>) {
    match filter {
        ArchiverFilter::Any => {}
        ArchiverFilter::WithoutArchiver => {
            entries.retain(|e| e.archiver.is_none());
        }
        ArchiverFilter::Named(name) => {
            entries.retain(|e| e.archiver.as_deref() == Some(name));
        }
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
/// `params` receives one internal tag id per positive tag reference, in
/// the order their `?N` placeholders appear. Tag ids missing from
/// `internal_ids` match nothing, so `NOT` over them matches everything.
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
pub struct JsonlStore {
    data_dir: PathBuf,
    db_path: PathBuf,
}

impl JsonlStore {
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

    pub fn create_tag(&self, tag: Tag) -> Result<Tag> {
        let conn = self.conn()?;
        let aliases = serde_json::to_string(&tag.aliases).context("Failed to serialize aliases")?;
        conn.execute(
            "INSERT INTO tags (id, name, specifier, description, aliases) VALUES (?1, ?2, ?3, ?4, ?5)",
            params![
                tag.id.to_string(),
                tag.name,
                tag.specifier,
                tag.description,
                aliases
            ],
        )
        .context("Failed to insert tag")?;
        Ok(tag)
    }

    pub fn list_tags(&self) -> Result<Vec<Tag>> {
        let conn = self.conn()?;
        let mut stmt =
            conn.prepare("SELECT id, name, specifier, description, aliases FROM tags")?;
        let rows = stmt.query_map([], |row| {
            let id_str: String = row.get(0)?;
            let name: String = row.get(1)?;
            let specifier: Option<String> = row.get(2)?;
            let description: String = row.get(3)?;
            let aliases_json: String = row.get(4)?;
            let aliases: Vec<String> = serde_json::from_str(&aliases_json).unwrap_or_default();
            let id = parse_uuid_id::<TagId>(&id_str)?;
            Ok(Tag {
                id,
                name,
                specifier,
                description,
                aliases,
            })
        })?;
        let mut tags = Vec::new();
        for r in rows {
            tags.push(r?);
        }
        Ok(tags)
    }
    pub fn rename_tag(&self, id: &TagId, new_name: &str) -> Result<Tag> {
        let conn = self.conn()?;
        let tx = conn.unchecked_transaction()?;
        let mut tag = tx
            .query_row(
                "SELECT id, name, specifier, description, aliases FROM tags WHERE id = ?1",
                params![id.to_string()],
                |row| {
                    let id_str: String = row.get(0)?;
                    let name: String = row.get(1)?;
                    let specifier: Option<String> = row.get(2)?;
                    let description: String = row.get(3)?;
                    let aliases_json: String = row.get(4)?;
                    let aliases: Vec<String> =
                        serde_json::from_str(&aliases_json).unwrap_or_default();
                    let tid = parse_uuid_id::<TagId>(&id_str)?;
                    Ok(Tag {
                        id: tid,
                        name,
                        specifier,
                        description,
                        aliases,
                    })
                },
            )
            .optional()?
            .context("Tag not found")?;
        tag.name = new_name.to_string();
        tag.aliases.retain(|a| a != new_name);
        let aliases = serde_json::to_string(&tag.aliases)?;
        tx.execute(
            "UPDATE tags SET name = ?1, aliases = ?2 WHERE id = ?3",
            params![tag.name, aliases, tag.id.to_string()],
        )?;
        tx.commit()?;
        Ok(tag)
    }
    #[allow(dead_code)]
    pub fn find_tags_by_name(&self, name: &str) -> Result<Vec<Tag>> {
        let conn = self.conn()?;
        let mut stmt = conn.prepare(
            "SELECT id, name, specifier, description, aliases FROM tags WHERE name = ?1",
        )?;
        let rows = stmt.query_map(params![name], |row| {
            let id_str: String = row.get(0)?;
            let name: String = row.get(1)?;
            let specifier: Option<String> = row.get(2)?;
            let description: String = row.get(3)?;
            let aliases_json: String = row.get(4)?;
            let aliases: Vec<String> = serde_json::from_str(&aliases_json).unwrap_or_default();
            let id = parse_uuid_id::<TagId>(&id_str)?;
            Ok(Tag {
                id,
                name,
                specifier,
                description,
                aliases,
            })
        })?;
        let mut tags = Vec::new();
        for r in rows {
            tags.push(r?);
        }
        Ok(tags)
    }

    pub fn find_tags_by_name_or_alias(&self, query: &str) -> Result<Vec<Tag>> {
        let conn = self.conn()?;
        let mut stmt = conn.prepare(
            "SELECT id, name, specifier, description, aliases FROM tags WHERE name = ?1 OR aliases LIKE ?2",
        )?;
        let rows = stmt.query_map(params![query, format!("%\"{}\"%", query)], |row| {
            let id_str: String = row.get(0)?;
            let name: String = row.get(1)?;
            let specifier: Option<String> = row.get(2)?;
            let description: String = row.get(3)?;
            let aliases_json: String = row.get(4)?;
            let aliases: Vec<String> = serde_json::from_str(&aliases_json).unwrap_or_default();
            let id = parse_uuid_id::<TagId>(&id_str)?;
            Ok(Tag {
                id,
                name,
                specifier,
                description,
                aliases,
            })
        })?;
        let mut tags = Vec::new();
        for r in rows {
            tags.push(r?);
        }
        Ok(tags)
    }
    pub fn find_tags_by_name_and_specifier(
        &self,
        name_or_alias: &str,
        specifier: &str,
    ) -> Result<Vec<Tag>> {
        let conn = self.conn()?;
        let mut stmt = conn.prepare(
            "SELECT id, name, specifier, description, aliases FROM tags WHERE (name = ?1 OR aliases LIKE ?2) AND specifier = ?3",
        )?;
        let rows = stmt.query_map(
            params![name_or_alias, format!("%\"{}\"%", name_or_alias), specifier],
            |row| {
                let id_str: String = row.get(0)?;
                let name: String = row.get(1)?;
                let specifier: Option<String> = row.get(2)?;
                let description: String = row.get(3)?;
                let aliases_json: String = row.get(4)?;
                let aliases: Vec<String> = serde_json::from_str(&aliases_json).unwrap_or_default();
                let id = parse_uuid_id::<TagId>(&id_str)?;
                Ok(Tag {
                    id,
                    name,
                    specifier,
                    description,
                    aliases,
                })
            },
        )?;
        let mut tags = Vec::new();
        for r in rows {
            tags.push(r?);
        }
        Ok(tags)
    }

    pub fn find_tag_by_id(&self, id: &TagId) -> Result<Option<Tag>> {
        let conn = self.conn()?;
        let tag = conn
            .query_row(
                "SELECT id, name, specifier, description, aliases FROM tags WHERE id = ?1",
                params![id.to_string()],
                |row| {
                    let id_str: String = row.get(0)?;
                    let name: String = row.get(1)?;
                    let specifier: Option<String> = row.get(2)?;
                    let description: String = row.get(3)?;
                    let aliases_json: String = row.get(4)?;
                    let aliases: Vec<String> =
                        serde_json::from_str(&aliases_json).unwrap_or_default();
                    let tid = parse_uuid_id::<TagId>(&id_str)?;
                    Ok(Tag {
                        id: tid,
                        name,
                        specifier,
                        description,
                        aliases,
                    })
                },
            )
            .optional()?;
        Ok(tag)
    }
    pub fn add_tag_alias(&self, id: &TagId, alias: &str) -> Result<Tag> {
        let conn = self.conn()?;
        let tx = conn.unchecked_transaction()?;
        let mut tag = tx
            .query_row(
                "SELECT id, name, specifier, description, aliases FROM tags WHERE id = ?1",
                params![id.to_string()],
                |row| {
                    let id_str: String = row.get(0)?;
                    let name: String = row.get(1)?;
                    let specifier: Option<String> = row.get(2)?;
                    let description: String = row.get(3)?;
                    let aliases_json: String = row.get(4)?;
                    let aliases: Vec<String> =
                        serde_json::from_str(&aliases_json).unwrap_or_default();
                    let tid = parse_uuid_id::<TagId>(&id_str)?;
                    Ok(Tag {
                        id: tid,
                        name,
                        specifier,
                        description,
                        aliases,
                    })
                },
            )
            .optional()?
            .context("Tag not found")?;
        if !tag.aliases.iter().any(|a| a == alias) {
            tag.aliases.push(alias.to_string());
        }
        let aliases = serde_json::to_string(&tag.aliases)?;
        tx.execute(
            "UPDATE tags SET aliases = ?1 WHERE id = ?2",
            params![aliases, tag.id.to_string()],
        )?;
        tx.commit()?;
        Ok(tag)
    }

    pub fn remove_tag_alias(&self, id: &TagId, alias: &str) -> Result<Tag> {
        let conn = self.conn()?;
        let tx = conn.unchecked_transaction()?;
        let mut tag = tx
            .query_row(
                "SELECT id, name, specifier, description, aliases FROM tags WHERE id = ?1",
                params![id.to_string()],
                |row| {
                    let id_str: String = row.get(0)?;
                    let name: String = row.get(1)?;
                    let specifier: Option<String> = row.get(2)?;
                    let description: String = row.get(3)?;
                    let aliases_json: String = row.get(4)?;
                    let aliases: Vec<String> =
                        serde_json::from_str(&aliases_json).unwrap_or_default();
                    let tid = parse_uuid_id::<TagId>(&id_str)?;
                    Ok(Tag {
                        id: tid,
                        name,
                        specifier,
                        description,
                        aliases,
                    })
                },
            )
            .optional()?
            .context("Tag not found")?;
        tag.aliases.retain(|a| a != alias);
        let aliases = serde_json::to_string(&tag.aliases)?;
        tx.execute(
            "UPDATE tags SET aliases = ?1 WHERE id = ?2",
            params![aliases, tag.id.to_string()],
        )?;
        tx.commit()?;
        Ok(tag)
    }
    pub fn create_entry(&self, entry: Entry) -> Result<Entry> {
        let conn = self.conn()?;
        let tx = conn.unchecked_transaction()?;
        let ordinal: i64 = tx.query_row(
            "SELECT COALESCE(MAX(ordinal), -1) + 1 FROM entries",
            [],
            |row| row.get(0),
        )?;
        tx.execute(
            "INSERT INTO entries (id, url, archiver, archive_path, description, ordinal) VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
            params![
                entry.id.to_string(),
                entry.url,
                entry.archiver,
                entry.archive_path,
                entry.description,
                ordinal
            ],
        )?;
        for tid in &entry.tag_ids {
            tx.execute(
                "INSERT OR IGNORE INTO entry_tags (entry_id, tag_id)
                 SELECT e.internal_id, t.internal_id FROM entries e, tags t
                 WHERE e.id = ?1 AND t.id = ?2",
                params![entry.id.to_string(), tid.to_string()],
            )?;
        }
        tx.commit()?;
        Ok(entry)
    }
    fn load_entry_with_tags(
        &self,
        conn: &Connection,
        entry_row: &rusqlite::Row<'_>,
    ) -> rusqlite::Result<Entry> {
        let id_str: String = entry_row.get(0)?;
        let url: String = entry_row.get(1)?;
        let archiver: Option<String> = entry_row.get(2)?;
        let archive_path: Option<String> = entry_row.get(3)?;
        let description: String = entry_row.get(4)?;
        let _ordinal: i64 = entry_row.get(5)?;
        let id = parse_uuid_id::<EntryId>(&id_str)?;
        let mut stmt = conn.prepare(
            "SELECT t.id FROM entry_tags et JOIN tags t ON t.internal_id = et.tag_id
             WHERE et.entry_id = ?1",
        )?;
        let mut tag_ids = Vec::new();
        let internal_id: i64 = entry_row.get(6)?;
        let rows = stmt.query_map(params![internal_id], |r| {
            let t: String = r.get(0)?;
            Ok(t)
        })?;
        for r in rows {
            let ts = r?;
            if let Ok(tid) = ts.parse::<TagId>() {
                tag_ids.push(tid);
            }
        }
        Ok(Entry {
            id,
            url,
            archiver,
            archive_path,
            description,
            tag_ids,
        })
    }
    pub fn list_entries(&self) -> Result<Vec<Entry>> {
        self.find_entries(&[], None)
    }

    pub fn find_entry_by_id(&self, id: &EntryId) -> Result<Option<Entry>> {
        let conn = self.conn()?;
        let mut stmt = conn.prepare(
            "SELECT id, url, archiver, archive_path, description, ordinal, internal_id FROM entries WHERE id = ?1",
        )?;
        let entry = stmt
            .query_row(params![id.to_string()], |row| {
                self.load_entry_with_tags(&conn, row)
            })
            .optional()?;
        Ok(entry)
    }

    /// Fetch just the insertion ordinal of an entry, without loading its tags.
    ///
    /// Ordinals are contiguous integers assigned at insert time (each new
    /// entry gets `MAX(ordinal) + 1`) and entries are never deleted
    /// individually, so the Nth row in `ORDER BY ordinal` has `ordinal == N`.
    /// This makes an entry's ordinal usable directly as a SQL `OFFSET` for
    /// "entries after this one" queries.
    pub fn find_entry_ordinal(&self, id: &EntryId) -> Result<Option<i64>> {
        let conn = self.conn()?;
        let ordinal = conn
            .query_row(
                "SELECT ordinal FROM entries WHERE id = ?1",
                params![id.to_string()],
                |row| row.get::<_, i64>(0),
            )
            .optional()?;
        Ok(ordinal)
    }

    pub fn find_entries_by_tag(&self, tag_id: &TagId) -> Result<Vec<Entry>> {
        self.find_entries(&[*tag_id], None)
    }
    pub fn find_entries(
        &self,
        tag_ids: &[TagId],
        archiver: Option<Option<&str>>,
    ) -> Result<Vec<Entry>> {
        self.find_entries_page(tag_ids, archiver, None)
    }

    /// Like [`find_entries`], but optionally pushes `limit`/`offset` into
    /// SQL when it is safe to do so.
    ///
    /// The limit is pushed into SQL only when there is no in-memory
    /// archiver filter (i.e. `archiver == None`), since that filter runs
    /// after the SQL query and a SQL-level `LIMIT` could otherwise cut off
    /// rows that the filter would have kept.
    pub fn find_entries_page(
        &self,
        tag_ids: &[TagId],
        archiver: Option<Option<&str>>,
        page: Option<(usize, usize)>,
    ) -> Result<Vec<Entry>> {
        let conn = self.conn()?;
        let archiver_filter = to_archiver_filter(archiver);
        // Only push the page into SQL when no in-memory archiver filter
        // will run after the query, otherwise a SQL LIMIT could drop rows
        // the archiver filter would have kept.
        let page = match archiver_filter {
            ArchiverFilter::Any => page,
            _ => None,
        };
        let tag_ids_str: Vec<String> = tag_ids.iter().map(|t| t.to_string()).collect();
        let mut entries: Vec<Entry> = if tag_ids.is_empty() {
            let sql = match page {
                Some(_) => "SELECT id, url, archiver, archive_path, description, ordinal, internal_id FROM entries ORDER BY ordinal LIMIT ?1 OFFSET ?2",
                None => "SELECT id, url, archiver, archive_path, description, ordinal, internal_id FROM entries ORDER BY ordinal",
            };
            let mut stmt = conn.prepare(sql)?;
            let mut params_vec: Vec<rusqlite::types::Value> = Vec::new();
            if let Some((limit, offset)) = page {
                params_vec.push(rusqlite::types::Value::Integer(limit as i64));
                params_vec.push(rusqlite::types::Value::Integer(offset as i64));
            }
            let rows = stmt.query_map(rusqlite::params_from_iter(params_vec), |row| {
                self.load_entry_with_tags(&conn, row)
            })?;
            let mut v = Vec::new();
            for r in rows {
                v.push(r?);
            }
            v
        } else {
            let mut internal_tag_ids = Vec::with_capacity(tag_ids_str.len());
            for tag_id in tag_ids {
                let internal_id = conn
                    .query_row(
                        "SELECT internal_id FROM tags WHERE id = ?1",
                        params![tag_id.to_string()],
                        |row| row.get::<_, i64>(0),
                    )
                    .optional()?;
                if let Some(internal_id) = internal_id {
                    internal_tag_ids.push(internal_id);
                } else {
                    internal_tag_ids.clear();
                    break;
                }
            }
            let exists_clause = (1..internal_tag_ids.len())
                .map(|i| format!(
                    " AND EXISTS (SELECT 1 FROM entry_tags matching_{i}
                     WHERE matching_{i}.entry_id = e.internal_id
                       AND matching_{i}.tag_id = ?{})",
                    i + 1
                ))
                .collect::<String>();
            let paging_clause = match page {
                Some(_) => format!(
                    " LIMIT ?{} OFFSET ?{}",
                    internal_tag_ids.len() + 1,
                    internal_tag_ids.len() + 2
                ),
                None => String::new(),
            };
            let sql = format!(
                "SELECT e.id, e.url, e.archiver, e.archive_path, e.description, e.ordinal, e.internal_id
                 FROM entries e
                 JOIN entry_tags candidate ON candidate.entry_id = e.internal_id
                 WHERE candidate.tag_id = ?1{exists_clause}
                 ORDER BY e.ordinal{paging_clause}"
            );
            let mut stmt = conn.prepare(&sql)?;
            let mut params_vec: Vec<rusqlite::types::Value> = Vec::new();
            for internal_id in internal_tag_ids {
                params_vec.push(rusqlite::types::Value::Integer(internal_id));
            }
            if let Some((limit, offset)) = page {
                params_vec.push(rusqlite::types::Value::Integer(limit as i64));
                params_vec.push(rusqlite::types::Value::Integer(offset as i64));
            }
            let rows = stmt.query_map(rusqlite::params_from_iter(params_vec), |row| {
                self.load_entry_with_tags(&conn, row)
            })?;
            let mut v = Vec::new();
            for r in rows {
                v.push(r?);
            }
            v
        };

        apply_archiver_filter(&mut entries, archiver_filter);
        Ok(entries)
    }

    /// Find entries whose tags satisfy a boolean query.
    ///
    /// `And` is intersection, `Or` is union, `Not` is the complement over
    /// all entries. Tag ids absent from the store match nothing.
    pub fn find_entries_by_query(
        &self,
        query: &ResolvedQuery,
        archiver: Option<Option<&str>>,
    ) -> Result<Vec<Entry>> {
        self.find_entries_by_query_page_opt(query, archiver, None)
    }

    /// Like [`find_entries_by_query`], but optionally pushes `limit`/`offset`
    /// into SQL when it is safe to do so.
    ///
    /// The limit is pushed into SQL only when there is no in-memory
    /// archiver filter (i.e. `archiver == None`), since that filter runs
    /// after the SQL query and a SQL-level `LIMIT` could otherwise cut off
    /// rows that the filter would have kept.
    pub fn find_entries_by_query_page_opt(
        &self,
        query: &ResolvedQuery,
        archiver: Option<Option<&str>>,
        page: Option<(usize, usize)>,
    ) -> Result<Vec<Entry>> {
        let archiver_filter = to_archiver_filter(archiver);
        let page = match archiver_filter {
            ArchiverFilter::Any => page,
            _ => None,
        };
        let mut entries = self.query_entries(query, page)?;
        apply_archiver_filter(&mut entries, archiver_filter);
        Ok(entries)
    }

    /// Find at most `limit` entries satisfying a boolean query, starting
    /// at `offset`, ordered by insertion order.
    ///
    /// The limit is pushed into SQL so callers can page through the full
    /// result set without loading it into memory. The archiver filter is
    /// not applied here because it runs in memory after the SQL query.
    pub fn find_entries_by_query_page(
        &self,
        query: &ResolvedQuery,
        limit: usize,
        offset: usize,
    ) -> Result<Vec<Entry>> {
        self.query_entries(query, Some((limit, offset)))
    }

    fn query_entries(
        &self,
        query: &ResolvedQuery,
        page: Option<(usize, usize)>,
    ) -> Result<Vec<Entry>> {
        let conn = self.conn()?;

        let mut tag_ids = HashSet::new();
        collect_tag_ids(query, &mut tag_ids);
        let mut internal_ids = HashMap::with_capacity(tag_ids.len());
        for tag_id in tag_ids {
            let internal_id = conn
                .query_row(
                    "SELECT internal_id FROM tags WHERE id = ?1",
                    params![tag_id.to_string()],
                    |row| row.get::<_, i64>(0),
                )
                .optional()?;
            if let Some(internal_id) = internal_id {
                internal_ids.insert(tag_id, internal_id);
            }
        }

        let mut params_vec = Vec::new();
        let mut counter = 0;
        let predicate = query_predicate(query, &internal_ids, &mut params_vec, &mut counter);
        let sql = match page {
            Some(_) => format!(
                "SELECT e.id, e.url, e.archiver, e.archive_path, e.description, e.ordinal, e.internal_id
                 FROM entries e
                 WHERE {predicate}
                 ORDER BY e.ordinal
                 LIMIT ?{} OFFSET ?{}",
                params_vec.len() + 1,
                params_vec.len() + 2
            ),
            None => format!(
                "SELECT e.id, e.url, e.archiver, e.archive_path, e.description, e.ordinal, e.internal_id
                 FROM entries e
                 WHERE {predicate}
                 ORDER BY e.ordinal"
            ),
        };
        let mut stmt = conn.prepare(&sql)?;
        if let Some((limit, offset)) = page {
            params_vec.push(rusqlite::types::Value::Integer(limit as i64));
            params_vec.push(rusqlite::types::Value::Integer(offset as i64));
        }
        let rows = stmt.query_map(rusqlite::params_from_iter(params_vec), |row| {
            self.load_entry_with_tags(&conn, row)
        })?;
        let mut entries = Vec::new();
        for r in rows {
            entries.push(r?);
        }
        Ok(entries)
    }

    pub fn add_tag_to_entry(&self, entry_id: &EntryId, tag_id: &TagId) -> Result<()> {
        let conn = self.conn()?;
        conn.execute(
            "INSERT OR IGNORE INTO entry_tags (entry_id, tag_id)
             SELECT e.internal_id, t.internal_id FROM entries e, tags t
             WHERE e.id = ?1 AND t.id = ?2",
            params![entry_id.to_string(), tag_id.to_string()],
        )?;
        Ok(())
    }

    /// True when the store holds no tags and no entries.
    pub fn is_empty(&self) -> Result<bool> {
        let conn = self.conn()?;
        let tag_count: i64 = conn.query_row("SELECT COUNT(*) FROM tags", [], |r| r.get(0))?;
        let entry_count: i64 = conn.query_row("SELECT COUNT(*) FROM entries", [], |r| r.get(0))?;
        Ok(tag_count == 0 && entry_count == 0)
    }

    /// Delete all tags, entries, and their links.
    pub fn clear_all(&self) -> Result<()> {
        let conn = self.conn()?;
        let tx = conn.unchecked_transaction()?;
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
                "INSERT INTO entries (id, url, archiver, archive_path, description, ordinal) VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
            )?;
            let mut link_stmt = tx.prepare(
                "INSERT OR IGNORE INTO entry_tags (entry_id, tag_id)
                 SELECT e.internal_id, t.internal_id FROM entries e, tags t
                 WHERE e.id = ?1 AND t.id = ?2",
            )?;
            for (ordinal, entry) in entries.iter().enumerate() {
                entry_stmt.execute(params![
                    entry.id.to_string(),
                    entry.url,
                    entry.archiver,
                    entry.archive_path,
                    entry.description,
                    ordinal as i64
                ])?;
                for tag_id in &entry.tag_ids {
                    let tag_id_str = tag_id.to_string();
                    link_stmt.execute(params![entry.id.to_string(), tag_id_str])?;
                }
            }
        }
        tx.commit()?;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn temp_store(tag: &str) -> (JsonlStore, PathBuf) {
        let dir = std::env::temp_dir().join(format!(
            "pantagraph_test_store_{tag}_{}",
            uuid::Uuid::now_v7()
        ));
        let store = JsonlStore::new(&dir).unwrap();
        (store, dir)
    }

    #[test]
    fn test_tag_crud() {
        let (store, dir) = temp_store("tag_crud");

        let created = store.create_tag(Tag::new("rust", "lang")).unwrap();
        let found = store.find_tag_by_id(&created.id).unwrap().unwrap();
        assert_eq!(found.name, "rust");
        assert_eq!(found.description, "lang");
        assert!(found.aliases.is_empty());
        assert!(found.specifier.is_none());

        assert_eq!(store.list_tags().unwrap().len(), 1);

        let renamed = store.rename_tag(&created.id, "rust-lang").unwrap();
        assert_eq!(renamed.name, "rust-lang");
        assert_eq!(
            store.find_tag_by_id(&created.id).unwrap().unwrap().name,
            "rust-lang"
        );

        let with_alias = store.add_tag_alias(&created.id, "rs").unwrap();
        assert_eq!(with_alias.aliases, vec!["rs".to_string()]);
        assert_eq!(store.find_tags_by_name_or_alias("rs").unwrap().len(), 1);

        let removed = store.remove_tag_alias(&created.id, "rs").unwrap();
        assert!(removed.aliases.is_empty());
        assert!(store.find_tags_by_name_or_alias("rs").unwrap().is_empty());

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn test_find_tags_by_specifier() {
        let (store, dir) = temp_store("tag_spec");
        let tag = Tag::with_specifier("game", Some("jp".to_string()), "games", vec![]);
        let id = store.create_tag(tag).unwrap().id;

        assert_eq!(
            store
                .find_tags_by_name_and_specifier("game", "jp")
                .unwrap()
                .len(),
            1
        );
        assert!(store
            .find_tags_by_name_and_specifier("game", "en")
            .unwrap()
            .is_empty());
        let _ = std::fs::remove_dir_all(&dir);
        let _ = id;
    }

    #[test]
    fn test_entry_crud_and_ordering() {
        let (store, dir) = temp_store("entry_crud");
        let tag = store.create_tag(Tag::new("t", "d")).unwrap();
        let tag_id = tag.id;

        let e1 = Entry::new(
            "https://a.test",
            Some("curl".to_string()),
            Some("archives/a.html".to_string()),
            "first",
            vec![tag_id],
        );
        let e2 = Entry::new("https://b.test", None, None, "second", vec![]);
        let e3 = Entry::new("https://c.test", None, None, "third", vec![tag_id]);

        let id1 = store.create_entry(e1).unwrap().id;
        let id2 = store.create_entry(e2).unwrap().id;
        let id3 = store.create_entry(e3).unwrap().id;

        // list_entries preserves insertion (ordinal) order
        let all = store.list_entries().unwrap();
        assert_eq!(
            all.iter().map(|e| e.id).collect::<Vec<_>>(),
            vec![id1, id2, id3]
        );

        // find_entry_by_id loads tags
        let found = store.find_entry_by_id(&id1).unwrap().unwrap();
        assert_eq!(found.url, "https://a.test");
        assert_eq!(found.archiver.as_deref(), Some("curl"));
        assert_eq!(found.tag_ids, vec![tag_id]);
        assert!(store.find_entry_by_id(&EntryId::new()).unwrap().is_none());

        // find_entries_by_tag
        let by_tag = store.find_entries_by_tag(&tag_id).unwrap();
        assert_eq!(
            by_tag.iter().map(|e| e.id).collect::<Vec<_>>(),
            vec![id1, id3]
        );

        // archiver filters
        let no_archiver = store.find_entries(&[], Some(None)).unwrap();
        assert_eq!(
            no_archiver.iter().map(|e| e.id).collect::<Vec<_>>(),
            vec![id2, id3]
        );
        let curl = store.find_entries(&[], Some(Some("curl"))).unwrap();
        assert_eq!(curl.iter().map(|e| e.id).collect::<Vec<_>>(), vec![id1]);

        // add_tag_to_entry
        store.add_tag_to_entry(&id2, &tag_id).unwrap();
        // idempotent
        store.add_tag_to_entry(&id2, &tag_id).unwrap();
        assert_eq!(store.find_entries_by_tag(&tag_id).unwrap().len(), 3);

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn test_find_entries_requires_all_resolved_tags() {
        let (store, dir) = temp_store("tag_intersection");
        let first = store
            .create_tag(Tag::with_specifier(
                "shared",
                Some("one".into()),
                "",
                vec![],
            ))
            .unwrap();
        let second = store
            .create_tag(Tag::with_specifier(
                "shared",
                Some("two".into()),
                "",
                vec![],
            ))
            .unwrap();
        let other = store.create_tag(Tag::new("other", "")).unwrap();

        let earlier = store
            .create_entry(Entry::new(
                "https://a.test",
                None,
                None,
                "",
                vec![first.id, second.id],
            ))
            .unwrap()
            .id;
        store
            .create_entry(Entry::new(
                "https://b.test",
                None,
                None,
                "",
                vec![first.id, other.id],
            ))
            .unwrap();
        let later = store
            .create_entry(Entry::new(
                "https://c.test",
                None,
                None,
                "",
                vec![first.id, second.id],
            ))
            .unwrap()
            .id;

        let found = store.find_entries(&[first.id, second.id], None).unwrap();
        assert_eq!(
            found.iter().map(|entry| entry.id).collect::<Vec<_>>(),
            vec![earlier, later]
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn test_find_entries_by_query_set_operations() {
        let (store, dir) = temp_store("query_sets");
        let a = store.create_tag(Tag::new("a", "")).unwrap().id;
        let b = store.create_tag(Tag::new("b", "")).unwrap().id;
        let unknown = TagId::new();

        let e1 = store
            .create_entry(Entry::new("https://ab.test", None, None, "", vec![a, b]))
            .unwrap()
            .id;
        let e2 = store
            .create_entry(Entry::new("https://a.test", None, None, "", vec![a]))
            .unwrap()
            .id;
        let e3 = store
            .create_entry(Entry::new("https://b.test", None, None, "", vec![b]))
            .unwrap()
            .id;
        let e4 = store
            .create_entry(Entry::new("https://none.test", None, None, "", vec![]))
            .unwrap()
            .id;

        let ids = |query: &ResolvedQuery| {
            store
                .find_entries_by_query(query, None)
                .unwrap()
                .iter()
                .map(|e| e.id)
                .collect::<Vec<_>>()
        };

        // Intersection
        assert_eq!(
            ids(&Query::And(vec![Query::Tag(a), Query::Tag(b)])),
            vec![e1]
        );
        // Union
        assert_eq!(
            ids(&Query::Or(vec![Query::Tag(a), Query::Tag(b)])),
            vec![e1, e2, e3]
        );
        // Difference via NOT
        assert_eq!(
            ids(&Query::And(vec![
                Query::Tag(a),
                Query::Not(Box::new(Query::Tag(b)))
            ])),
            vec![e2]
        );
        // Complement over all entries
        assert_eq!(ids(&Query::Not(Box::new(Query::Tag(a)))), vec![e3, e4]);
        // Unknown tag ids match nothing; their complement matches everything
        assert!(ids(&Query::Tag(unknown)).is_empty());
        assert_eq!(
            ids(&Query::Not(Box::new(Query::Tag(unknown)))),
            vec![e1, e2, e3, e4]
        );
        // Archiver filter still applies
        let with_archiver = store
            .create_entry(Entry::new(
                "https://curl.test",
                Some("curl".to_string()),
                None,
                "",
                vec![a],
            ))
            .unwrap()
            .id;
        assert_eq!(
            store
                .find_entries_by_query(&Query::Tag(a), Some(Some("curl")))
                .unwrap()
                .iter()
                .map(|e| e.id)
                .collect::<Vec<_>>(),
            vec![with_archiver]
        );

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn test_find_entries_by_query_page() {
        let (store, dir) = temp_store("query_page");
        let a = store.create_tag(Tag::new("a", "")).unwrap().id;
        for i in 0..25 {
            store
                .create_entry(Entry::new(
                    format!("https://e/{i}"),
                    None,
                    None,
                    "",
                    vec![a],
                ))
                .unwrap();
        }

        let page = |limit, offset| {
            store
                .find_entries_by_query_page(&Query::Tag(a), limit, offset)
                .unwrap()
                .iter()
                .map(|e| e.url.clone())
                .collect::<Vec<_>>()
        };

        let page0 = page(10, 0);
        assert_eq!(page0.len(), 10);
        assert_eq!(page0.first().unwrap(), "https://e/0");
        assert_eq!(page0.last().unwrap(), "https://e/9");

        let page1 = page(10, 10);
        assert_eq!(page1.len(), 10);
        assert_eq!(page1.first().unwrap(), "https://e/10");

        let page2 = page(10, 20);
        assert_eq!(page2.len(), 5);
        assert_eq!(page2.first().unwrap(), "https://e/20");

        assert!(page(10, 25).is_empty());
        assert_eq!(page(11, 0).len(), 11);

        // The unpaged API still returns everything.
        assert_eq!(
            store
                .find_entries_by_query(&Query::Tag(a), None)
                .unwrap()
                .len(),
            25
        );

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn test_is_empty_clear_all_import_bulk() {
        let (store, dir) = temp_store("bulk");
        assert!(store.is_empty().unwrap());

        let tag = Tag::with_aliases("t", "d", vec!["a".to_string()]);
        let tag_id = tag.id;
        let entry = Entry::new(
            "https://x.test",
            None,
            None,
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
                    None,
                    None,
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
