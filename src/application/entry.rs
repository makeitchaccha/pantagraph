use std::collections::HashMap;

use anyhow::{Context, Result};

use crate::archiver::ToolConfig;
use crate::domain::{Entry, EntryId, TagId};
use crate::query::{self, Query, ResolvedQuery};
use crate::store::JsonlStore;

use super::tag::{resolve_query, resolve_tag};

/// Number of entries fetched per search page.
pub const SEARCH_PAGE_SIZE: usize = 10;

/// One page of search results.
#[derive(Debug)]
pub struct EntryPage {
    pub entries: Vec<Entry>,
    pub names: HashMap<TagId, String>,
    /// Whether more results exist after this page.
    pub has_more: bool,
}

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
fn tag_name_map(store: &JsonlStore) -> Result<HashMap<TagId, String>> {
    Ok(store
        .list_tags()?
        .into_iter()
        .map(|t| (t.id, t.display_name()))
        .collect())
}

pub fn handle_entry_add(
    store: &JsonlStore,
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
    store: &JsonlStore,
    archiver: Option<String>,
    tag_refs: Vec<String>,
    query: Option<String>,
    limit: Option<usize>,
    offset: Option<usize>,
    after: Option<String>,
) -> Result<EntryResult> {
    let archiver_filter = archiver
        .as_deref()
        .map(|name| if name.is_empty() { None } else { Some(name) });

    let mut entries = match query {
        None => {
            let mut tag_ids = Vec::new();
            for tag_ref in &tag_refs {
                let tag = resolve_tag(store, tag_ref)?;
                tag_ids.push(tag.id);
            }
            // `entry list -t a -t b` means entries must have both tags.
            store.find_entries(&tag_ids, archiver_filter)?
        }
        Some(query_str) => {
            let parsed = query::parse(&query_str)?;
            let resolved = resolve_query(store, &parsed)?;
            // `-t` tags are additional ANDed constraints alongside the query.
            let mut clauses: Vec<ResolvedQuery> = Vec::with_capacity(tag_refs.len() + 1);
            for tag_ref in &tag_refs {
                clauses.push(Query::Tag(resolve_tag(store, tag_ref)?.id));
            }
            clauses.push(resolved);
            let combined = if clauses.len() == 1 {
                clauses.remove(0)
            } else {
                Query::And(clauses)
            };
            store.find_entries_by_query(&combined, archiver_filter)?
        }
    };

    if let Some(after_id) = after {
        if let Ok(entry_id) = after_id.parse::<EntryId>() {
            if let Some(pos) = entries.iter().position(|e| e.id == entry_id) {
                entries.drain(..=pos);
            }
        }
    }

    let start = offset.unwrap_or(0);
    if start > entries.len() {
        entries.clear();
    } else {
        entries = entries.drain(start..).collect();
    }
    if let Some(limit) = limit {
        if entries.len() > limit {
            entries.truncate(limit);
        }
    }

    let names = tag_name_map(store)?;
    Ok(EntryResult::Listed { entries, names })
}

/// Execute `query` and fetch one zero-based page of results.
///
/// A blank query matches all entries, mirroring `entry list` without
/// `--query`. One extra row is fetched to report whether a further page
/// exists, then the page is truncated to [`SEARCH_PAGE_SIZE`].
pub fn handle_entry_search_page(store: &JsonlStore, query: &str, page: usize) -> Result<EntryPage> {
    let trimmed = query.trim();
    let resolved = if trimmed.is_empty() {
        Query::And(Vec::new())
    } else {
        let parsed = query::parse(trimmed)?;
        resolve_query(store, &parsed)?
    };

    let offset = page * SEARCH_PAGE_SIZE;
    let mut rows = store.find_entries_by_query_page(&resolved, SEARCH_PAGE_SIZE + 1, offset)?;
    let has_more = rows.len() > SEARCH_PAGE_SIZE;
    rows.truncate(SEARCH_PAGE_SIZE);

    let names = tag_name_map(store)?;
    Ok(EntryPage {
        entries: rows,
        names,
        has_more,
    })
}

pub fn handle_entry_show(store: &JsonlStore, id: String) -> Result<EntryResult> {
    let entry_id: EntryId = id.parse().context("Invalid entry ID")?;
    let entry = store
        .find_entry_by_id(&entry_id)?
        .context("Entry not found")?;
    let names = tag_name_map(store)?;

    Ok(EntryResult::Show { entry, names })
}

pub fn handle_entry_tag(
    store: &JsonlStore,
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
