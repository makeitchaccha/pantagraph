use std::io::{BufRead, BufReader, BufWriter, Write};
use std::path::{Path, PathBuf};

use anyhow::{bail, Context, Result};

use crate::domain::{Entry, Tag};
use crate::store::JsonlStore;

#[derive(Debug)]
pub struct ExportResult {
    pub out_dir: PathBuf,
    pub tag_count: usize,
    pub entry_count: usize,
}

#[derive(Debug)]
pub struct ImportResult {
    pub tag_count: usize,
    pub entry_count: usize,
}

fn tags_path(dir: &Path) -> PathBuf {
    dir.join("tags.jsonl")
}

fn entries_path(dir: &Path) -> PathBuf {
    dir.join("entries.jsonl")
}

fn load_jsonl<T: serde::de::DeserializeOwned>(path: &Path) -> Result<Vec<T>> {
    if !path.exists() {
        return Ok(Vec::new());
    }
    let file =
        std::fs::File::open(path).with_context(|| format!("Failed to open {}", path.display()))?;
    let reader = BufReader::new(file);
    let mut values = Vec::new();
    for (index, line) in reader.lines().enumerate() {
        let line = line
            .with_context(|| format!("Failed to read {} at line {}", path.display(), index + 1))?;
        if line.trim().is_empty() {
            continue;
        }
        let value = serde_json::from_str(&line)
            .with_context(|| format!("Failed to parse {} at line {}", path.display(), index + 1))?;
        values.push(value);
    }
    Ok(values)
}

fn save_jsonl<T: serde::Serialize>(path: &Path, values: &[T]) -> Result<()> {
    let file = std::fs::File::create(path)
        .with_context(|| format!("Failed to create {}", path.display()))?;
    let mut writer = BufWriter::new(file);
    for value in values {
        serde_json::to_writer(&mut writer, value).context("Failed to serialize JSONL record")?;
        writer
            .write_all(b"\n")
            .with_context(|| format!("Failed to write to {}", path.display()))?;
    }
    writer
        .flush()
        .with_context(|| format!("Failed to flush {}", path.display()))?;
    Ok(())
}

pub fn handle_export(store: &JsonlStore, out_dir: Option<String>) -> Result<ExportResult> {
    let out_dir = match out_dir {
        Some(dir) => PathBuf::from(dir),
        None => store.data_dir().to_path_buf(),
    };
    std::fs::create_dir_all(&out_dir)
        .with_context(|| format!("Failed to create directory {}", out_dir.display()))?;

    let tags = store.list_tags()?;
    let entries = store.list_entries()?;
    save_jsonl(&tags_path(&out_dir), &tags)?;
    save_jsonl(&entries_path(&out_dir), &entries)?;

    Ok(ExportResult {
        out_dir,
        tag_count: tags.len(),
        entry_count: entries.len(),
    })
}

