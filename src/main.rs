use std::collections::HashMap;

mod archiver;
mod domain;
mod store;

use anyhow::{bail, Context, Result};
use clap::{Parser, Subcommand};
use domain::{Entry, EntryId, Tag, TagId};
use store::TomlStore;
use archiver::ToolConfig;

#[derive(Parser)]
#[command(name = "pantagraph", about = "Index the web through human-assigned tags")]
struct Cli {
    /// Path to data directory
    #[arg(long, default_value = "data", global = true)]
    data_dir: String,

    #[command(subcommand)]
    command: Commands,
}

#[derive(Subcommand)]
enum Commands {
    /// Manage tags
    Tag {
        #[command(subcommand)]
        command: TagCommands,
    },
    /// Manage entries
    Entry {
        #[command(subcommand)]
        command: EntryCommands,
    },
}

#[derive(Subcommand)]
enum TagCommands {
    /// Create a new tag
    Create {
        /// Tag name
        name: String,
        /// Tag description
        #[arg(long, short, default_value = "")]
        description: String,
        /// Tag aliases (repeatable)
        #[arg(long = "alias", short = 'a')]
        alias: Vec<String>,
    },
    /// List all tags
    List {
        /// Filter by name or alias (substring match)
        #[arg(long)]
        name: Option<String>,
    },
    /// Show tag details
    Show {
        /// Tag name, alias, or ID
        name_or_id: String,
    },
    /// Rename a tag
    Rename {
        /// Tag name, alias, or ID
        name_or_id: String,
        /// New tag name
        new_name: String,
    },
    /// Manage tag aliases
    Alias {
        #[command(subcommand)]
        command: TagAliasCommands,
    },
    /// Add an alias to a tag
    #[command(name = "add-alias")]
    AddAlias {
        /// Tag name, alias, or ID
        name_or_id: String,
        /// Alias to add
        alias: String,
    },
    /// Remove an alias from a tag
    #[command(name = "remove-alias")]
    RemoveAlias {
        /// Tag name, alias, or ID
        name_or_id: String,
        /// Alias to remove
        alias: String,
    },
}

#[derive(Subcommand)]
enum TagAliasCommands {
    /// Add an alias to a tag
    Add {
        /// Tag name, alias, or ID
        name_or_id: String,
        /// Alias to add
        alias: String,
    },
    /// Remove an alias from a tag
    Remove {
        /// Tag name, alias, or ID
        name_or_id: String,
        /// Alias to remove
        alias: String,
    },
    /// List aliases of a tag
    List {
        /// Tag name, alias, or ID
        name_or_id: String,
    },
}

#[derive(Subcommand)]
enum EntryCommands {
    /// Add a new entry (URL with tags, optionally archived)
    Add {
        /// Archiver tool to use (e.g. single-file, default: none)
        #[arg(long, default_value = "")]
        archiver: String,
        /// URL to index
        url: String,
        /// Description of this entry
        #[arg(long, short, default_value = "")]
        description: String,
        /// Tags to associate (name or ID, repeatable)
        #[arg(long, short)]
        tag: Vec<String>,
    },
    /// List entries, optionally filtered by tag(s) or archiver
    List {
        /// Filter by archiver name
        #[arg(long)]
        archiver: Option<String>,
        /// Filter by tag (name or ID, repeatable — entries must match ALL)
        #[arg(long, short)]
        tag: Vec<String>,
    },
    /// Show entry details
    Show {
        /// Entry ID
        id: String,
    },
    /// Add a tag to an existing entry
    Tag {
        /// Entry ID
        entry_id: String,
        /// Tag name or ID to add
        tag: String,
    },
}

// ── Helpers ─────────────────────────────────────────────────────────

/// Parse `"name#N"` into `("name", Some(N))`. Plain `"name"` returns `("name", None)`.
fn parse_disambiguation(input: &str) -> (&str, Option<usize>) {
    if let Some((name, suffix)) = input.rsplit_once('#') {
        if let Ok(n) = suffix.parse::<usize>() {
            if n >= 1 {
                return (name, Some(n));
            }
        }
    }
    (input, None)
}

