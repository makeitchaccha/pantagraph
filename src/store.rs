use std::path::PathBuf;

use crate::cache::{ArchiverFilter, SourceSignature, SqliteCache};
use crate::domain::{Entry, EntryId, Tag, TagId};
use anyhow::{bail, Context, Result};

/// File-based store using JSON Lines.
///
/// All data is read from / written to two files: `tags.jsonl` and `entries.jsonl`
/// inside the configured data directory.
pub struct JsonlStore {
    data_dir: PathBuf,
    cache: SqliteCache,
}

impl JsonlStore {
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
        self.data_dir.join("tags.jsonl")
    }

    fn entries_path(&self) -> PathBuf {
        self.data_dir.join("entries.jsonl")
    }

    fn migrate_legacy_file(&self, jsonl_path: &PathBuf, legacy_name: &str) -> Result<()> {
        if jsonl_path.exists() {
            return Ok(());
        }
        let legacy_path = self.data_dir.join(legacy_name);
        if !legacy_path.exists() {
            return Ok(());
        }
        let content = std::fs::read_to_string(&legacy_path)
            .with_context(|| format!("Failed to read {}", legacy_path.display()))?;
        let values: Vec<serde_json::Value> = if content.trim().is_empty() {
            Vec::new()
        } else {
            let root: serde_json::Value = toml::from_str::<toml::Value>(&content)
                .with_context(|| format!("Failed to parse {}", legacy_path.display()))?
                .try_into()
                .context("Failed to convert legacy TOML data")?;
            let key = if legacy_name == "tags.toml" {
                "tags"
            } else {
                "entries"
            };
            serde_json::from_value::<serde_json::Map<String, serde_json::Value>>(root)
                .context("Invalid legacy TOML data")?
                .get(key)
                .and_then(serde_json::Value::as_array)
                .cloned()
                .unwrap_or_default()
        };
        let lines = values
            .iter()
            .map(serde_json::to_string)
            .collect::<std::result::Result<Vec<_>, _>>()?;
        std::fs::write(
            jsonl_path,
            lines.join("\n") + if lines.is_empty() { "" } else { "\n" },
        )
        .with_context(|| format!("Failed to write {}", jsonl_path.display()))
    }

    fn load_tags(&self) -> Result<Vec<Tag>> {
        let path = self.tags_path();
        self.migrate_legacy_file(&path, "tags.toml")?;
        load_jsonl(&path)
    }

    fn save_tags(&self, tags: &[Tag]) -> Result<()> {
        save_jsonl(&self.tags_path(), tags)
    }

    fn load_entries(&self) -> Result<Vec<Entry>> {
        let path = self.entries_path();
        self.migrate_legacy_file(&path, "entries.toml")?;
        load_jsonl(&path)
    }

    fn save_entries(&self, entries: &[Entry]) -> Result<()> {
        save_jsonl(&self.entries_path(), entries)
    }

    // ── Tag operations ──────────────────────────────────────────────

    pub fn create_tag(&self, tag: Tag) -> Result<Tag> {
        let mut tags = self.load_tags()?;
        tags.push(tag.clone());
        self.save_tags(&tags)?;
        self.cache.invalidate();
        Ok(tag)
    }

    pub fn list_tags(&self) -> Result<Vec<Tag>> {
        self.load_tags()
    }

    pub fn rename_tag(&self, id: &TagId, new_name: &str) -> Result<Tag> {
        let mut tags = self.load_tags()?;
        let tag = tags
            .iter_mut()
            .find(|t| t.id == *id)
            .context("Tag not found")?;
        tag.name = new_name.to_string();
        tag.aliases.retain(|a| a != new_name);
        let updated = tag.clone();
        self.save_tags(&tags)?;
        self.cache.invalidate();
        Ok(updated)
    }

    #[allow(dead_code)]
    pub fn find_tags_by_name(&self, name: &str) -> Result<Vec<Tag>> {
        let tags = self.load_tags()?;
        Ok(tags.into_iter().filter(|t| t.name == name).collect())
    }

    pub fn find_tags_by_name_or_alias(&self, query: &str) -> Result<Vec<Tag>> {
        let tags = self.load_tags()?;
        Ok(tags
            .into_iter()
            .filter(|t| t.name == query || t.aliases.iter().any(|a| a == query))
            .collect())
    }

    pub fn find_tags_by_name_and_specifier(
        &self,
        name_or_alias: &str,
        specifier: &str,
    ) -> Result<Vec<Tag>> {
        let tags = self.load_tags()?;
        Ok(tags
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
        let tags = self.load_tags()?;
        Ok(tags.into_iter().find(|t| t.id == *id))
    }

    pub fn add_tag_alias(&self, id: &TagId, alias: &str) -> Result<Tag> {
        let alias = alias.trim();
        if alias.is_empty() {
            bail!("Alias cannot be empty");
        }
        let mut tags = self.load_tags()?;
        let tag = tags
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
        self.save_tags(&tags)?;
        self.cache.invalidate();
        Ok(updated)
    }

    pub fn remove_tag_alias(&self, id: &TagId, alias: &str) -> Result<Tag> {
        let alias = alias.trim();
        let mut tags = self.load_tags()?;
        let tag = tags
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
        self.save_tags(&tags)?;
        self.cache.invalidate();
        Ok(updated)
    }

    // ── Entry operations ────────────────────────────────────────────

    pub fn create_entry(&self, entry: Entry) -> Result<Entry> {
        let mut entries = self.load_entries()?;
        entries.push(entry.clone());
        self.save_entries(&entries)?;
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

        let tags_path = self.tags_path();
        let entries_path = self.entries_path();
        let current_signature = SourceSignature::read(&self.tags_path(), &self.entries_path())?;
        if signature == current_signature {
            if let Ok(Some(cached)) = self
                .cache
                .rebuild(&signature, &tags_path, &entries_path)
                .and_then(|()| {
                    self.cache
                        .query_entries(&signature, tag_ids, archiver_filter)
                })
            {
                return Ok(cached);
            }
        }

        let entries = self.load_entries()?;
        if signature == SourceSignature::read(&self.tags_path(), &self.entries_path())? {
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
        let mut entries = self.load_entries()?;
        let entry = entries
            .iter_mut()
            .find(|e| e.id == *entry_id)
            .context("Entry not found")?;
        if !entry.tag_ids.contains(tag_id) {
            entry.tag_ids.push(*tag_id);
        }
        self.save_entries(&entries)?;
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

fn load_jsonl<T: serde::de::DeserializeOwned>(path: &std::path::Path) -> Result<Vec<T>> {
    if !path.exists() {
        return Ok(Vec::new());
    }
    let content = std::fs::read_to_string(path)
        .with_context(|| format!("Failed to read {}", path.display()))?;
    content
        .lines()
        .enumerate()
        .filter(|(_, line)| !line.trim().is_empty())
        .map(|(index, line)| {
            serde_json::from_str(line).with_context(|| {
                format!("Failed to parse {} at line {}", path.display(), index + 1)
            })
        })
        .collect()
}

fn save_jsonl<T: serde::Serialize>(path: &std::path::Path, values: &[T]) -> Result<()> {
    let mut content = String::new();
    for value in values {
        content
            .push_str(&serde_json::to_string(value).context("Failed to serialize JSONL record")?);
        content.push('\n');
    }
    std::fs::write(path, content).with_context(|| format!("Failed to write {}", path.display()))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_rename_tag() {
        let dir = std::env::temp_dir().join(format!("pantagraph_test_{}", uuid::Uuid::now_v7()));
        let store = JsonlStore::new(&dir).unwrap();
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
    fn migrates_legacy_toml_to_jsonl_without_removing_source() {
        let dir = std::env::temp_dir().join(format!("pantagraph_migrate_{}", uuid::Uuid::now_v7()));
        std::fs::create_dir_all(&dir).unwrap();
        let tag = Tag::new("legacy", "from toml");
        let entry = Entry::new("https://legacy.example", None, None, "old", vec![tag.id]);
        let tags_toml = format!(
            "[[tags]]\nid = \"{}\"\nname = \"{}\"\ndescription = \"{}\"\naliases = []\n",
            tag.id, tag.name, tag.description
        );
        let entries_toml = format!(
            "[[entries]]\nid = \"{}\"\nurl = \"{}\"\ndescription = \"{}\"\ntag_ids = [\"{}\"]\n",
            entry.id, entry.url, entry.description, tag.id
        );
        std::fs::write(dir.join("tags.toml"), tags_toml).unwrap();
        std::fs::write(dir.join("entries.toml"), entries_toml).unwrap();

        let store = JsonlStore::new(&dir).unwrap();
        assert_eq!(store.list_tags().unwrap()[0].name, "legacy");
        assert_eq!(
            store.list_entries().unwrap()[0].url,
            "https://legacy.example"
        );
        assert!(dir.join("tags.jsonl").exists());
        assert!(dir.join("entries.jsonl").exists());
        assert!(dir.join("tags.toml").exists());
        assert!(dir.join("entries.toml").exists());

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn test_rename_nonexistent_tag() {
        let dir = std::env::temp_dir().join(format!("pantagraph_test_{}", uuid::Uuid::now_v7()));
        let store = JsonlStore::new(&dir).unwrap();
        let fake_id = TagId::new();

        assert!(store.rename_tag(&fake_id, "new_name").is_err());

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn test_add_and_remove_tag_alias() {
        let dir =
            std::env::temp_dir().join(format!("pantagraph_test_alias_{}", uuid::Uuid::now_v7()));
        let store = JsonlStore::new(&dir).unwrap();
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
    fn cache_build_preserves_tag_order_and_rebuilds_without_touching_jsonl() {
        let dir = std::env::temp_dir().join(format!("pantagraph_cache_{}", uuid::Uuid::now_v7()));
        let store = JsonlStore::new(&dir).unwrap();
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
        let store = JsonlStore::new(&dir).unwrap();
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

        // An external JSONL edit makes the cached source signature stale.
        let mut entries = store.load_entries().unwrap();
        let external = Entry::new("https://external.example", None, None, "", vec![second.id]);
        let external_id = external.id;
        entries.push(external);
        save_jsonl(&store.entries_path(), &entries).unwrap();
        let refreshed = store.find_entries_by_tag(&second.id).unwrap();
        assert_eq!(
            refreshed.iter().map(|entry| entry.id).collect::<Vec<_>>(),
            vec![both.id, external_id]
        );

        // A damaged cache is disposable and rebuilt from JSONL.
        store.cache.invalidate();
        std::fs::write(dir.join("cache.sqlite3"), b"not a sqlite database").unwrap();
        let recovered = store.find_entries_by_tag(&first.id).unwrap();
        assert_eq!(recovered.len(), 2);
        assert_eq!(recovered[0].id, only_first.id);

        let _ = std::fs::remove_dir_all(&dir);
    }
}
