use std::path::PathBuf;

use crate::domain::{Entry, EntryId, Tag, TagId};
use anyhow::{bail, Context, Result};
use serde::{Deserialize, Serialize};

use crate::cache::{ArchiverFilter, SourceSignature, SqliteCache};

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
    cache: SqliteCache,
}

impl TomlStore {
    pub fn new(data_dir: impl Into<PathBuf>) -> Result<Self> {
        let data_dir = data_dir.into();
        std::fs::create_dir_all(&data_dir)
            .with_context(|| format!("Failed to create data directory: {}", data_dir.display()))?;
        let cache = SqliteCache::new(&data_dir);
        Ok(Self { data_dir, cache })
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
        self.cache.invalidate();
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
        self.cache.invalidate();
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
        self.cache.invalidate();
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
        self.cache.invalidate();
        Ok(updated)
    }

    // ── Entry operations ────────────────────────────────────────────

    pub fn create_entry(&self, entry: Entry) -> Result<Entry> {
        let mut file = self.load_entries()?;
        file.entries.push(entry.clone());
        self.save_entries(&file)?;
        self.cache.invalidate();
        Ok(entry)
    }

    pub fn list_entries(&self) -> Result<Vec<Entry>> {
        self.find_entries(&[], None)
    }

    pub fn find_entry_by_id(&self, id: &EntryId) -> Result<Option<Entry>> {
        Ok(self
            .list_entries()?
            .into_iter()
            .find(|entry| entry.id == *id))
    }

    pub fn find_entries_by_tag(&self, tag_id: &TagId) -> Result<Vec<Entry>> {
        self.find_entries(&[*tag_id], None)
    }

    /// Find entries carrying every supplied tag, optionally filtered by archiver.
    /// `Some(None)` selects entries without an archiver.
    pub fn find_entries(
        &self,
        tag_ids: &[TagId],
        archiver: Option<Option<&str>>,
    ) -> Result<Vec<Entry>> {
        let signature = SourceSignature::read(&self.tags_path(), &self.entries_path())?;
        let archiver_filter = match archiver {
            None => ArchiverFilter::Any,
            Some(None) => ArchiverFilter::WithoutArchiver,
            Some(Some(name)) => ArchiverFilter::Named(name),
        };
        if let Ok(Some(entries)) = self
            .cache
            .query_entries(&signature, tag_ids, archiver_filter)
        {
            return Ok(entries);
        }

        let tags = self.load_tags()?.tags;
        let entries = self.load_entries()?.entries;
        let current_signature = SourceSignature::read(&self.tags_path(), &self.entries_path())?;
        if signature == current_signature {
            let _ = self.cache.rebuild(&signature, &tags, &entries);
            if let Ok(Some(cached)) = self
                .cache
                .query_entries(&signature, tag_ids, archiver_filter)
            {
                return Ok(cached);
            }
        }

        Ok(filter_entries(entries, tag_ids, archiver_filter))
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
        self.cache.invalidate();
        Ok(())
    }
}

fn filter_entries(
    mut entries: Vec<Entry>,
    tag_ids: &[TagId],
    archiver: ArchiverFilter<'_>,
) -> Vec<Entry> {
    entries.retain(|entry| {
        let matches_archiver = match archiver {
            ArchiverFilter::Any => true,
            ArchiverFilter::WithoutArchiver => entry.archiver.is_none(),
            ArchiverFilter::Named(name) => entry.archiver.as_deref() == Some(name),
        };
        matches_archiver && tag_ids.iter().all(|tag_id| entry.tag_ids.contains(tag_id))
    });
    entries
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

    #[test]
    fn cache_build_preserves_tag_order_and_rebuilds_without_touching_toml() {
        let dir = std::env::temp_dir().join(format!("pantagraph_cache_{}", uuid::Uuid::now_v7()));
        let store = TomlStore::new(&dir).unwrap();
        let first = store.create_tag(Tag::new("first", "")).unwrap();
        let second = store.create_tag(Tag::new("second", "")).unwrap();
        let entry = Entry::new(
            "https://cache.example",
            None,
            None,
            "cached",
            vec![second.id, first.id, second.id],
        );
        let entry = store.create_entry(entry).unwrap();

        let canonical_tags = std::fs::read(store.tags_path()).unwrap();
        let canonical_entries = std::fs::read(store.entries_path()).unwrap();
        let cached = store.list_entries().unwrap();
        assert_eq!(cached[0].tag_ids, vec![second.id, first.id, second.id]);
        assert!(dir.join("cache.sqlite3").exists());

        store.cache.invalidate();
        let rebuilt = store.list_entries().unwrap();
        assert_eq!(rebuilt[0].id, entry.id);
        assert_eq!(rebuilt[0].tag_ids, cached[0].tag_ids);
        assert_eq!(std::fs::read(store.tags_path()).unwrap(), canonical_tags);
        assert_eq!(
            std::fs::read(store.entries_path()).unwrap(),
            canonical_entries
        );

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn cache_filters_single_and_multiple_tags_and_recovers_stale_or_corrupt_index() {
        let dir =
            std::env::temp_dir().join(format!("pantagraph_cache_filter_{}", uuid::Uuid::now_v7()));
        let store = TomlStore::new(&dir).unwrap();
        let first = store.create_tag(Tag::new("first", "")).unwrap();
        let second = store.create_tag(Tag::new("second", "")).unwrap();
        let only_first = store
            .create_entry(Entry::new(
                "https://one.example",
                None,
                None,
                "",
                vec![first.id],
            ))
            .unwrap();
        let both = store
            .create_entry(Entry::new(
                "https://both.example",
                Some("archive".to_string()),
                None,
                "",
                vec![first.id, second.id],
            ))
            .unwrap();

        let first_matches = store.find_entries_by_tag(&first.id).unwrap();
        assert_eq!(first_matches.len(), 2);
        let all_matches = store.find_entries(&[first.id, second.id], None).unwrap();
        assert_eq!(
            all_matches.iter().map(|entry| entry.id).collect::<Vec<_>>(),
            vec![both.id]
        );
        let archiver_matches = store.find_entries(&[], Some(Some("archive"))).unwrap();
        assert_eq!(
            archiver_matches
                .iter()
                .map(|entry| entry.id)
                .collect::<Vec<_>>(),
            vec![both.id]
        );

        // An external TOML edit makes the cached source signature stale.
        let mut entries = store.load_entries().unwrap();
        let external = Entry::new("https://external.example", None, None, "", vec![second.id]);
        let external_id = external.id;
        entries.entries.push(external);
        std::fs::write(
            store.entries_path(),
            toml::to_string_pretty(&entries).unwrap(),
        )
        .unwrap();
        let refreshed = store.find_entries_by_tag(&second.id).unwrap();
        assert_eq!(
            refreshed.iter().map(|entry| entry.id).collect::<Vec<_>>(),
            vec![both.id, external_id]
        );

        // A damaged cache is disposable and rebuilt from TOML.
        store.cache.invalidate();
        std::fs::write(dir.join("cache.sqlite3"), b"not a sqlite database").unwrap();
        let recovered = store.find_entries_by_tag(&first.id).unwrap();
        assert_eq!(recovered.len(), 2);
        assert_eq!(recovered[0].id, only_first.id);

        let _ = std::fs::remove_dir_all(&dir);
    }
}
