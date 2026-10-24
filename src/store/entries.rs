use anyhow::Result;
use rusqlite::{params, Connection, OptionalExtension};

use super::{archiver_filter_predicate, parse_uuid_id, to_archiver_filter, Store};
use crate::domain::{Entry, EntryId, TagId};

impl Store {
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

    pub(crate) fn load_entry_with_tags(
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
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::domain::Tag;

    fn temp_store(tag: &str) -> (Store, std::path::PathBuf) {
        let dir = std::env::temp_dir().join(format!(
            "pantagraph_test_store_{tag}_{}",
            uuid::Uuid::now_v7()
        ));
        let store = Store::new(&dir).unwrap();
        (store, dir)
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
}