/// Resolve a tag reference to a single Tag.
///
/// Accepted formats:
/// - UUID string (exact ID lookup)
/// - Tag name (exact match; must be unique)
/// - `name#N` (disambiguation: pick the Nth candidate, 1-indexed, sorted by UUID)
fn resolve_tag(store: &TomlStore, name_or_id: &str) -> Result<Tag> {
    // Try as UUID first
    if let Ok(tag_id) = name_or_id.parse::<TagId>() {
        if let Some(tag) = store.find_tag_by_id(&tag_id)? {
            return Ok(tag);
        }
    }

    // Try name#N disambiguation syntax
    let (name, pick) = parse_disambiguation(name_or_id);

    let mut tags = store.find_tags_by_name_or_alias(name)?;
    tags.sort_by_key(|t| t.id.0);

    match (tags.len(), pick) {
        (0, _) => bail!("Tag not found: {name}"),
        (1, _) => Ok(tags.into_iter().next().unwrap()),
        (_, Some(n)) => {
            let count = tags.len();
            tags.into_iter()
                .nth(n - 1)
                .with_context(|| format!("{name}#{n} is out of range (only {count} candidates)"))
        }
        (_, None) => {
            eprintln!("Multiple tags with name or alias '{name}':");
            for (i, tag) in tags.iter().enumerate() {
                let alias_info = if tag.name != name && tag.aliases.iter().any(|a| a == name) {
                    format!(" [{}]", tag.name)
                } else {
                    String::new()
                };
                if tag.description.is_empty() {
                    eprintln!("  {name}#{}{alias_info}", i + 1);
                } else {
                    eprintln!("  {name}#{}{alias_info} {}", i + 1, tag.description);
                }
            }
            bail!("Ambiguous tag name. Use {name}#N to disambiguate.");
        }
    }
}

/// Build a TagId → name lookup from all tags.
fn tag_name_map(store: &TomlStore) -> Result<HashMap<TagId, String>> {
    Ok(store
        .list_tags()?
        .into_iter()
        .map(|t| (t.id, t.name))
        .collect())
}

// ── Display ─────────────────────────────────────────────────────────

fn print_tag_short(tag: &Tag) {
    if tag.description.is_empty() {
        println!("{}", tag.name);
    } else {
        println!("{} — {}", tag.name, tag.description);
    }
    println!("  id: {}", tag.id);
    if !tag.aliases.is_empty() {
        println!("  aliases: {}", tag.aliases.join(", "));
    }
}

fn print_entry_short(entry: &Entry, names: &HashMap<TagId, String>) {
    let tag_labels: Vec<&str> = entry
        .tag_ids
        .iter()
        .map(|id| names.get(id).map(|s| s.as_str()).unwrap_or("???"))
        .collect();
    let tags_str = if tag_labels.is_empty() {
        String::from("(no tags)")
    } else {
        tag_labels.join(", ")
    };
    let archiver_badge = match &entry.archiver {
        Some(a) if !a.is_empty() => format!("[{a}] "),
        _ => String::new(),
    };
    println!("{}{entry_url}  [{tags_str}]", archiver_badge, entry_url = entry.url);
    if let Some(archive_path) = &entry.archive_path {
        println!("  archive: {}", archive_path);
    }
    if !entry.description.is_empty() {
        println!("  {}", entry.description);
    }
    println!("  id: {}", entry.id);
}

// ── Subcommand handlers ─────────────────────────────────────────────

fn handle_tag_create(
    store: &TomlStore,
    name: String,
    description: String,
    aliases: Vec<String>,
) -> Result<()> {
    let name = name.trim();
    if name.is_empty() {
        bail!("Tag name cannot be empty");
    }
    let mut clean_aliases = Vec::new();
    for a in aliases {
        let a = a.trim();
        if a.is_empty() {
            continue;
        }
        if a == name {
            bail!("Alias cannot be identical to the tag name: '{a}'");
        }
        if !clean_aliases.contains(&a.to_string()) {
            clean_aliases.push(a.to_string());
        }
    }
    let tag = Tag::with_aliases(name, description, clean_aliases);
    let tag = store.create_tag(tag)?;
    println!("Created tag:");
    print_tag_short(&tag);
    Ok(())
}

fn handle_tag_list(store: &TomlStore, name_filter: Option<String>) -> Result<()> {
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

    if tags.is_empty() {
        println!("No tags found.");
    } else {
        for tag in &tags {
            print_tag_short(tag);
        }
    }
    Ok(())
}

fn handle_tag_show(store: &TomlStore, name_or_id: String) -> Result<()> {
    let tag = resolve_tag(store, &name_or_id)?;
    println!("Tag:         {}", tag.name);
    println!("ID:          {}", tag.id);
    println!(
        "Aliases:     {}",
        if tag.aliases.is_empty() {
            "(none)".to_string()
        } else {
            tag.aliases.join(", ")
        }
    );
    println!(
        "Description: {}",
        if tag.description.is_empty() {
            "(none)"
        } else {
            &tag.description
        }
    );

    // Show entries with this tag
    let entries = store.find_entries_by_tag(&tag.id)?;
    if !entries.is_empty() {
        println!("\nEntries ({}):", entries.len());
        for entry in &entries {
            let archiver_badge = match &entry.archiver {
                Some(a) if !a.is_empty() => format!("[{a}] "),
                _ => String::new(),
            };
            println!("  {}{}", archiver_badge, entry.url);
        }
    }
    Ok(())
}

