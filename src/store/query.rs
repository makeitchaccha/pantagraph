use std::collections::{HashMap, HashSet};
use anyhow::Result;
use rusqlite::{params, OptionalExtension};

use super::{
    archiver_filter_predicate, collect_tag_ids, query_predicate, to_archiver_filter,
    ArchiverFilter, Store,
};
use crate::domain::Entry;
use crate::query::ResolvedQuery;

impl Store {
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
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::domain::{Entry, Tag, TagId};
    use crate::query::Query;

    fn temp_store(tag: &str) -> (Store, std::path::PathBuf) {
        let dir = std::env::temp_dir().join(format!(
            "pantagraph_test_store_{tag}_{}",
            uuid::Uuid::now_v7()
        ));
        let store = Store::new(&dir).unwrap();
        (store, dir)
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

        fn ids(store: &Store, query: &ResolvedQuery) -> Vec<crate::domain::EntryId> {
            store
                .find_entries_by_query(query, None)
                .unwrap()
                .into_iter()
                .map(|e| e.id)
                .collect()
        }

        assert_eq!(ids(&store, &Query::Meta { key: "archiver".to_string(), value: Some("yt-dlp".to_string()) }), vec![id1]);
        assert_eq!(ids(&store, &Query::Meta { key: "archiver".to_string(), value: None }), vec![id1, _id2]);
        assert_eq!(ids(&store, &Query::Meta { key: "nonexist".to_string(), value: None }), vec![]);
        assert_eq!(ids(&store, &Query::Not(Box::new(Query::Meta { key: "archiver".to_string(), value: None }))), vec![_id3]);
        let _ = std::fs::remove_dir_all(&dir);
    }
}
