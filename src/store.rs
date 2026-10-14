use std::path::PathBuf;

use crate::domain::{Entry, EntryId, Tag, TagId};
use anyhow::{bail, Context, Result};
use serde::{Deserialize, Serialize};

#[derive(Debug, Default, Serialize, Deserialize)]
struct TagsFile {
    #[serde(default)]
    tags: Vec<Tag>,
}

#[derive(Debug, Default, Serialize, Deserialize)]
struct EntriesFile {
    #[serde(default)]
    entries: Vec<Entry>,
}

/// File-based store using TOML.
///
/// All data is read from / written to two files: `tags.toml` and `entries.toml`
/// inside the configured data directory.
pub struct TomlStore {
    data_dir: PathBuf,
}

impl TomlStore {
    pub fn new(data_dir: impl Into<PathBuf>) -> Result<Self> {
        let data_dir = data_dir.into();
        std::fs::create_dir_all(&data_dir)
            .with_context(|| format!("Failed to create data directory: {}", data_dir.display()))?;
        Ok(Self { data_dir })
    }

    pub fn data_dir(&self) -> &std::path::Path {
        &self.data_dir
    }

    fn tags_path(&self) -> PathBuf {
        self.data_dir.join("tags.toml")
    }

    fn entries_path(&self) -> PathBuf {
        self.data_dir.join("entries.toml")
    }

    fn load_tags(&self) -> Result<TagsFile> {
        let path = self.tags_path();
        if !path.exists() {
            return Ok(TagsFile::default());
        }
        let content = std::fs::read_to_string(&path)
            .with_context(|| format!("Failed to read {}", path.display()))?;
        toml::from_str(&content).context("Failed to parse tags.toml")
    }

    fn save_tags(&self, file: &TagsFile) -> Result<()> {
        let content = toml::to_string_pretty(file).context("Failed to serialize tags")?;
        std::fs::write(self.tags_path(), content)?;
        Ok(())
    }

    fn load_entries(&self) -> Result<EntriesFile> {
        let path = self.entries_path();
        if !path.exists() {
            return Ok(EntriesFile::default());
        }
        let content = std::fs::read_to_string(&path)
            .with_context(|| format!("Failed to read {}", path.display()))?;
        toml::from_str(&content).context("Failed to parse entries.toml")
    }

    fn save_entries(&self, file: &EntriesFile) -> Result<()> {
        let content = toml::to_string_pretty(file).context("Failed to serialize entries")?;
        std::fs::write(self.entries_path(), content)?;
        Ok(())
    }

    // ── Tag operations ──────────────────────────────────────────────

    pub fn create_tag(&self, tag: Tag) -> Result<Tag> {
        let mut file = self.load_tags()?;
        file.tags.push(tag.clone());
        self.save_tags(&file)?;
        Ok(tag)
    }

    pub fn list_tags(&self) -> Result<Vec<Tag>> {
        Ok(self.load_tags()?.tags)
    }

    pub fn rename_tag(&self, id: &TagId, new_name: &str) -> Result<Tag> {
        let mut file = self.load_tags()?;
        let tag = file
            .tags
            .iter_mut()
            .find(|t| t.id == *id)
            .context("Tag not found")?;
        tag.name = new_name.to_string();
        tag.aliases.retain(|a| a != new_name);
        let updated = tag.clone();
        self.save_tags(&file)?;
        Ok(updated)
    }

    #[allow(dead_code)]
    pub fn find_tags_by_name(&self, name: &str) -> Result<Vec<Tag>> {
        let file = self.load_tags()?;
        Ok(file.tags.into_iter().filter(|t| t.name == name).collect())
    }

    pub fn find_tags_by_name_or_alias(&self, query: &str) -> Result<Vec<Tag>> {
        let file = self.load_tags()?;
        Ok(file
            .tags
            .into_iter()
            .filter(|t| t.name == query || t.aliases.iter().any(|a| a == query))
            .collect())
    }

    pub fn find_tags_by_name_and_specifier(
        &self,
        name_or_alias: &str,
        specifier: &str,
    ) -> Result<Vec<Tag>> {
        let file = self.load_tags()?;
        Ok(file
            .tags
            .into_iter()
            .filter(|t| {
                let matches_name =
                    t.name == name_or_alias || t.aliases.iter().any(|a| a == name_or_alias);
                let matches_specifier = t.specifier.as_deref() == Some(specifier);
                matches_name && matches_specifier
            })
            .collect())
    }

    pub fn find_tag_by_id(&self, id: &TagId) -> Result<Option<Tag>> {
        let file = self.load_tags()?;
        Ok(file.tags.into_iter().find(|t| t.id == *id))
    }

    pub fn add_tag_alias(&self, id: &TagId, alias: &str) -> Result<Tag> {
        let alias = alias.trim();
        if alias.is_empty() {
            bail!("Alias cannot be empty");
        }
        let mut file = self.load_tags()?;
        let tag = file
            .tags
            .iter_mut()
            .find(|t| t.id == *id)
            .context("Tag not found")?;
        if tag.name == alias {
            bail!("Alias cannot be identical to the tag name");
        }
        if tag.aliases.iter().any(|a| a == alias) {
            bail!("Tag already has alias '{}'", alias);
        }
        tag.aliases.push(alias.to_string());
        let updated = tag.clone();
        self.save_tags(&file)?;
        Ok(updated)
    }

