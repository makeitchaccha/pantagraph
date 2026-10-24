use anyhow::{bail, Result};
use rusqlite::{params, OptionalExtension};

use super::Store;
use crate::domain::EntryId;

impl Store {
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
            bail!("Entry not found: {entry_id}");
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
    pub fn get_entry_meta(
        &self,
        entry_id: &EntryId,
    ) -> Result<std::collections::BTreeMap<String, String>> {
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
    use crate::domain::{Entry, Tag};

    fn temp_store(tag: &str) -> (Store, std::path::PathBuf) {
        let dir = std::env::temp_dir().join(format!(
            "pantagraph_test_store_{tag}_{}",
            uuid::Uuid::now_v7()
        ));
        let store = Store::new(&dir).unwrap();
        (store, dir)
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
}
