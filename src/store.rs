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
            "INSERT INTO entries (id, url, description, ordinal) VALUES (?1, ?2, ?3, ?4)",
            params![entry.id.to_string(), entry.url, entry.description, ordinal],
        )?;
        let internal_id: i64 = tx.last_insert_rowid();
        for tid in &entry.tag_ids {
            tx.execute(
                "INSERT OR IGNORE INTO entry_tags (entry_id, tag_id)
                 VALUES (?1, (SELECT internal_id FROM tags WHERE id = ?2))",
                params![internal_id, tid.to_string()],
            )?;
        }
        let mut meta_key_stmt =
            tx.prepare("INSERT OR IGNORE INTO meta_keys (key) VALUES (?1)")?;
        let mut meta_value_stmt = tx.prepare(
            "INSERT OR REPLACE INTO entry_meta (entry_id, key_id, value)
             VALUES (?1, (SELECT internal_id FROM meta_keys WHERE key = ?2), ?3)",
        )?;
        for (key, value) in &entry.meta {
            meta_key_stmt.execute(params![key])?;
            meta_value_stmt.execute(params![internal_id, key, value])?;
        }
        drop(meta_key_stmt);
        drop(meta_value_stmt);
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
        let description: String = entry_row.get(2)?;
        let _ordinal: i64 = entry_row.get(3)?;
        let id = parse_uuid_id::<EntryId>(&id_str)?;
        let internal_id: i64 = entry_row.get(4)?;
        let mut stmt = conn.prepare(
            "SELECT t.id FROM entry_tags et JOIN tags t ON t.internal_id = et.tag_id
             WHERE et.entry_id = ?1",
        )?;
        let mut tag_ids = Vec::new();
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
        let mut meta_stmt = conn.prepare(
            "SELECT mk.key, em.value FROM entry_meta em
             JOIN meta_keys mk ON mk.internal_id = em.key_id
             WHERE em.entry_id = ?1
             ORDER BY mk.key",
        )?;
        let mut meta = std::collections::BTreeMap::new();
        let meta_rows = meta_stmt.query_map(params![internal_id], |r| {
            let key: String = r.get(0)?;
            let value: String = r.get(1)?;
            Ok((key, value))
        })?;
        for r in meta_rows {
            let (key, value) = r?;
            meta.insert(key, value);
        }
        Ok(Entry {
            id,
            url,
            description,
            tag_ids,
            meta,
        })
    }
    pub fn list_entries(&self) -> Result<Vec<Entry>> {
        self.find_entries(&[], None)
    }

    pub fn find_entry_by_id(&self, id: &EntryId) -> Result<Option<Entry>> {
        let conn = self.conn()?;
        let mut stmt = conn.prepare(
            "SELECT id, url, description, ordinal, internal_id FROM entries WHERE id = ?1",
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
    /// The archiver filter and the page are both pushed into SQL so this
    /// function never applies an in-memory filter or slice.
    pub fn find_entries_page(
        &self,
        tag_ids: &[TagId],
        archiver: Option<Option<&str>>,
        page: Option<(usize, usize)>,
    ) -> Result<Vec<Entry>> {
        let conn = self.conn()?;
        let archiver_filter = to_archiver_filter(archiver);

        let mut internal_tag_ids = Vec::with_capacity(tag_ids.len());
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
                // A referenced tag is missing entirely: this is an
                // internal inconsistency (callers resolve names to ids
                // first), so surface it as an error rather than silently
                // matching nothing.
                anyhow::bail!("Tag id {} not found in store", tag_id);
            }
        }

        let mut params_vec: Vec<rusqlite::types::Value> = Vec::new();
        let mut counter = 0;
        let mut predicates: Vec<String> = Vec::new();

        for (i, internal_id) in internal_tag_ids.iter().enumerate() {
            params_vec.push(rusqlite::types::Value::Integer(*internal_id));
            predicates.push(format!(
                "EXISTS (SELECT 1 FROM entry_tags matching_{i}
                 WHERE matching_{i}.entry_id = e.internal_id
                   AND matching_{i}.tag_id = ?{})",
                params_vec.len()
            ));
        }

        predicates.push(archiver_filter_predicate(
            archiver_filter,
            "e",
            &mut params_vec,
            &mut counter,
        ));
        let where_clause = predicates.join(" AND ");

        let paging_clause = match page {
            Some(_) => format!(
                " LIMIT ?{} OFFSET ?{}",
                params_vec.len() + 1,
                params_vec.len() + 2
            ),
            None => String::new(),
        };

        let sql = format!(
            "SELECT e.id, e.url, e.description, e.ordinal, e.internal_id
             FROM entries e
             WHERE {where_clause}
             ORDER BY e.ordinal{paging_clause}"
        );
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
    /// into SQL.
    ///
    /// The archiver filter and the page are both pushed into SQL so this
    /// function never applies an in-memory filter or slice.
    pub fn find_entries_by_query_page_opt(
        &self,
        query: &ResolvedQuery,
        archiver: Option<Option<&str>>,
        page: Option<(usize, usize)>,
    ) -> Result<Vec<Entry>> {
        let archiver_filter = to_archiver_filter(archiver);
        self.query_entries(query, archiver_filter, page)
    }

    /// Find at most `limit` entries satisfying a boolean query, starting
    /// at `offset`, ordered by insertion order.
    ///
    /// The limit is pushed into SQL so callers can page through the full
    /// result set without loading it into memory.
    pub fn find_entries_by_query_page(
        &self,
        query: &ResolvedQuery,
        limit: usize,
        offset: usize,
    ) -> Result<Vec<Entry>> {
        self.query_entries(query, ArchiverFilter::Any, Some((limit, offset)))
    }

    fn query_entries(
        &self,
        query: &ResolvedQuery,
        archiver_filter: ArchiverFilter<'_>,
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
        let mut predicate = query_predicate(query, &internal_ids, &mut params_vec, &mut counter);
        let archiver_clause =
            archiver_filter_predicate(archiver_filter, "e", &mut params_vec, &mut counter);
        predicate = format!("({predicate}) AND {archiver_clause}");
        let sql = match page {
            Some(_) => format!(
                "SELECT e.id, e.url, e.description, e.ordinal, e.internal_id
                 FROM entries e
                 WHERE {predicate}
                 ORDER BY e.ordinal
                 LIMIT ?{} OFFSET ?{}",
                params_vec.len() + 1,
                params_vec.len() + 2
            ),
            None => format!(
                "SELECT e.id, e.url, e.description, e.ordinal, e.internal_id
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

    /// Set (or overwrite) `key` on an entry's meta attributes.
    pub fn set_entry_meta(&self, entry_id: &EntryId, key: &str, value: &str) -> Result<()> {
        let conn = self.conn()?;
        let tx = conn.unchecked_transaction()?;
        let internal_id: Option<i64> = tx
            .query_row(
                "SELECT internal_id FROM entries WHERE id = ?1",
                params![entry_id.to_string()],
                |row| row.get(0),
            )
            .optional()?;
        let Some(internal_id) = internal_id else {
            anyhow::bail!("Entry not found: {entry_id}");
        };
        tx.execute("INSERT OR IGNORE INTO meta_keys (key) VALUES (?1)", params![key])?;
        tx.execute(
            "INSERT OR REPLACE INTO entry_meta (entry_id, key_id, value)
             VALUES (?1, (SELECT internal_id FROM meta_keys WHERE key = ?2), ?3)",
            params![internal_id, key, value],
        )?;
        tx.commit()?;
        Ok(())
    }

    /// Remove `key` from an entry's meta attributes.
    ///
    /// Removing a key that is not set is not an error. Returns whether the
    /// key was present.
    pub fn remove_entry_meta(&self, entry_id: &EntryId, key: &str) -> Result<bool> {
        let conn = self.conn()?;
        let removed = conn.execute(
            "DELETE FROM entry_meta
             WHERE entry_id = (SELECT internal_id FROM entries WHERE id = ?1)
               AND key_id = (SELECT internal_id FROM meta_keys WHERE key = ?2)",
            params![entry_id.to_string(), key],
        )?;
        Ok(removed > 0)
    }

    /// Return one entry's meta attributes, sorted by key.
    pub fn get_entry_meta(&self, entry_id: &EntryId) -> Result<std::collections::BTreeMap<String, String>> {
        let conn = self.conn()?;
        let mut stmt = conn.prepare(
            "SELECT mk.key, em.value FROM entry_meta em
             JOIN meta_keys mk ON mk.internal_id = em.key_id
             JOIN entries e ON e.internal_id = em.entry_id
             WHERE e.id = ?1
             ORDER BY mk.key",
        )?;
        let rows = stmt.query_map(params![entry_id.to_string()], |r| {
            Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?))
        })?;
        let mut meta = std::collections::BTreeMap::new();
        for row in rows {
            let (key, value) = row?;
            meta.insert(key, value);
        }
        Ok(meta)
    }

    /// Return every meta key known to the store, sorted.
    pub fn list_meta_keys(&self) -> Result<Vec<String>> {
        let conn = self.conn()?;
        let mut stmt = conn.prepare("SELECT key FROM meta_keys ORDER BY key")?;
        let rows = stmt.query_map([], |r| r.get::<_, String>(0))?;
        let mut keys = Vec::new();
        for row in rows {
            keys.push(row?);
        }
        Ok(keys)
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

        let mut e1 = Entry::new(
            "https://a.test",
            "first",
            vec![tag_id],
        );
        e1.meta.insert("archived".to_string(), "true".to_string());
        e1.meta.insert("archiver".to_string(), "curl".to_string());
        e1.meta
            .insert("archive_path".to_string(), "archives/a.html".to_string());
        let e2 = Entry::new("https://b.test", "second", vec![]);
        let e3 = Entry::new("https://c.test", "third", vec![tag_id]);

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
        assert_eq!(found.meta.get("archiver").map(String::as_str), Some("curl"));
        assert_eq!(found.tag_ids, vec![tag_id]);
        assert!(store.find_entry_by_id(&EntryId::new()).unwrap().is_none());

        // find_entries_by_tag
        let by_tag = store.find_entries_by_tag(&tag_id).unwrap();
        assert_eq!(
            by_tag.iter().map(|e| e.id).collect::<Vec<_>>(),
            vec![id1, id3]
        );

        // archiver filters (via the `archiver` meta key)
        let no_archiver = store.find_entries(&[], Some(None)).unwrap();
        assert_eq!(
            no_archiver.iter().map(|e| e.id).collect::<Vec<_>>(),
            vec![id2, id3]
        );
        let curl = store.find_entries(&[], Some(Some("curl"))).unwrap();
        assert_eq!(curl.iter().map(|e| e.id).collect::<Vec<_>>(), vec![id1]);
        let archived = store.find_entries(&[], Some(Some("curl"))).unwrap();
        assert_eq!(archived[0].meta.get("archived").map(String::as_str), Some("true"));

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
                "",
                vec![first.id, second.id],
            ))
            .unwrap()
            .id;
        store
            .create_entry(Entry::new(
                "https://b.test",
                "",
                vec![first.id, other.id],
            ))
            .unwrap();
        let later = store
            .create_entry(Entry::new(
                "https://c.test",
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
            .create_entry(Entry::new("https://ab.test", "", vec![a, b]))
            .unwrap()
            .id;
        let e2 = store
            .create_entry(Entry::new("https://a.test", "", vec![a]))
            .unwrap()
            .id;
        let e3 = store
            .create_entry(Entry::new("https://b.test", "", vec![b]))
            .unwrap()
            .id;
        let e4 = store
            .create_entry(Entry::new("https://none.test", "", vec![]))
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
        let mut with_archiver_entry = Entry::new("https://curl.test", "", vec![a]);
        with_archiver_entry
            .meta
            .insert("archiver".to_string(), "curl".to_string());
        with_archiver_entry
            .meta
            .insert("archived".to_string(), "true".to_string());
        let with_archiver = store.create_entry(with_archiver_entry).unwrap().id;
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
    fn test_meta_set_get_remove() {
        let (store, dir) = temp_store("meta_crud");
        let tag = store.create_tag(Tag::new("t", "d")).unwrap();
        let entry = Entry::new("https://example.com", "desc", vec![tag.id]);
        let id = store.create_entry(entry).unwrap().id;

        store
            .set_entry_meta(&id, "archiver", "yt-dlp")
            .expect("set meta");
        store.set_entry_meta(&id, "archived", "true").expect("set");
        let meta = store.get_entry_meta(&id).unwrap();
        assert_eq!(meta.get("archiver").map(String::as_str), Some("yt-dlp"));
        assert_eq!(meta.get("archived").map(String::as_str), Some("true"));

        assert!(store.remove_entry_meta(&id, "archiver").unwrap());
        assert!(!store.remove_entry_meta(&id, "archiver").unwrap()); // no-op
        let meta2 = store.get_entry_meta(&id).unwrap();
        assert!(meta2.get("archiver").is_none());
        assert_eq!(meta2.get("archived").map(String::as_str), Some("true"));

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn test_query_meta_predicate() {
        let (store, dir) = temp_store("meta_query");
        let a = store.create_tag(Tag::new("a", "")).unwrap().id;
        let mut e1 = Entry::new("https://e1", "", vec![a]);
        e1.meta.insert("archiver".to_string(), "yt-dlp".to_string());
        e1.meta.insert("archived".to_string(), "true".to_string());
        let mut e2 = Entry::new("https://e2", "", vec![a]);
        e2.meta.insert("archiver".to_string(), "curl".to_string());
        let mut e3 = Entry::new("https://e3", "", vec![a]);
        e3.meta.insert("extra".to_string(), "value".to_string());
        let id1 = store.create_entry(e1).unwrap().id;
        let _id2 = store.create_entry(e2).unwrap().id;
        let _id3 = store.create_entry(e3).unwrap().id;

        fn ids(store: &JsonlStore, query: &crate::query::ResolvedQuery) -> Vec<crate::domain::EntryId> {
            store
                .find_entries_by_query(query, None)
                .unwrap()
                .into_iter()
                .map(|e| e.id)
                .collect()
        }
        use crate::query::Query;

        assert_eq!(ids(&store, &Query::Meta { key: "archiver".to_string(), value: Some("yt-dlp".to_string()) }), vec![id1]);
        assert_eq!(ids(&store, &Query::Meta { key: "archiver".to_string(), value: None }), vec![id1, _id2]);
        assert_eq!(ids(&store, &Query::Meta { key: "nonexist".to_string(), value: None }), vec![]);
        assert_eq!(ids(&store, &Query::Not(Box::new(Query::Meta { key: "archiver".to_string(), value: None }))), vec![_id3]);
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
