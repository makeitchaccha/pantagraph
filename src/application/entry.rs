use std::collections::HashMap;

use anyhow::{Context, Result};

use crate::archiver::ToolConfig;
use crate::domain::{Entry, EntryId, TagId};
use crate::store::TomlStore;

use super::tag::resolve_tag;

#[derive(Debug)]
pub enum EntryResult {
    Added {
        entry: Entry,
        names: HashMap<TagId, String>,
        archiver: Option<String>,
    },
    Listed {
        entries: Vec<Entry>,
        names: HashMap<TagId, String>,
    },
    Show {
        entry: Entry,
        names: HashMap<TagId, String>,
    },
    Tagged {
        entry_id: EntryId,
        tag_name: String,
    },
}

/// Build a TagId → display name lookup from all tags.
fn tag_name_map(store: &TomlStore) -> Result<HashMap<TagId, String>> {
    Ok(store
        .list_tags()?
        .into_iter()
        .map(|t| (t.id, t.display_name()))
        .collect())
}

pub fn handle_entry_add(
    store: &TomlStore,
    archiver: String,
    url: String,
    description: String,
    tag_refs: Vec<String>,
) -> Result<EntryResult> {
    let mut tag_ids = Vec::new();
    for tag_ref in &tag_refs {
        let tag = resolve_tag(store, tag_ref)?;
        tag_ids.push(tag.id);
    }

    let entry_id = EntryId::new();
    let (archiver_opt, archive_path_opt) = if archiver.trim().is_empty() {
        (None, None)
    } else {
        let archiver_name = archiver.trim();
        let tool_config = ToolConfig::load_from_dir(store.data_dir())?;
        let outdir = store.data_dir().join("archives").join(entry_id.to_string());
        std::fs::create_dir_all(&outdir)
            .with_context(|| format!("Failed to create directory {}", outdir.display()))?;
        tool_config.execute_archiver(archiver_name, &url, &outdir)?;
        let relative_path = format!("archives/{entry_id}");
        (Some(archiver_name.to_string()), Some(relative_path))
    };

    let mut entry = Entry::new(
        &url,
        archiver_opt.clone(),
        archive_path_opt,
        &description,
        tag_ids,
    );
    entry.id = entry_id;
    let entry = store.create_entry(entry)?;
    let names = tag_name_map(store)?;
    Ok(EntryResult::Added {
        entry,
        names,
        archiver: archiver_opt,
    })
}

pub fn handle_entry_list(
    store: &TomlStore,
    archiver: Option<String>,
    tag_refs: Vec<String>,
) -> Result<EntryResult> {
    let entries: Vec<Entry> = {
        let mut entries = store.list_entries()?;
        if let Some(target_archiver) = archiver {
            if target_archiver.is_empty() {
                entries.retain(|e| e.archiver.is_none());
            } else {
                entries.retain(|e| e.archiver.as_deref() == Some(&target_archiver));
            }
        }
        if !tag_refs.is_empty() {
            let mut tag_ids = Vec::new();
            for tag_ref in &tag_refs {
                let tag = resolve_tag(store, tag_ref)?;
                tag_ids.push(tag.id);
            }
            // `entry list -t a -t b` means entries must have both tags.
            entries.retain(|e| tag_ids.iter().all(|tid| e.tag_ids.contains(tid)));
        }
        entries
    };

    let names = tag_name_map(store)?;
    Ok(EntryResult::Listed { entries, names })
}

pub fn handle_entry_show(store: &TomlStore, id: String) -> Result<EntryResult> {
    let entry_id: EntryId = id.parse().context("Invalid entry ID")?;
    let entry = store
        .find_entry_by_id(&entry_id)?
        .context("Entry not found")?;
    let names = tag_name_map(store)?;

    Ok(EntryResult::Show { entry, names })
}

pub fn handle_entry_tag(
    store: &TomlStore,
    entry_id: String,
    tag_ref: String,
) -> Result<EntryResult> {
    let eid: EntryId = entry_id.parse().context("Invalid entry ID")?;
    let tag = resolve_tag(store, &tag_ref)?;
    store.find_entry_by_id(&eid)?.context("Entry not found")?;
    store.add_tag_to_entry(&eid, &tag.id)?;
    Ok(EntryResult::Tagged {
        entry_id: eid,
        tag_name: tag.name,
    })
}