    pub fn remove_tag_alias(&self, id: &TagId, alias: &str) -> Result<Tag> {
        let alias = alias.trim();
        let mut file = self.load_tags()?;
        let tag = file
            .tags
            .iter_mut()
            .find(|t| t.id == *id)
            .context("Tag not found")?;
        let pos = tag
            .aliases
            .iter()
            .position(|a| a == alias)
            .with_context(|| format!("Alias '{}' not found on tag '{}'", alias, tag.name))?;
        tag.aliases.remove(pos);
        let updated = tag.clone();
        self.save_tags(&file)?;
        Ok(updated)
    }

    // ── Entry operations ────────────────────────────────────────────

    pub fn create_entry(&self, entry: Entry) -> Result<Entry> {
        let mut file = self.load_entries()?;
        file.entries.push(entry.clone());
        self.save_entries(&file)?;
        Ok(entry)
    }

    pub fn list_entries(&self) -> Result<Vec<Entry>> {
        Ok(self.load_entries()?.entries)
    }

    pub fn find_entry_by_id(&self, id: &EntryId) -> Result<Option<Entry>> {
        let file = self.load_entries()?;
        Ok(file.entries.into_iter().find(|e| e.id == *id))
    }

    pub fn find_entries_by_tag(&self, tag_id: &TagId) -> Result<Vec<Entry>> {
        let file = self.load_entries()?;
        Ok(file
            .entries
            .into_iter()
            .filter(|e| e.tag_ids.contains(tag_id))
            .collect())
    }

    pub fn add_tag_to_entry(&self, entry_id: &EntryId, tag_id: &TagId) -> Result<()> {
        let mut file = self.load_entries()?;
        let entry = file
            .entries
            .iter_mut()
            .find(|e| e.id == *entry_id)
            .context("Entry not found")?;
        if !entry.tag_ids.contains(tag_id) {
            entry.tag_ids.push(*tag_id);
        }
        self.save_entries(&file)?;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_rename_tag() {
        let dir = std::env::temp_dir().join(format!("pantagraph_test_{}", uuid::Uuid::now_v7()));
        let store = TomlStore::new(&dir).unwrap();
        let tag = store.create_tag(Tag::new("old_name", "desc")).unwrap();

        let updated = store.rename_tag(&tag.id, "new_name").unwrap();
        assert_eq!(updated.name, "new_name");
        assert_eq!(updated.description, "desc");
        assert_eq!(updated.id, tag.id);

        let fetched = store.find_tag_by_id(&tag.id).unwrap().unwrap();
        assert_eq!(fetched.name, "new_name");

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn test_rename_nonexistent_tag() {
        let dir = std::env::temp_dir().join(format!("pantagraph_test_{}", uuid::Uuid::now_v7()));
        let store = TomlStore::new(&dir).unwrap();
        let fake_id = TagId::new();

        assert!(store.rename_tag(&fake_id, "new_name").is_err());

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn test_add_and_remove_tag_alias() {
        let dir =
            std::env::temp_dir().join(format!("pantagraph_test_alias_{}", uuid::Uuid::now_v7()));
        let store = TomlStore::new(&dir).unwrap();
        let tag = store.create_tag(Tag::new("rust", "Rust lang")).unwrap();

        // Add alias
        let updated = store.add_tag_alias(&tag.id, "rs").unwrap();
        assert_eq!(updated.aliases, vec!["rs"]);

        // Cannot add duplicate alias
        assert!(store.add_tag_alias(&tag.id, "rs").is_err());

        // Cannot add empty alias
        assert!(store.add_tag_alias(&tag.id, "  ").is_err());

        // Cannot add alias matching tag name
        assert!(store.add_tag_alias(&tag.id, "rust").is_err());

        // Add second alias
        let updated2 = store.add_tag_alias(&tag.id, "rust-lang").unwrap();
        assert_eq!(updated2.aliases, vec!["rs", "rust-lang"]);

        // Find by alias
        let found = store.find_tags_by_name_or_alias("rs").unwrap();
        assert_eq!(found.len(), 1);
        assert_eq!(found[0].id, tag.id);

        let found_by_name = store.find_tags_by_name_or_alias("rust").unwrap();
        assert_eq!(found_by_name.len(), 1);
        assert_eq!(found_by_name[0].id, tag.id);

        // Remove alias
        let after_remove = store.remove_tag_alias(&tag.id, "rs").unwrap();
        assert_eq!(after_remove.aliases, vec!["rust-lang"]);

        // Removing non-existent alias errors
        assert!(store.remove_tag_alias(&tag.id, "rs").is_err());

        // Renaming to existing alias removes it from aliases
        let renamed = store.rename_tag(&tag.id, "rust-lang").unwrap();
        assert_eq!(renamed.name, "rust-lang");
        assert!(renamed.aliases.is_empty());

        let _ = std::fs::remove_dir_all(&dir);
    }
}
