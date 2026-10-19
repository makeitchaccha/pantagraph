use anyhow::{bail, Context, Result};
use std::fmt;

use crate::config::DuplicatePolicy;
use crate::domain::{Tag, TagId};
use crate::query::{Query, ResolvedQuery};
use crate::store::JsonlStore;

#[derive(Debug)]
pub enum TagResult {
    Created {
        tag: Tag,
        conflicts: Vec<Tag>,
    },
    Listed(Vec<Tag>),
    Show {
        tag: Tag,
        entries: Vec<crate::domain::Entry>,
    },
    Renamed {
        old_name: String,
        tag: Tag,
    },
    AliasAdded {
        alias: String,
        tag: Tag,
    },
    AliasRemoved {
        alias: String,
        tag: Tag,
    },
    AliasList(Tag),
}

#[derive(Debug)]
pub struct TagConflictAbort {
    pub label: String,
    pub tags: Vec<Tag>,
}

impl fmt::Display for TagConflictAbort {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "Aborted due to tag conflict for '{}'.", self.label)
    }
}

impl std::error::Error for TagConflictAbort {}

/// Parse a tag reference string.
///
/// Supports `name#N`, `name@specifier#N`, `name@specifier`, and `name`.
pub fn parse_tag_ref(input: &str) -> (&str, Option<&str>, Option<usize>) {
    let (base, pick) = if let Some((base, suffix)) = input.rsplit_once('#') {
        if let Ok(n) = suffix.parse::<usize>() {
            if n >= 1 {
                (base, Some(n))
            } else {
                (input, None)
            }
        } else {
            (input, None)
        }
    } else {
        (input, None)
    };

    if let Some((name, specifier)) = base.split_once('@') {
        (name, Some(specifier), pick)
    } else {
        (base, None, pick)
    }
}

/// Resolve a tag reference to a single tag, preserving the CLI's disambiguation behavior.
pub fn resolve_tag(store: &JsonlStore, name_or_id: &str) -> Result<Tag> {
    if let Ok(tag_id) = name_or_id.parse::<TagId>() {
        if let Some(tag) = store.find_tag_by_id(&tag_id)? {
            return Ok(tag);
        }
    }

    let (name, specifier, pick) = parse_tag_ref(name_or_id);
    let mut tags = if let Some(spec) = specifier {
        store.find_tags_by_name_and_specifier(name, spec)?
    } else {
        store.find_tags_by_name_or_alias(name)?
    };
    tags.sort_by_key(|t| t.id.0);

    match (tags.len(), pick) {
        (0, _) => bail!("Tag not found: {name_or_id}"),
        (1, _) => Ok(tags.into_iter().next().unwrap()),
        (_, Some(n)) => {
            let count = tags.len();
            tags.into_iter()
                .nth(n - 1)
                .with_context(|| format!("{name_or_id} is out of range (only {count} candidates)"))
        }
        (_, None) => {
            let candidates = tags
                .iter()
                .enumerate()
                .map(|(i, tag)| {
                    let alias_info = if tag.name != name && tag.aliases.iter().any(|a| a == name) {
                        format!(" [{}]", tag.name)
                    } else {
                        String::new()
                    };
                    let spec_info = tag
                        .specifier
                        .as_ref()
                        .map(|s| format!(" (@{s})"))
                        .unwrap_or_default();
                    if tag.description.is_empty() {
                        format!("  {name}#{}{spec_info}{alias_info}", i + 1)
                    } else {
                        format!(
                            "  {name}#{}{spec_info}{alias_info} {}",
                            i + 1,
                            tag.description
                        )
                    }
                })
                .collect::<Vec<_>>()
                .join("\n");
            bail!("Multiple tags match '{name_or_id}':\n{candidates}\nAmbiguous tag name. Use {name_or_id}#N (or specify @specifier) to disambiguate.");
        }
    }
}

/// Resolve every tag reference in a parsed query to a [`TagId`].
///
/// Resolution uses the same rules as `-t` references, including `#N`
/// and `@specifier` disambiguation.
pub fn resolve_query(store: &JsonlStore, query: &Query) -> Result<ResolvedQuery> {
    let resolved = match query {
        Query::Tag(tag_ref) => Query::Tag(resolve_tag(store, tag_ref)?.id),
        Query::And(queries) => Query::And(
            queries
                .iter()
                .map(|q| resolve_query(store, q))
                .collect::<Result<Vec<_>>>()?,
        ),
        Query::Or(queries) => Query::Or(
            queries
                .iter()
                .map(|q| resolve_query(store, q))
                .collect::<Result<Vec<_>>>()?,
        ),
        Query::Not(inner) => Query::Not(Box::new(resolve_query(store, inner)?)),
    };
    Ok(resolved)
}

pub fn handle_tag_create(
    store: &JsonlStore,
    name: String,
    specifier: Option<String>,
    description: String,
    aliases: Vec<String>,
    same_name_policy: DuplicatePolicy,
    same_qualified_ref_policy: DuplicatePolicy,
) -> Result<TagResult> {
    handle_tag_create_inner(
        store,
        name,
        specifier,
        description,
        aliases,
        same_name_policy,
        same_qualified_ref_policy,
    )
}