fn handle_tag_rename(store: &TomlStore, name_or_id: String, new_name: String) -> Result<()> {
    let new_name = new_name.trim();
    if new_name.is_empty() {
        bail!("Tag name cannot be empty");
    }
    let tag = resolve_tag(store, &name_or_id)?;
    let old_name = tag.name.clone();
    let updated = store.rename_tag(&tag.id, new_name)?;
    println!("Renamed tag '{}' to '{}':", old_name, updated.name);
    print_tag_short(&updated);
    Ok(())
}

fn handle_tag_alias_add(store: &TomlStore, name_or_id: String, alias: String) -> Result<()> {
    let tag = resolve_tag(store, &name_or_id)?;
    let updated = store.add_tag_alias(&tag.id, &alias)?;
    println!("Added alias '{}' to tag '{}':", alias.trim(), updated.name);
    print_tag_short(&updated);
    Ok(())
}

fn handle_tag_alias_remove(store: &TomlStore, name_or_id: String, alias: String) -> Result<()> {
    let tag = resolve_tag(store, &name_or_id)?;
    let updated = store.remove_tag_alias(&tag.id, &alias)?;
    println!(
        "Removed alias '{}' from tag '{}':",
        alias.trim(),
        updated.name
    );
    print_tag_short(&updated);
    Ok(())
}

fn handle_tag_alias_list(store: &TomlStore, name_or_id: String) -> Result<()> {
    let tag = resolve_tag(store, &name_or_id)?;
    if tag.aliases.is_empty() {
        println!("No aliases for tag '{}'.", tag.name);
    } else {
        println!("Aliases for tag '{}':", tag.name);
        for alias in &tag.aliases {
            println!("  {alias}");
        }
    }
    Ok(())
}

fn handle_entry_add(
    store: &TomlStore,
    archiver: String,
    url: String,
    description: String,
    tag_refs: Vec<String>,
) -> Result<()> {
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

        // Entry-specific archive directory: <data_dir>/archives/<entry_id>/
        let outdir = store.data_dir().join("archives").join(entry_id.to_string());
        std::fs::create_dir_all(&outdir)
            .with_context(|| format!("Failed to create directory {}", outdir.display()))?;

        println!("Archiving with '{}'...", archiver_name);
        tool_config.execute_archiver(archiver_name, &url, &outdir)?;

        let relative_path = format!("archives/{entry_id}");
        (Some(archiver_name.to_string()), Some(relative_path))
    };

    let mut entry = Entry::new(
        &url,
        archiver_opt,
        archive_path_opt,
        &description,
        tag_ids,
    );
    entry.id = entry_id;

    let entry = store.create_entry(entry)?;
    let names = tag_name_map(store)?;
    println!("Created entry:");
    print_entry_short(&entry, &names);
    Ok(())
}

fn handle_entry_list(
    store: &TomlStore,
    archiver: Option<String>,
    tag_refs: Vec<String>,
) -> Result<()> {
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
            entries.retain(|e| tag_ids.iter().all(|tid| e.tag_ids.contains(tid)));
        }
        entries
    };

    let names = tag_name_map(store)?;
    if entries.is_empty() {
        println!("No entries found.");
    } else {
        for entry in &entries {
            print_entry_short(entry, &names);
        }
    }
    Ok(())
}

fn handle_entry_show(store: &TomlStore, id: String) -> Result<()> {
    let entry_id: EntryId = id.parse().context("Invalid entry ID")?;
    let entry = store
        .find_entry_by_id(&entry_id)?
        .context("Entry not found")?;
    let names = tag_name_map(store)?;

    println!("URL:         {}", entry.url);
    println!("ID:          {}", entry.id);
    if let Some(ref archiver) = entry.archiver {
        println!("Archiver:    {}", archiver);
    }
    if let Some(ref archive_path) = entry.archive_path {
        println!("Archive:     {}", archive_path);
    }
    println!(
        "Description: {}",
        if entry.description.is_empty() {
            "(none)"
        } else {
            &entry.description
        }
    );

    let tag_labels: Vec<String> = entry
        .tag_ids
        .iter()
        .map(|id| {
            names
                .get(id)
                .cloned()
                .unwrap_or_else(|| format!("(unknown: {})", id))
        })
        .collect();
    println!(
        "Tags:        {}",
        if tag_labels.is_empty() {
            "(none)".to_string()
        } else {
            tag_labels.join(", ")
        }
    );
    Ok(())
}

