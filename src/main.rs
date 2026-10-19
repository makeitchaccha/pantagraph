mod application;
mod config;
mod tui;

use pantagraph::{archiver, domain, query, store};

use std::collections::HashMap;

use anyhow::Result;
use application::{entry, importexport, tag};
use clap::{Parser, Subcommand};
use config::Config;
use domain::{Entry, Tag, TagId};
use store::JsonlStore;

#[derive(Parser)]
#[command(
    name = "pantagraph",
    about = "Index the web through human-assigned tags"
)]
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
    /// Export tags and entries to JSONL files (the legacy interchange format)
    Export {
        /// Output directory (default: the data directory itself)
        #[arg(long)]
        out: Option<String>,
    },
    /// Import tags and entries from JSONL files (the legacy interchange format)
    Import {
        /// Input directory containing tags.jsonl and entries.jsonl
        #[arg(long)]
        from: Option<String>,
        /// Replace all existing data instead of refusing to touch a non-empty store
        #[arg(long)]
        overwrite: bool,
    },
    /// Browse search results interactively
    Tui,
}

#[derive(Subcommand)]
enum TagCommands {
    /// Create a new tag
    Create {
        /// Tag name
        name: String,
        /// Optional specifier (e.g. name@specifier) to distinguish conflicting tags
        #[arg(long = "specifier", short = 's')]
        specifier: Option<String>,
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
        /// Maximum number of tags to return
        #[arg(long)]
        limit: Option<usize>,
        /// Number of tags to skip
        #[arg(long)]
        offset: Option<usize>,
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
    /// List entries, optionally filtered by tag(s), query, or archiver
    List {
        /// Filter by archiver name
        #[arg(long)]
        archiver: Option<String>,
        /// Filter by tag (name or ID, repeatable — entries must match ALL)
        #[arg(long, short)]
        tag: Vec<String>,
        /// Filter by query expression: space/* = AND, + = OR, ! = NOT (e.g. 'vocaloid KAITO !cover')
        #[arg(long)]
        query: Option<String>,
        /// Maximum number of entries to return
        #[arg(long)]
        limit: Option<usize>,
        /// Number of entries to skip
        #[arg(long)]
        offset: Option<usize>,
        /// Return entries after this ID (exclusive)
        #[arg(long)]
        after: Option<String>,
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
    /// Manage an entry's meta attributes (system-assigned key/value data)
    Meta {
        #[command(subcommand)]
        command: EntryMetaCommands,
    },
}

#[derive(Subcommand)]
enum EntryMetaCommands {
    /// Set a meta key on an entry
    Set {
        /// Entry ID
        entry_id: String,
        /// Meta key (free-form, e.g. `source`)
        key: String,
        /// Meta value
        value: String,
    },
    /// Remove a meta key from an entry
    Remove {
        /// Entry ID
        entry_id: String,
        /// Meta key to remove
        key: String,
    },
    /// List an entry's meta attributes
    List {
        /// Entry ID
        entry_id: String,
    },
}

// ── Main ────────────────────────────────────────────────────────────

fn run() -> Result<()> {
    let cli = Cli::parse();
    let store = JsonlStore::new(&cli.data_dir)?;
    let config = Config::load_from_dir(store.data_dir())?;

    match cli.command {
        Commands::Tag { command } => match command {
            TagCommands::Create {
                name,
                specifier,
                description,
                alias,
            } => tag::handle_tag_create(
                &store,
                name,
                specifier,
                description,
                alias,
                config.tag.same_name_policy,
                config.tag.same_qualified_ref_policy,
            )
            .map(present_tag_result),
            TagCommands::List { name, limit, offset } => {
                tag::handle_tag_list(&store, name, limit, offset).map(present_tag_result)
            }
            TagCommands::Show { name_or_id } => {
                tag::handle_tag_show(&store, name_or_id).map(present_tag_result)
            }
            TagCommands::Rename {
                name_or_id,
                new_name,
            } => tag::handle_tag_rename(&store, name_or_id, new_name).map(present_tag_result),
            TagCommands::Alias { command } => match command {
                TagAliasCommands::Add { name_or_id, alias } => {
                    tag::handle_tag_alias_add(&store, name_or_id, alias).map(present_tag_result)
                }
                TagAliasCommands::Remove { name_or_id, alias } => {
                    tag::handle_tag_alias_remove(&store, name_or_id, alias).map(present_tag_result)
                }
                TagAliasCommands::List { name_or_id } => {
                    tag::handle_tag_alias_list(&store, name_or_id).map(present_tag_result)
                }
            },
            TagCommands::AddAlias { name_or_id, alias } => {
                tag::handle_tag_alias_add(&store, name_or_id, alias).map(present_tag_result)
            }
            TagCommands::RemoveAlias { name_or_id, alias } => {
                tag::handle_tag_alias_remove(&store, name_or_id, alias).map(present_tag_result)
            }
        },
        Commands::Entry { command } => match command {
            EntryCommands::Add {
                archiver,
                url,
                description,
                tag,
            } => entry::handle_entry_add(&store, archiver, url, description, tag)
                .map(present_entry_result),
            EntryCommands::List {
                archiver,
                tag,
                query,
                limit,
                offset,
                after,
            } => entry::handle_entry_list(&store, archiver, tag, query, limit, offset, after)
                .map(present_entry_result),
            EntryCommands::Show { id } => {
                entry::handle_entry_show(&store, id).map(present_entry_result)
            }
            EntryCommands::Tag { entry_id, tag } => {
                entry::handle_entry_tag(&store, entry_id, tag).map(present_entry_result)
            }
            EntryCommands::Meta { command } => match command {
                EntryMetaCommands::Set {
                    entry_id,
                    key,
                    value,
                } => entry::handle_entry_meta_set(&store, entry_id, key, value)
                    .map(present_entry_result),
                EntryMetaCommands::Remove { entry_id, key } => {
                    entry::handle_entry_meta_remove(&store, entry_id, key)
                        .map(present_entry_result)
                }
                EntryMetaCommands::List { entry_id } => {
                    entry::handle_entry_meta_list(&store, entry_id).map(present_entry_result)
                }
            },
        },
        Commands::Export { out } => {
            let result = importexport::handle_export(&store, out)?;
            println!(
                "Exported {} tags and {} entries to {}",
                result.tag_count,
                result.entry_count,
                result.out_dir.display()
            );
            Ok(())
        }
        Commands::Import { from, overwrite } => {
            let result = importexport::handle_import(&store, from, overwrite)?;
            println!(
                "Imported {} tags and {} entries",
                result.tag_count, result.entry_count
            );
            Ok(())
        }
        Commands::Tui => tui::run(&store),
    }
}

fn present_tag_result(result: tag::TagResult) {
    use tag::TagResult::*;
    match result {
        Created { tag, conflicts } => {
            if !conflicts.is_empty() {
                print_conflicts("Warning", &tag.display_name(), &conflicts);
            }
            println!("Created tag:");
            print_tag_short(&tag);
        }
        Listed(tags) => {
            if tags.is_empty() {
                println!("No tags found.");
            } else {
                for tag in &tags {
                    print_tag_short(tag);
                }
            }
        }
        Show { tag, entries } => {
            println!("Tag:         {}", tag.name);
            println!("ID:          {}", tag.id);
            if let Some(spec) = &tag.specifier {
                println!("Specifier:   @{spec}");
            }
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
            if !entries.is_empty() {
                println!("\nEntries ({}):", entries.len());
                for entry in entries {
                    println!(
                        "  {}{}",
                        entry
                            .meta
                            .get("archiver")
                            .map(|a| format!("[{a}] "))
                            .unwrap_or_default(),
                        entry.url
                    );
                }
            }
        }
        Renamed { old_name, tag } => {
            println!("Renamed tag '{}' to '{}':", old_name, tag.name);
            print_tag_short(&tag);
        }
        AliasAdded { alias, tag } => {
            println!("Added alias '{}' to tag '{}':", alias, tag.name);
            print_tag_short(&tag);
        }
        AliasRemoved { alias, tag } => {
            println!("Removed alias '{}' from tag '{}':", alias, tag.name);
            print_tag_short(&tag);
        }
        AliasList(tag) => {
            if tag.aliases.is_empty() {
                println!("No aliases for tag '{}'.", tag.name);
            } else {
                println!("Aliases for tag '{}':", tag.name);
                for alias in tag.aliases {
                    println!("  {alias}");
                }
            }
        }
    }
}

fn present_entry_result(result: entry::EntryResult) {
    use entry::EntryResult::*;
    match result {
        Added {
            entry,
            names,
            archiver,
        } => {
            if let Some(name) = archiver {
                println!("Archiving with '{}'...", name);
            }
            println!("Created entry:");
            print_entry_short(&entry, &names);
        }
        Listed { entries, names } => {
            if entries.is_empty() {
                println!("No entries found.");
            } else {
                for entry in &entries {
                    print_entry_short(entry, &names);
                }
            }
        }
        Show { entry, names } => {
            println!("URL:         {}", entry.url);
            println!("ID:          {}", entry.id);
            for (key, value) in &entry.meta {
                println!("Meta:        {key}={value}");
            }
            println!(
                "Description: {}",
                if entry.description.is_empty() {
                    "(none)"
                } else {
                    &entry.description
                }
            );
            let labels = entry
                .tag_ids
                .iter()
                .map(|id| {
                    names
                        .get(id)
                        .cloned()
                        .unwrap_or_else(|| format!("(unknown: {id})"))
                })
                .collect::<Vec<_>>();
            println!(
                "Tags:        {}",
                if labels.is_empty() {
                    "(none)".to_string()
                } else {
                    labels.join(", ")
                }
            );
        }
        Tagged { entry_id, tag_name } => println!("Added tag '{}' to entry {}", tag_name, entry_id),
        MetaSet {
            entry_id,
            key,
            value,
        } => println!("Set {key}={value} on entry {entry_id}"),
        MetaRemoved {
            entry_id,
            key,
            removed,
        } => {
            if removed {
                println!("Removed {key} from entry {entry_id}");
            } else {
                println!("Entry {entry_id} has no {key} meta key");
            }
        }
        MetaListed { entry_id, meta } => {
            if meta.is_empty() {
                println!("Entry {entry_id} has no meta attributes.");
            } else {
                println!("Meta attributes for entry {entry_id}:");
                for (key, value) in meta {
                    println!("  {key}={value}");
                }
            }
        }
    }
}

fn print_conflicts(prefix: &str, label: &str, tags: &[Tag]) {
    eprintln!("{prefix}: Tag conflict detected for '{label}'.");
    eprintln!("Existing matching tag(s):");
    for tag in tags {
        let aliases = if tag.aliases.is_empty() {
            String::new()
        } else {
            format!(" (aliases: {})", tag.aliases.join(", "))
        };
        let specifier = tag
            .specifier
            .as_ref()
            .map(|s| format!(" (@{s})"))
            .unwrap_or_default();
        eprintln!(
            "  - ID: {} | name: '{}{specifier}{aliases}'",
            tag.id, tag.name
        );
    }
}

fn print_tag_short(tag: &Tag) {
    if tag.description.is_empty() {
        println!("{}", tag.display_name());
    } else {
        println!("{} — {}", tag.display_name(), tag.description);
    }
    println!("  id: {}", tag.id);
    if let Some(specifier) = &tag.specifier {
        println!("  specifier: @{specifier}");
    }
    if !tag.aliases.is_empty() {
        println!("  aliases: {}", tag.aliases.join(", "));
    }
}

fn print_entry_short(entry: &Entry, names: &HashMap<TagId, String>) {
    let tags = entry
        .tag_ids
        .iter()
        .map(|id| names.get(id).map(String::as_str).unwrap_or("???"))
        .collect::<Vec<_>>();
    let tags = if tags.is_empty() {
        "(no tags)".to_string()
    } else {
        tags.join(", ")
    };
    let badge = entry
        .meta
        .get("archiver")
        .map(|a| format!("[{a}] "))
        .unwrap_or_default();
    println!("{badge}{}  [{tags}]", entry.url);
    if let Some(path) = entry.meta.get("archive_path") {
        println!("  archive: {path}");
    }
    if !entry.description.is_empty() {
        println!("  {}", entry.description);
    }
    println!("  id: {}", entry.id);
}

fn main() {
    if let Err(e) = run() {
        if let Some(conflict) = e.downcast_ref::<tag::TagConflictAbort>() {
            print_conflicts("Error", &conflict.label, &conflict.tags);
        }
        eprintln!("Error: {e:#}");
        std::process::exit(1);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::DuplicatePolicy;
    use crate::domain::Tag;

    #[test]
    fn test_parse_tag_ref() {
        assert_eq!(tag::parse_tag_ref("rust"), ("rust", None, None));
        assert_eq!(tag::parse_tag_ref("rust#1"), ("rust", None, Some(1)));
        assert_eq!(tag::parse_tag_ref("rust#2"), ("rust", None, Some(2)));
        assert_eq!(tag::parse_tag_ref("rust#0"), ("rust#0", None, None));
        assert_eq!(tag::parse_tag_ref("rust#abc"), ("rust#abc", None, None));
        assert_eq!(
            tag::parse_tag_ref("rust@lang"),
            ("rust", Some("lang"), None)
        );
        assert_eq!(
            tag::parse_tag_ref("rust@game#1"),
            ("rust", Some("game"), Some(1))
        );
    }

    #[test]
    fn test_handle_tag_rename() {
        let dir =
            std::env::temp_dir().join(format!("pantagraph_test_main_{}", uuid::Uuid::now_v7()));
        let store = JsonlStore::new(&dir).unwrap();
        let tag = store.create_tag(Tag::new("old-tag", "desc")).unwrap();

        // Rename via name
        assert!(
            tag::handle_tag_rename(&store, "old-tag".to_string(), "new-tag".to_string()).is_ok()
        );
        let updated = store.find_tag_by_id(&tag.id).unwrap().unwrap();
        assert_eq!(updated.name, "new-tag");

        // Rename via UUID
        assert!(
            tag::handle_tag_rename(&store, tag.id.to_string(), "uuid-renamed".to_string()).is_ok()
        );
        let updated2 = store.find_tag_by_id(&tag.id).unwrap().unwrap();
        assert_eq!(updated2.name, "uuid-renamed");

        // Disallow empty name
        assert!(
            tag::handle_tag_rename(&store, "uuid-renamed".to_string(), "   ".to_string()).is_err()
        );

        // Error if not found
        assert!(
            tag::handle_tag_rename(&store, "non-existent".to_string(), "valid".to_string())
                .is_err()
        );

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn test_handle_tag_rename_disambiguation() {
        let dir =
            std::env::temp_dir().join(format!("pantagraph_test_disambig_{}", uuid::Uuid::now_v7()));
        let store = JsonlStore::new(&dir).unwrap();
        let tag1 = store.create_tag(Tag::new("dupe", "first")).unwrap();
        let tag2 = store.create_tag(Tag::new("dupe", "second")).unwrap();

        // Ambiguous rename should fail without #N
        assert!(tag::handle_tag_rename(&store, "dupe".to_string(), "renamed".to_string()).is_err());

        // Disambiguated rename should succeed
        // Sorted by UUID order
        let mut expected_order = [tag1.id, tag2.id];
        expected_order.sort_by_key(|id| id.0);

        assert!(
            tag::handle_tag_rename(&store, "dupe#1".to_string(), "renamed-1".to_string()).is_ok()
        );
        let first_updated = store.find_tag_by_id(&expected_order[0]).unwrap().unwrap();
        assert_eq!(first_updated.name, "renamed-1");

        let second_original = store.find_tag_by_id(&expected_order[1]).unwrap().unwrap();
        assert_eq!(second_original.name, "dupe");

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn test_tag_alias_and_entry_integration() {
        let dir = std::env::temp_dir().join(format!(
            "pantagraph_test_alias_int_{}",
            uuid::Uuid::now_v7()
        ));
        let store = JsonlStore::new(&dir).unwrap();

        // 1. Create tag with aliases
        assert!(tag::handle_tag_create(
            &store,
            "rust".to_string(),
            None,
            "Rust programming language".to_string(),
            vec!["rs".to_string(), "rust-lang".to_string()],
            DuplicatePolicy::Allow,
            DuplicatePolicy::Error,
        )
        .is_ok());

        // 2. Resolve by alias
        let resolved = tag::resolve_tag(&store, "rs").unwrap();
        assert_eq!(resolved.name, "rust");

        let resolved_alias2 = tag::resolve_tag(&store, "rust-lang").unwrap();
        assert_eq!(resolved_alias2.id, resolved.id);

        // 3. Add entry using alias
        assert!(entry::handle_entry_add(
            &store,
            "".to_string(),
            "https://www.rust-lang.org".to_string(),
            "Rust website".to_string(),
            vec!["rs".to_string()]
        )
        .is_ok());

        let entries = store.list_entries().unwrap();
        assert_eq!(entries.len(), 1);
        assert_eq!(entries[0].tag_ids, vec![resolved.id]);

        // 4. Filter entries by alias
        assert!(
            entry::handle_entry_list(&store, None, vec!["rust-lang".to_string()], None, None, None, None).is_ok()
        );

        // 5. Add another tag and attach via alias using handle_entry_tag
        assert!(tag::handle_tag_create(
            &store,
            "systems".to_string(),
            None,
            "".to_string(),
            vec!["sys".to_string()],
            DuplicatePolicy::Allow,
            DuplicatePolicy::Error,
        )
        .is_ok());

        assert!(
            entry::handle_entry_tag(&store, entries[0].id.to_string(), "sys".to_string()).is_ok()
        );
        let updated_entry = store.find_entry_by_id(&entries[0].id).unwrap().unwrap();
        assert_eq!(updated_entry.tag_ids.len(), 2);

        // 6. Rename tag referenced by alias
        assert!(tag::handle_tag_rename(&store, "rs".to_string(), "rustacean".to_string()).is_ok());
        let renamed = tag::resolve_tag(&store, "rs").unwrap();
        assert_eq!(renamed.name, "rustacean");

        // 7. Add & remove alias via CLI handler
        assert!(
            tag::handle_tag_alias_add(&store, "rustacean".to_string(), "crab".to_string()).is_ok()
        );
        let with_crab = tag::resolve_tag(&store, "crab").unwrap();
        assert_eq!(with_crab.name, "rustacean");

        assert!(tag::handle_tag_alias_list(&store, "crab".to_string()).is_ok());

        assert!(
            tag::handle_tag_alias_remove(&store, "crab".to_string(), "crab".to_string()).is_ok()
        );
        assert!(tag::resolve_tag(&store, "crab").is_err());

        // 8. Tag show works with alias
        assert!(tag::handle_tag_show(&store, "rs".to_string()).is_ok());

        // 9. Tag list filter matches alias
        assert!(tag::handle_tag_list(&store, Some("rust-lan".to_string()), None, None).is_ok());

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn test_tag_alias_disambiguation() {
        let dir = std::env::temp_dir().join(format!(
            "pantagraph_test_alias_dis_{}",
            uuid::Uuid::now_v7()
        ));
        let store = JsonlStore::new(&dir).unwrap();

        let tag1 = store
            .create_tag(Tag::with_aliases(
                "first",
                "first tag",
                vec!["common".to_string()],
            ))
            .unwrap();
        let tag2 = store
            .create_tag(Tag::with_aliases(
                "second",
                "second tag",
                vec!["common".to_string()],
            ))
            .unwrap();

        // Ambiguous without #N
        assert!(tag::resolve_tag(&store, "common").is_err());

        let mut expected = [tag1.id, tag2.id];
        expected.sort_by_key(|id| id.0);

        let picked1 = tag::resolve_tag(&store, "common#1").unwrap();
        assert_eq!(picked1.id, expected[0]);

        let picked2 = tag::resolve_tag(&store, "common#2").unwrap();
        assert_eq!(picked2.id, expected[1]);

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn test_tag_create_duplicate_policies() {
        let dir =
            std::env::temp_dir().join(format!("pantagraph_test_conflict_{}", uuid::Uuid::now_v7()));
        let store = JsonlStore::new(&dir).unwrap();

        // Initial tag
        store
            .create_tag(Tag::with_aliases("alpha", "desc", vec!["al".to_string()]))
            .unwrap();

        // Duplicate unqualified names are allowed by default.
        let res_allowed = tag::handle_tag_create(
            &store,
            "alpha".to_string(),
            None,
            "".to_string(),
            vec![],
            DuplicatePolicy::Allow,
            DuplicatePolicy::Error,
        );
        assert!(res_allowed.is_ok());

        // Duplicate aliases/names are controlled by the same name policy.
        let res_alias = tag::handle_tag_create(
            &store,
            "al".to_string(),
            None,
            "".to_string(),
            vec![],
            DuplicatePolicy::Error,
            DuplicatePolicy::Error,
        );
        assert!(res_alias.is_err());

        // Name policy can be set to warn.
        let res_warn = tag::handle_tag_create(
            &store,
            "alpha".to_string(),
            None,
            "".to_string(),
            vec![],
            DuplicatePolicy::Warn,
            DuplicatePolicy::Error,
        );
        assert!(res_warn.is_ok());

        // Now three tags with name "alpha" exist.
        let found = store.find_tags_by_name_or_alias("alpha").unwrap();
        assert_eq!(found.len(), 3);

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn test_tag_specifier_resolution() {
        let dir = std::env::temp_dir().join(format!(
            "pantagraph_test_specifier_{}",
            uuid::Uuid::now_v7()
        ));
        let store = JsonlStore::new(&dir).unwrap();

        // Create two tags with same name "rust", but different specifiers
        assert!(tag::handle_tag_create(
            &store,
            "rust@lang".to_string(),
            None,
            "Rust language".to_string(),
            vec!["rs".to_string()],
            DuplicatePolicy::Allow,
            DuplicatePolicy::Error,
        )
        .is_ok());

        assert!(tag::handle_tag_create(
            &store,
            "rust".to_string(),
            Some("game".to_string()),
            "Rust video game".to_string(),
            vec![],
            DuplicatePolicy::Allow,
            DuplicatePolicy::Error,
        )
        .is_ok());

        // Bare "rust" is ambiguous
        assert!(tag::resolve_tag(&store, "rust").is_err());

        // "rust@lang" resolves to the programming language tag
        let lang_tag = tag::resolve_tag(&store, "rust@lang").unwrap();
        assert_eq!(lang_tag.name, "rust");
        assert_eq!(lang_tag.specifier, Some("lang".to_string()));
        assert_eq!(lang_tag.display_name(), "rust@lang");

        // "rust@game" resolves to the video game tag
        let game_tag = tag::resolve_tag(&store, "rust@game").unwrap();
        assert_eq!(game_tag.name, "rust");
        assert_eq!(game_tag.specifier, Some("game".to_string()));
        assert_eq!(game_tag.display_name(), "rust@game");

        // Alias "rs@lang" also resolves correctly
        let alias_spec = tag::resolve_tag(&store, "rs@lang").unwrap();
        assert_eq!(alias_spec.id, lang_tag.id);

        // When rust@lang and rust@game exist:
        // A different qualified ref is allowed.
        assert!(tag::handle_tag_create(
            &store,
            "rust@editor".to_string(),
            None,
            "Rust editor".to_string(),
            vec![],
            DuplicatePolicy::Allow,
            DuplicatePolicy::Error,
        )
        .is_ok());

        // Duplicate qualified refs fail by default.
        assert!(tag::handle_tag_create(
            &store,
            "rust@lang".to_string(),
            None,
            "Rust lang duplicate".to_string(),
            vec![],
            DuplicatePolicy::Allow,
            DuplicatePolicy::Error,
        )
        .is_err());

        // A bare name matching an existing qualified name uses the qualified policy.
        assert!(tag::handle_tag_create(
            &store,
            "rust".to_string(),
            None,
            "Bare rust".to_string(),
            vec![],
            DuplicatePolicy::Allow,
            DuplicatePolicy::Error,
        )
        .is_err());

        // A bare duplicate with only other bare tags uses the name policy.
        assert!(tag::handle_tag_create(
            &store,
            "rust".to_string(),
            None,
            "Bare rust".to_string(),
            vec![],
            DuplicatePolicy::Error,
            DuplicatePolicy::Error,
        )
        .is_err());

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn test_handle_entry_list_query() {
        let dir = std::env::temp_dir().join(format!(
            "pantagraph_test_entry_query_{}",
            uuid::Uuid::now_v7()
        ));
        let store = JsonlStore::new(&dir).unwrap();

        let alpha = store.create_tag(Tag::new("alpha", "")).unwrap().id;
        let beta = store.create_tag(Tag::new("beta", "")).unwrap().id;
        let gamma = store.create_tag(Tag::new("gamma", "")).unwrap().id;

        let ids = |tag_ids: Vec<TagId>| {
            store
                .create_entry(Entry::new("https://x.test", "", tag_ids))
                .unwrap()
                .id
        };
        let ab = ids(vec![alpha, beta]);
        let a = ids(vec![alpha]);
        let b = ids(vec![beta]);
        let bg = ids(vec![beta, gamma]);
        let g = ids(vec![gamma]);

        let search = |query: Option<&str>, tag_refs: &[&str]| {
            let result = entry::handle_entry_list(
                &store,
                None,
                tag_refs.iter().map(|s| s.to_string()).collect(),
                query.map(str::to_string),
                None,
                None,
                None,
            )
            .unwrap();
            match result {
                entry::EntryResult::Listed { entries, .. } => {
                    entries.iter().map(|e| e.id).collect::<Vec<_>>()
                }
                _ => panic!("expected Listed result"),
            }
        };

        // Implicit AND and explicit * return the same entries.
        assert_eq!(search(Some("alpha beta"), &[]), vec![ab]);
        assert_eq!(search(Some("alpha * beta"), &[]), vec![ab]);
        // `A + B C` is parsed as `A + (B * C)`.
        assert_eq!(search(Some("alpha + beta gamma"), &[]), vec![ab, a, bg]);
        assert_eq!(search(Some("alpha + beta"), &[]), vec![ab, a, b, bg]);
        // `A * !B` returns entries with A but without B.
        assert_eq!(search(Some("alpha * !beta"), &[]), vec![a]);
        assert_eq!(search(Some("!alpha"), &[]), vec![b, bg, g]);
        // `-t` tags combine with the query via AND.
        assert_eq!(search(Some("beta"), &["alpha"]), vec![ab]);
        assert_eq!(search(None, &["beta"]), vec![ab, b, bg]);
        // Malformed queries and unknown tags fail without panicking.
        assert!(entry::handle_entry_list(
            &store,
            None,
            vec![],
            Some("alpha +".to_string()),
            None,
            None,
            None
        )
        .is_err());
        assert!(entry::handle_entry_list(
            &store,
            None,
            vec![],
            Some("alpha missing-tag".to_string()),
            None,
            None,
            None
        )
        .is_err());

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn test_handle_entry_list_limit_offset_after() {
        let dir = std::env::temp_dir().join(format!(
            "pantagraph_test_entry_limit_offset_after_{}",
            uuid::Uuid::now_v7()
        ));
        let store = JsonlStore::new(&dir).unwrap();

        let alpha = store.create_tag(Tag::new("alpha", "")).unwrap().id;
        let beta = store.create_tag(Tag::new("beta", "")).unwrap().id;

        let ids = |tag_ids: Vec<TagId>| {
            store
                .create_entry(Entry::new("https://x.test", "", tag_ids))
                .unwrap()
                .id
        };
        let e0 = ids(vec![alpha]);
        let e1 = ids(vec![alpha, beta]);
        let e2 = ids(vec![beta]);
        let e3 = ids(vec![]);
        let e4 = ids(vec![alpha]);

        let list = |tag_refs: &[&str],
                    query: Option<&str>,
                    limit: Option<usize>,
                    offset: Option<usize>,
                    after: Option<&str>| {
            let result = entry::handle_entry_list(
                &store,
                None,
                tag_refs.iter().map(|s| s.to_string()).collect(),
                query.map(str::to_string),
                limit,
                offset,
                after.map(str::to_string),
            )
            .unwrap();
            match result {
                entry::EntryResult::Listed { entries, .. } => {
                    entries.iter().map(|e| e.id).collect::<Vec<_>>()
                }
                _ => panic!("expected Listed result"),
            }
        };

        // limit alone truncates from the start.
        assert_eq!(list(&[], None, Some(2), None, None), vec![e0, e1]);
        // offset alone skips from the start.
        assert_eq!(list(&[], None, None, Some(3), None), vec![e3, e4]);
        // limit + offset together.
        assert_eq!(list(&[], None, Some(2), Some(1), None), vec![e1, e2]);
        // offset beyond the end returns nothing.
        assert_eq!(
            list(&[], None, Some(5), Some(10), None),
            Vec::<domain::EntryId>::new()
        );
        // after (unfiltered) returns entries strictly after the anchor.
        let e1_str = e1.to_string();
        assert_eq!(list(&[], None, None, None, Some(&e1_str)), vec![e2, e3, e4]);
        // after + limit combines.
        assert_eq!(
            list(&[], None, Some(2), None, Some(&e1_str)),
            vec![e2, e3]
        );
        // after + offset combines.
        assert_eq!(
            list(&[], None, None, Some(1), Some(&e1_str)),
            vec![e3, e4]
        );
        // after on an unknown / malformed id is a no-op.
        assert_eq!(
            list(&[], None, Some(2), None, Some("not-a-uuid")),
            vec![e0, e1]
        );
        // after with a tag filter still works (falls back to in-memory path).
        // Entries with `alpha`: e0, e1, e4.
        let a_only = vec![e0, e1, e4];
        assert_eq!(
            list(&["alpha"], None, None, None, Some(&e0.to_string())),
            vec![e1, e4]
        );
        assert_eq!(
            list(&["alpha"], None, Some(1), None, Some(&e0.to_string())),
            vec![e1]
        );
        assert_eq!(list(&["alpha"], None, None, None, None), a_only);
        // after with a query still works (falls back to in-memory path).
        assert_eq!(
            list(&[], Some("alpha"), None, None, Some(&e0.to_string())),
            vec![e1, e4]
        );
        assert_eq!(
            list(&[], Some("alpha"), Some(1), None, Some(&e0.to_string())),
            vec![e1]
        );

        let _ = std::fs::remove_dir_all(&dir);
    }
}
