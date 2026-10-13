use std::collections::HashMap;

mod archiver;
mod domain;
mod store;

use std::path::Path;

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
    },
    /// List all tags
    List {
        /// Filter by name (substring match)
        #[arg(long)]
        name: Option<String>,
    },
    /// Show tag details
    Show {
        /// Tag name or ID
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

    let mut tags = store.find_tags_by_name(name)?;
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
            eprintln!("Multiple tags with name '{name}':");
            for (i, tag) in tags.iter().enumerate() {
                if tag.description.is_empty() {
                    eprintln!("  {name}#{}", i + 1);
                } else {
                    eprintln!("  {name}#{} {}", i + 1, tag.description);
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

fn handle_tag_create(store: &TomlStore, name: String, description: String) -> Result<()> {
    let tag = Tag::new(&name, &description);
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
            .filter(|t| t.name.contains(filter.as_str()))
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

    let (archiver_opt, archive_path_opt) = if archiver.trim().is_empty() {
        (None, None)
    } else {
        let archiver_name = archiver.trim();
        let tool_config = ToolConfig::load_from_dir(store.data_dir())?;

        // Determine destination file path
        let archives_dir = store.data_dir().join("archives");
        std::fs::create_dir_all(&archives_dir)
            .with_context(|| format!("Failed to create directory {}", archives_dir.display()))?;

        // Extension determined by archiver name or default to html
        let ext = if archiver_name == "single-file" {
            "html"
        } else {
            "archive"
        };

        // Create a unique entry ID upfront for archiving
        let entry_id = EntryId::new();
        let file_name = format!("{entry_id}.{ext}");
        let full_output_path = archives_dir.join(&file_name);

        println!("Archiving with '{}'...", archiver_name);
        tool_config.execute_archiver(archiver_name, &url, &full_output_path)?;

        let relative_path = format!("archives/{file_name}");
        (Some(archiver_name.to_string()), Some(relative_path))
    };

    let mut entry = Entry::new(
        &url,
        archiver_opt,
        archive_path_opt,
        &description,
        tag_ids,
    );
    // If we already generated an entry_id during archiving, use that ID
    if let Some(ref path) = entry.archive_path {
        if let Some(file_stem) = Path::new(path).file_stem().and_then(|s| s.to_str()) {
            if let Ok(id) = file_stem.parse::<EntryId>() {
                entry.id = id;
            }
        }
    }

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
            TagCommands::Create { name, description } => {
                handle_tag_create(&store, name, description)
            }
            TagCommands::List { name } => handle_tag_list(&store, name),
            TagCommands::Show { name_or_id } => handle_tag_show(&store, name_or_id),
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