fn handle_tag_create_inner(
    store: &JsonlStore,
    name: String,
    mut specifier: Option<String>,
    description: String,
    aliases: Vec<String>,
    same_name_policy: DuplicatePolicy,
    same_qualified_ref_policy: DuplicatePolicy,
) -> Result<TagResult> {
    let name_trimmed = name.trim();
    if name_trimmed.is_empty() {
        bail!("Tag name cannot be empty");
    }

    let (tag_name, parsed_spec) = if let Some((n, s)) = name_trimmed.split_once('@') {
        (n.trim(), Some(s.trim()))
    } else {
        (name_trimmed, None)
    };
    if tag_name.is_empty() {
        bail!("Tag name cannot be empty");
    }
    if let (None, Some(s)) = (&specifier, parsed_spec) {
        if !s.is_empty() {
            specifier = Some(s.to_string());
        }
    }
    let clean_specifier = specifier
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty());

    let mut clean_aliases = Vec::new();
    for a in aliases {
        let a = a.trim();
        if a.is_empty() {
            continue;
        }
        if a == tag_name {
            bail!("Alias cannot be identical to the tag name: '{a}'");
        }
        if !clean_aliases.contains(&a.to_string()) {
            clean_aliases.push(a.to_string());
        }
    }

    let qualified = clean_specifier.as_ref().map(|s| format!("{tag_name}@{s}"));
    let duplicate_policy = if let Some(ref spec) = clean_specifier {
        if store
            .find_tags_by_name_and_specifier(tag_name, spec)?
            .is_empty()
        {
            same_name_policy
        } else {
            same_qualified_ref_policy
        }
    } else {
        let existing = store.find_tags_by_name_or_alias(tag_name)?;
        if existing.iter().any(|tag| tag.specifier.is_some()) {
            same_qualified_ref_policy
        } else {
            same_name_policy
        }
    };
    let mut conflicts = Vec::new();
    let conflicting_tags = if let Some(ref spec) = clean_specifier {
        store.find_tags_by_name_and_specifier(tag_name, spec)?
    } else {
        store.find_tags_by_name_or_alias(tag_name)?
    };
    if !conflicting_tags.is_empty() && duplicate_policy != DuplicatePolicy::Allow {
        if duplicate_policy == DuplicatePolicy::Error {
            return Err(TagConflictAbort {
                label: qualified.unwrap_or_else(|| tag_name.to_string()),
                tags: conflicting_tags,
            }
            .into());
        }
        conflicts = conflicting_tags;
    }

    let tag = Tag::with_specifier(tag_name, clean_specifier, description, clean_aliases);
    let tag = store.create_tag(tag)?;
    Ok(TagResult::Created { tag, conflicts })
}

pub fn handle_tag_list(store: &JsonlStore, name_filter: Option<String>) -> Result<TagResult> {
    let tags: Vec<Tag> = if let Some(ref filter) = name_filter {
        store
            .list_tags()?
            .into_iter()
            .filter(|t| {
                t.name.contains(filter.as_str())
                    || t.aliases.iter().any(|a| a.contains(filter.as_str()))
            })
            .collect()
    } else {
        store.list_tags()?
    };
    Ok(TagResult::Listed(tags))
}

pub fn handle_tag_show(store: &JsonlStore, name_or_id: String) -> Result<TagResult> {
    let tag = resolve_tag(store, &name_or_id)?;
    let entries = store.find_entries_by_tag(&tag.id)?;
    Ok(TagResult::Show { tag, entries })
}

pub fn handle_tag_rename(
    store: &JsonlStore,
    name_or_id: String,
    new_name: String,
) -> Result<TagResult> {
    let new_name = new_name.trim();
    if new_name.is_empty() {
        bail!("Tag name cannot be empty");
    }
    let tag = resolve_tag(store, &name_or_id)?;
    let old_name = tag.name.clone();
    let updated = store.rename_tag(&tag.id, new_name)?;
    Ok(TagResult::Renamed {
        old_name,
        tag: updated,
    })
}

pub fn handle_tag_alias_add(
    store: &JsonlStore,
    name_or_id: String,
    alias: String,
) -> Result<TagResult> {
    let tag = resolve_tag(store, &name_or_id)?;
    let updated = store.add_tag_alias(&tag.id, &alias)?;
    Ok(TagResult::AliasAdded {
        alias: alias.trim().to_string(),
        tag: updated,
    })
}

pub fn handle_tag_alias_remove(
    store: &JsonlStore,
    name_or_id: String,
    alias: String,
) -> Result<TagResult> {
    let tag = resolve_tag(store, &name_or_id)?;
    let updated = store.remove_tag_alias(&tag.id, &alias)?;
    Ok(TagResult::AliasRemoved {
        alias: alias.trim().to_string(),
        tag: updated,
    })
}

pub fn handle_tag_alias_list(store: &JsonlStore, name_or_id: String) -> Result<TagResult> {
    let tag = resolve_tag(store, &name_or_id)?;
    Ok(TagResult::AliasList(tag))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_parse_tag_ref() {
        assert_eq!(parse_tag_ref("rust"), ("rust", None, None));
        assert_eq!(parse_tag_ref("rust#1"), ("rust", None, Some(1)));
        assert_eq!(parse_tag_ref("rust#2"), ("rust", None, Some(2)));
        assert_eq!(parse_tag_ref("rust#0"), ("rust#0", None, None));
        assert_eq!(parse_tag_ref("rust#abc"), ("rust#abc", None, None));
        assert_eq!(parse_tag_ref("rust@lang"), ("rust", Some("lang"), None));
        assert_eq!(
            parse_tag_ref("rust@game#1"),
            ("rust", Some("game"), Some(1))
        );
    }
}