pub fn handle_import(
    store: &JsonlStore,
    in_dir: Option<String>,
    overwrite: bool,
) -> Result<ImportResult> {
    let in_dir = match in_dir {
        Some(dir) => PathBuf::from(dir),
        None => store.data_dir().to_path_buf(),
    };
    let tags_file = tags_path(&in_dir);
    let entries_file = entries_path(&in_dir);
    if !tags_file.exists() && !entries_file.exists() {
        bail!(
            "No tags.jsonl or entries.jsonl found in {}",
            in_dir.display()
        );
    }
    let tags: Vec<Tag> = load_jsonl(&tags_file)?;
    let entries: Vec<Entry> = load_jsonl(&entries_file)?;

    if !store.is_empty()? && !overwrite {
        bail!(
            "Store is not empty. Pass --overwrite to replace its contents with {}",
            in_dir.display()
        );
    }
    if overwrite {
        store.clear_all()?;
    }
    store.import_bulk(&tags, &entries)?;

    Ok(ImportResult {
        tag_count: tags.len(),
        entry_count: entries.len(),
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::domain::{Entry, EntryId, Tag, TagId};

    fn temp_dir(tag: &str) -> PathBuf {
        std::env::temp_dir().join(format!("pantagraph_test_{tag}_{}", uuid::Uuid::now_v7()))
    }

    #[test]
    fn test_export_import_round_trip() {
        let store_dir = temp_dir("rt_store");
        let out_dir = temp_dir("rt_out");
        let store = JsonlStore::new(&store_dir).unwrap();

        let rust = Tag::with_aliases("rust", "lang", vec!["rs".to_string()]);
        let game = Tag::with_specifier("game", Some("jp".to_string()), "games", vec![]);
        let rust_id = store.create_tag(rust.clone()).unwrap().id;
        let game_id = store.create_tag(game.clone()).unwrap().id;

        let e1 = Entry::new(
            "https://example.com/a",
            Some("yt-dlp".to_string()),
            Some("archives/a.html".to_string()),
            "first",
            vec![rust_id],
        );
        let e2 = Entry::new("https://example.com/b", None, None, "second", vec![game_id]);
        store.create_entry(e1.clone()).unwrap();
        store.create_entry(e2.clone()).unwrap();

        let exported = handle_export(&store, Some(out_dir.to_string_lossy().into())).unwrap();
        assert_eq!(exported.tag_count, 2);
        assert_eq!(exported.entry_count, 2);
        assert!(out_dir.join("tags.jsonl").exists());
        assert!(out_dir.join("entries.jsonl").exists());

        let store2_dir = temp_dir("rt_store2");
        let store2 = JsonlStore::new(&store2_dir).unwrap();
        let imported =
            handle_import(&store2, Some(out_dir.to_string_lossy().into()), false).unwrap();
        assert_eq!(imported.tag_count, 2);
        assert_eq!(imported.entry_count, 2);

        let tags = store2.list_tags().unwrap();
        assert_eq!(tags.len(), 2);
        let rust2 = store2.find_tag_by_id(&rust_id).unwrap().unwrap();
        assert_eq!(rust2.name, "rust");
        assert_eq!(rust2.aliases, vec!["rs".to_string()]);
        let game2 = store2.find_tag_by_id(&game_id).unwrap().unwrap();
        assert_eq!(game2.specifier.as_deref(), Some("jp"));

        let entries = store2.list_entries().unwrap();
        assert_eq!(entries.len(), 2);
        assert_eq!(entries[0].id, e1.id);
        assert_eq!(entries[0].url, "https://example.com/a");
        assert_eq!(entries[0].archiver.as_deref(), Some("yt-dlp"));
        assert_eq!(entries[0].tag_ids, vec![rust_id]);
        assert_eq!(entries[1].id, e2.id);
        assert!(entries[1].archiver.is_none());

        let _ = std::fs::remove_dir_all(&store_dir);
        let _ = std::fs::remove_dir_all(&store2_dir);
        let _ = std::fs::remove_dir_all(&out_dir);
    }

    #[test]
    fn test_import_gating() {
        let store_dir = temp_dir("gate_store");
        let src_dir = temp_dir("gate_src");
        let store = JsonlStore::new(&store_dir).unwrap();

        // Non-empty source
        let tag = Tag::new("t", "d");
        let tid = store.create_tag(tag).unwrap().id;
        store
            .create_entry(Entry::new("https://x.test", None, None, "", vec![tid]))
            .unwrap();
        handle_export(&store, Some(src_dir.to_string_lossy().into())).unwrap();

        // Non-empty target, no --overwrite -> refuse
        let target_dir = temp_dir("gate_target");
        let target = JsonlStore::new(&target_dir).unwrap();
        target.create_tag(Tag::new("existing", "keep")).unwrap();
        let err = handle_import(&target, Some(src_dir.to_string_lossy().into()), false);
        assert!(err.is_err());
        assert!(err.unwrap_err().to_string().contains("--overwrite"));
        // Existing data untouched
        assert_eq!(target.list_tags().unwrap().len(), 1);

        // With --overwrite -> replaced
        let res = handle_import(&target, Some(src_dir.to_string_lossy().into()), true).unwrap();
        assert_eq!(res.tag_count, 1);
        assert_eq!(res.entry_count, 1);
        assert_eq!(target.list_tags().unwrap().len(), 1);
        assert_eq!(target.list_tags().unwrap()[0].name, "t");

        let _ = std::fs::remove_dir_all(&store_dir);
        let _ = std::fs::remove_dir_all(&src_dir);
        let _ = std::fs::remove_dir_all(&target_dir);
    }

    #[test]
    fn test_import_missing_files() {
        let store_dir = temp_dir("miss_store");
        let empty_dir = temp_dir("miss_empty");
        std::fs::create_dir_all(&empty_dir).unwrap();
        let store = JsonlStore::new(&store_dir).unwrap();

        let err = handle_import(&store, Some(empty_dir.to_string_lossy().into()), false);
        assert!(err.is_err());
        assert!(err.unwrap_err().to_string().contains("No tags.jsonl"));

        let _ = std::fs::remove_dir_all(&store_dir);
        let _ = std::fs::remove_dir_all(&empty_dir);
    }

    #[test]
    fn test_import_entries_only() {
        let src_dir = temp_dir("eo_src");
        let store_dir = temp_dir("eo_store");
        std::fs::create_dir_all(&src_dir).unwrap();
        // entries.jsonl without tags.jsonl; unknown tag ids are skipped
        let unknown = TagId::new();
        let entry = Entry::new("https://only.entries", None, None, "desc", vec![unknown]);
        save_jsonl(&entries_path(&src_dir), std::slice::from_ref(&entry)).unwrap();

        let store = JsonlStore::new(&store_dir).unwrap();
        let res = handle_import(&store, Some(src_dir.to_string_lossy().into()), false).unwrap();
        assert_eq!(res.tag_count, 0);
        assert_eq!(res.entry_count, 1);
        let entries = store.list_entries().unwrap();
        assert_eq!(entries.len(), 1);
        assert!(entries[0].tag_ids.is_empty());

        let _ = std::fs::remove_dir_all(&src_dir);
        let _ = std::fs::remove_dir_all(&store_dir);
    }

    #[test]
    fn test_export_preserves_entry_order() {
        let store_dir = temp_dir("ord_store");
        let out_dir = temp_dir("ord_out");
        let store = JsonlStore::new(&store_dir).unwrap();
        let ids: Vec<EntryId> = (0..5)
            .map(|i| {
                store
                    .create_entry(Entry::new(
                        format!("https://example.com/{i}"),
                        None,
                        None,
                        format!("e{i}"),
                        vec![],
                    ))
                    .unwrap()
                    .id
            })
            .collect();

        handle_export(&store, Some(out_dir.to_string_lossy().into())).unwrap();
        let exported: Vec<Entry> = load_jsonl(&entries_path(&out_dir)).unwrap();
        let exported_ids: Vec<EntryId> = exported.iter().map(|e| e.id).collect();
        assert_eq!(exported_ids, ids);

        let _ = std::fs::remove_dir_all(&store_dir);
        let _ = std::fs::remove_dir_all(&out_dir);
    }
}
