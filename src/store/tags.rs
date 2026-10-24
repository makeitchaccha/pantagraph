use anyhow::{Context, Result};
use rusqlite::{params, OptionalExtension};

use super::{parse_uuid_id, Store};
use crate::domain::{Tag, TagId};

impl Store {
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
}

#[cfg(test)]
mod tests {
    use super::*;

    fn temp_store(tag: &str) -> (Store, std::path::PathBuf) {
        let dir = std::env::temp_dir().join(format!(
            "pantagraph_test_store_{tag}_{}",
            uuid::Uuid::now_v7()
        ));
        let store = Store::new(&dir).unwrap();
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
}
