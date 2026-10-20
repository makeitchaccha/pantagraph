use std::collections::HashMap;

use anyhow::{Context, Result};

use crate::archiver::ToolConfig;
use crate::domain::{Entry, EntryId, TagId};
use crate::query::{self, Query, ResolvedQuery};
use crate::store::Store;

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
    MetaSet {
        entry_id: EntryId,
        key: String,
        value: String,
    },
    MetaRemoved {
        entry_id: EntryId,
        key: String,
        removed: bool,
    },
    MetaListed {
        entry_id: EntryId,
        meta: std::collections::BTreeMap<String, String>,
    },
}

/// Build a TagId → display name lookup from all tags.
fn tag_name_map(store: &Store) -> Result<HashMap<TagId, String>> {
    Ok(store
        .list_tags()?
        .into_iter()
        .map(|t| (t.id, t.display_name()))
        .collect())
}

pub fn handle_entry_add(
    store: &Store,
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
    let archiver_opt = if archiver.trim().is_empty() {
        None
    } else {
        let archiver_name = archiver.trim();
        let tool_config = ToolConfig::load_from_dir(store.data_dir())?;
        let outdir = store.data_dir().join("archives").join(entry_id.to_string());
        std::fs::create_dir_all(&outdir)
            .with_context(|| format!("Failed to create directory {}", outdir.display()))?;
        tool_config.execute_archiver(archiver_name, &url, &outdir)?;
        Some(archiver_name.to_string())
    };

    let mut entry = Entry::new(&url, &description, tag_ids);
    entry.id = entry_id;
    if let Some(name) = &archiver_opt {
        let relative_path = format!("archives/{entry_id}");
        entry.meta.insert("archived".to_string(), "true".to_string());
        entry.meta.insert("archiver".to_string(), name.clone());
        entry.meta.insert("archive_path".to_string(), relative_path);
    }
    let entry = store.create_entry(entry)?;
    let names = tag_name_map(store)?;
    Ok(EntryResult::Added {
        entry,
        names,
        archiver: archiver_opt,
    })
}

pub fn handle_entry_list(
    store: &Store,
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

    // Resolve `--after` to an offset into the ordinal-ordered result set,
    // when possible: ordinals are contiguous integers assigned at insert
    // time (see `find_entry_ordinal`), so an entry's ordinal is directly
    // usable as a SQL OFFSET for "entries after this one" — but only in
    // the unfiltered case (no `-t` tags, no `--query`), where the result
    // set is the full table and ordinals are dense 0..N-1. With a tag
    // filter or query, the filtered subset has ordinal gaps, so an
    // ordinal can't be reused as a row offset. If the anchor entry doesn't
    // exist, treat `--after` as a no-op (matching prior in-memory
    // behavior, which only filtered when the id was found).
    let unfiltered = tag_refs.is_empty() && query.is_none();
    let mut after_offset = 0usize;
    let mut after_resolved = false;
    if unfiltered {
        if let Some(after_id) = &after {
            if let Ok(entry_id) = after_id.parse::<EntryId>() {
                if let Some(ordinal) = store.find_entry_ordinal(&entry_id)? {
                    after_offset = (ordinal as usize) + 1;
                    after_resolved = true;
                }
            }
        }
    }

    // Push `limit`/`offset` into SQL when `--after` resolved cleanly to an
    // offset above (or is not used at all). The archiver filter and tag /
    // query filters are always pushed into SQL, so a SQL-level LIMIT is
    // always correct here. Only push when there is a `limit`; offset alone
    // doesn't reduce rows fetched from SQL, so it isn't worth the SQL-level
    // pushdown.
    let sql_page = if after.is_none() || after_resolved {
        limit.map(|limit| (limit, offset.unwrap_or(0) + after_offset))
    } else {
        None
    };

    let mut entries = match query {
        None => {
            let mut tag_ids = Vec::new();
            for tag_ref in &tag_refs {
                let tag = resolve_tag(store, tag_ref)?;
                tag_ids.push(tag.id);
            }
            // `entry list -t a -t b` means entries must have both tags.
            store.find_entries_page(&tag_ids, archiver_filter, sql_page)?
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
            store.find_entries_by_query_page_opt(&combined, archiver_filter, sql_page)?
        }
    };

    // If the page was already pushed into SQL (including a resolved
    // `--after` offset), skip re-applying the offset and limit in memory
    // below.
    if sql_page.is_none() {
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
    }

    let names = tag_name_map(store)?;
    Ok(EntryResult::Listed { entries, names })
}

/// Execute `query` and fetch one zero-based page of results.
///
/// A blank query matches all entries, mirroring `entry list` without
/// `--query`. One extra row is fetched to report whether a further page
/// exists, then the page is truncated to [`SEARCH_PAGE_SIZE`].
pub fn handle_entry_search_page(store: &Store, query: &str, page: usize) -> Result<EntryPage> {
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

pub fn handle_entry_show(store: &Store, id: String) -> Result<EntryResult> {
    let entry_id: EntryId = id.parse().context("Invalid entry ID")?;
    let entry = store
        .find_entry_by_id(&entry_id)?
        .context("Entry not found")?;
    let names = tag_name_map(store)?;

    Ok(EntryResult::Show { entry, names })
}

pub fn handle_entry_tag(
    store: &Store,
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

/// Set `key = value` on an entry's meta attributes.
pub fn handle_entry_meta_set(
    store: &Store,
    entry_id: String,
    key: String,
    value: String,
) -> Result<EntryResult> {
    let eid: EntryId = entry_id.parse().context("Invalid entry ID")?;
    store.find_entry_by_id(&eid)?.context("Entry not found")?;
    store.set_entry_meta(&eid, &key, &value)?;
    Ok(EntryResult::MetaSet {
        entry_id: eid,
        key,
        value,
    })
}

/// Remove `key` from an entry's meta attributes.
pub fn handle_entry_meta_remove(
    store: &Store,
    entry_id: String,
    key: String,
) -> Result<EntryResult> {
    let eid: EntryId = entry_id.parse().context("Invalid entry ID")?;
    store.find_entry_by_id(&eid)?.context("Entry not found")?;
    let removed = store.remove_entry_meta(&eid, &key)?;
    Ok(EntryResult::MetaRemoved {
        entry_id: eid,
        key,
        removed,
    })
}

/// List one entry's meta attributes.
pub fn handle_entry_meta_list(
    store: &Store,
    entry_id: String,
) -> Result<EntryResult> {
    let eid: EntryId = entry_id.parse().context("Invalid entry ID")?;
    store.find_entry_by_id(&eid)?.context("Entry not found")?;
    let meta = store.get_entry_meta(&eid)?;
    Ok(EntryResult::MetaListed {
        entry_id: eid,
        meta,
    })
}