fn handle_entry_tag(store: &TomlStore, entry_id: String, tag_ref: String) -> Result<()> {
    let eid: EntryId = entry_id.parse().context("Invalid entry ID")?;
    let tag = resolve_tag(store, &tag_ref)?;

    // Verify entry exists
    store
        .find_entry_by_id(&eid)?
        .context("Entry not found")?;

    store.add_tag_to_entry(&eid, &tag.id)?;
    println!("Added tag '{}' to entry {}", tag.name, eid);
    Ok(())
}

// ── Main ────────────────────────────────────────────────────────────

fn run() -> Result<()> {
    let cli = Cli::parse();
    let store = TomlStore::new(&cli.data_dir)?;

    match cli.command {
        Commands::Tag { command } => match command {
            TagCommands::Create {
                name,
                description,
                alias,
            } => handle_tag_create(&store, name, description, alias),
            TagCommands::List { name } => handle_tag_list(&store, name),
            TagCommands::Show { name_or_id } => handle_tag_show(&store, name_or_id),
            TagCommands::Rename {
                name_or_id,
                new_name,
            } => handle_tag_rename(&store, name_or_id, new_name),
            TagCommands::Alias { command } => match command {
                TagAliasCommands::Add { name_or_id, alias } => {
                    handle_tag_alias_add(&store, name_or_id, alias)
                }
                TagAliasCommands::Remove { name_or_id, alias } => {
                    handle_tag_alias_remove(&store, name_or_id, alias)
                }
                TagAliasCommands::List { name_or_id } => {
                    handle_tag_alias_list(&store, name_or_id)
                }
            },
            TagCommands::AddAlias { name_or_id, alias } => {
                handle_tag_alias_add(&store, name_or_id, alias)
            }
            TagCommands::RemoveAlias { name_or_id, alias } => {
                handle_tag_alias_remove(&store, name_or_id, alias)
            }
        },
        Commands::Entry { command } => match command {
            EntryCommands::Add {
                archiver,
                url,
                description,
                tag,
            } => handle_entry_add(&store, archiver, url, description, tag),
            EntryCommands::List { archiver, tag } => {
                handle_entry_list(&store, archiver, tag)
            }
            EntryCommands::Show { id } => handle_entry_show(&store, id),
            EntryCommands::Tag { entry_id, tag } => handle_entry_tag(&store, entry_id, tag),
        },
    }
}

fn main() {
    if let Err(e) = run() {
        eprintln!("Error: {e:#}");
        std::process::exit(1);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_parse_disambiguation() {
        assert_eq!(parse_disambiguation("rust"), ("rust", None));
        assert_eq!(parse_disambiguation("rust#1"), ("rust", Some(1)));
        assert_eq!(parse_disambiguation("rust#2"), ("rust", Some(2)));
        assert_eq!(parse_disambiguation("rust#0"), ("rust#0", None));
        assert_eq!(parse_disambiguation("rust#abc"), ("rust#abc", None));
    }

    #[test]
    fn test_handle_tag_rename() {
        let dir = std::env::temp_dir().join(format!("pantagraph_test_main_{}", uuid::Uuid::now_v7()));
        let store = TomlStore::new(&dir).unwrap();
        let tag = store.create_tag(Tag::new("old-tag", "desc")).unwrap();

        // Rename via name
        assert!(handle_tag_rename(&store, "old-tag".to_string(), "new-tag".to_string()).is_ok());
        let updated = store.find_tag_by_id(&tag.id).unwrap().unwrap();
        assert_eq!(updated.name, "new-tag");

        // Rename via UUID
        assert!(handle_tag_rename(&store, tag.id.to_string(), "uuid-renamed".to_string()).is_ok());
        let updated2 = store.find_tag_by_id(&tag.id).unwrap().unwrap();
        assert_eq!(updated2.name, "uuid-renamed");

        // Disallow empty name
        assert!(handle_tag_rename(&store, "uuid-renamed".to_string(), "   ".to_string()).is_err());

        // Error if not found
        assert!(handle_tag_rename(&store, "non-existent".to_string(), "valid".to_string()).is_err());

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn test_handle_tag_rename_disambiguation() {
        let dir = std::env::temp_dir().join(format!("pantagraph_test_disambig_{}", uuid::Uuid::now_v7()));
        let store = TomlStore::new(&dir).unwrap();
        let tag1 = store.create_tag(Tag::new("dupe", "first")).unwrap();
        let tag2 = store.create_tag(Tag::new("dupe", "second")).unwrap();

        // Ambiguous rename should fail without #N
        assert!(handle_tag_rename(&store, "dupe".to_string(), "renamed".to_string()).is_err());

        // Disambiguated rename should succeed
        // Sorted by UUID order
        let mut expected_order = [tag1.id, tag2.id];
        expected_order.sort_by_key(|id| id.0);

        assert!(handle_tag_rename(&store, "dupe#1".to_string(), "renamed-1".to_string()).is_ok());
        let first_updated = store.find_tag_by_id(&expected_order[0]).unwrap().unwrap();
        assert_eq!(first_updated.name, "renamed-1");

        let second_original = store.find_tag_by_id(&expected_order[1]).unwrap().unwrap();
        assert_eq!(second_original.name, "dupe");

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn test_tag_alias_and_entry_integration() {
        let dir = std::env::temp_dir().join(format!("pantagraph_test_alias_int_{}", uuid::Uuid::now_v7()));
        let store = TomlStore::new(&dir).unwrap();

        // 1. Create tag with aliases
        assert!(handle_tag_create(
            &store,
            "rust".to_string(),
            "Rust programming language".to_string(),
            vec!["rs".to_string(), "rust-lang".to_string()]
        ).is_ok());

        // 2. Resolve by alias
        let resolved = resolve_tag(&store, "rs").unwrap();
        assert_eq!(resolved.name, "rust");

        let resolved_alias2 = resolve_tag(&store, "rust-lang").unwrap();
        assert_eq!(resolved_alias2.id, resolved.id);

        // 3. Add entry using alias
        assert!(handle_entry_add(
            &store,
            "".to_string(),
            "https://www.rust-lang.org".to_string(),
            "Rust website".to_string(),
            vec!["rs".to_string()]
        ).is_ok());

        let entries = store.list_entries().unwrap();
        assert_eq!(entries.len(), 1);
        assert_eq!(entries[0].tag_ids, vec![resolved.id]);

        // 4. Filter entries by alias
        assert!(handle_entry_list(&store, None, vec!["rust-lang".to_string()]).is_ok());

        // 5. Add another tag and attach via alias using handle_entry_tag
        assert!(handle_tag_create(
            &store,
            "systems".to_string(),
            "".to_string(),
            vec!["sys".to_string()]
        ).is_ok());

        assert!(handle_entry_tag(&store, entries[0].id.to_string(), "sys".to_string()).is_ok());
        let updated_entry = store.find_entry_by_id(&entries[0].id).unwrap().unwrap();
        assert_eq!(updated_entry.tag_ids.len(), 2);

        // 6. Rename tag referenced by alias
        assert!(handle_tag_rename(&store, "rs".to_string(), "rustacean".to_string()).is_ok());
        let renamed = resolve_tag(&store, "rs").unwrap();
        assert_eq!(renamed.name, "rustacean");

        // 7. Add & remove alias via CLI handler
        assert!(handle_tag_alias_add(&store, "rustacean".to_string(), "crab".to_string()).is_ok());
        let with_crab = resolve_tag(&store, "crab").unwrap();
        assert_eq!(with_crab.name, "rustacean");

        assert!(handle_tag_alias_list(&store, "crab".to_string()).is_ok());

        assert!(handle_tag_alias_remove(&store, "crab".to_string(), "crab".to_string()).is_ok());
        assert!(resolve_tag(&store, "crab").is_err());

        // 8. Tag show works with alias
        assert!(handle_tag_show(&store, "rs".to_string()).is_ok());

        // 9. Tag list filter matches alias
        assert!(handle_tag_list(&store, Some("rust-lan".to_string())).is_ok());

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn test_tag_alias_disambiguation() {
        let dir = std::env::temp_dir().join(format!("pantagraph_test_alias_dis_{}", uuid::Uuid::now_v7()));
        let store = TomlStore::new(&dir).unwrap();

        let tag1 = store.create_tag(Tag::with_aliases("first", "first tag", vec!["common".to_string()])).unwrap();
        let tag2 = store.create_tag(Tag::with_aliases("second", "second tag", vec!["common".to_string()])).unwrap();

        // Ambiguous without #N
        assert!(resolve_tag(&store, "common").is_err());

        let mut expected = [tag1.id, tag2.id];
        expected.sort_by_key(|id| id.0);

        let picked1 = resolve_tag(&store, "common#1").unwrap();
        assert_eq!(picked1.id, expected[0]);

        let picked2 = resolve_tag(&store, "common#2").unwrap();
        assert_eq!(picked2.id, expected[1]);

        let _ = std::fs::remove_dir_all(&dir);
    }
}
