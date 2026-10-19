use std::{
    fs::{self, File},
    io::{BufWriter, Write},
    path::{Path, PathBuf},
};

use anyhow::{Context, Result};
use clap::Parser;
use rand::{rngs::StdRng, RngExt, SeedableRng};
use uuid::Uuid;

use pantagraph::domain::{Entry, EntryId, Tag, TagId};

#[derive(Debug, Parser)]
#[command(name = "generate-fixture")]
#[command(about = "Generate a Pantagraph JSONL fixture")]
struct Args {
    /// Number of tags to generate.
    #[arg(long, default_value_t = 500)]
    tags: usize,

    /// Number of entries to generate.
    #[arg(long, default_value_t = 10_000)]
    entries: usize,

    /// Minimum number of tags assigned to each entry.
    #[arg(long, default_value_t = 1)]
    min_tags: usize,

    /// Maximum number of tags assigned to each entry.
    #[arg(long, default_value_t = 8)]
    max_tags: usize,

    /// Random seed.
    #[arg(long, default_value_t = 42)]
    seed: u64,

    /// Vocabulary for generated names and descriptions (comma-separated).
    #[arg(long, value_delimiter = ',', default_values = [
        "amber", "bright", "calm", "clear", "cloud", "daily", "deep", "field", "fresh",
        "green", "kind", "light", "local", "open", "quiet", "river", "simple", "small",
        "solid", "useful", "warm", "wide", "wild", "wood"
    ])]
    words: Vec<String>,

    /// Output directory.
    #[arg(long, default_value = "./fixture")]
    output: PathBuf,
}

fn main() -> Result<()> {
    let args = Args::parse();

    if args.min_tags > args.max_tags {
        anyhow::bail!(
            "--min-tags ({}) must not be greater than --max-tags ({})",
            args.min_tags,
            args.max_tags
        );
    }

    generate_fixture(
        &args.output,
        args.seed,
        args.tags,
        args.entries,
        args.min_tags,
        args.max_tags,
        &args.words,
    )
}

fn generate_fixture(
    output: &Path,
    seed: u64,
    tag_count: usize,
    entry_count: usize,
    min_tags: usize,
    max_tags: usize,
    words: &[String],
) -> Result<()> {
    fs::create_dir_all(output)
        .with_context(|| format!("failed to create output directory: {}", output.display()))?;

    let mut rng = StdRng::seed_from_u64(seed);

    let default_words = ["sample".to_string(), "item".to_string()];
    let words = if words.is_empty() {
        &default_words
    } else {
        words
    };
    let tags = generate_tags(&mut rng, tag_count, words);
    write_jsonl(output.join("tags.jsonl"), &tags).context("failed to write tags.jsonl")?;

    let mut entries_writer = BufWriter::new(
        File::create(output.join("entries.jsonl")).context("failed to create entries.jsonl")?,
    );
    let mut archived_entries = 0;
    let mut total_tag_assignments = 0;
    for entry in generate_entries(&mut rng, entry_count, &tags, min_tags, max_tags, words) {
        if entry.meta.contains_key("archiver") {
            archived_entries += 1;
        }
        total_tag_assignments += entry.tag_ids.len();
        serde_json::to_writer(&mut entries_writer, &entry).context("failed to serialize entry")?;
        entries_writer
            .write_all(b"\n")
            .context("failed to write entry")?;
    }
    entries_writer
        .flush()
        .context("failed to flush entries.jsonl")?;

    let average_tags = if entry_count == 0 {
        0.0
    } else {
        total_tag_assignments as f64 / entry_count as f64
    };

    let readme = format!(
        "\
Pantagraph fixture

seed: {seed}
tags: {}
entries: {}
tags per entry: {}..={}
average tags per entry: {average_tags:.2}
archived entries: {archived_entries}

Generated with:

    cargo run --bin generate-fixture -- \\
        --tags {} \\
        --entries {} \\
        --min-tags {} \\
        --max-tags {} \\
        --seed {} \\
        --words {} \\
        --output {}

Files:

    tags.jsonl
    entries.jsonl
",
        tags.len(),
        entry_count,
        min_tags,
        max_tags,
        tag_count,
        entry_count,
        min_tags,
        max_tags,
        seed,
        words.join(","),
        output.display(),
    );

    fs::write(output.join("README.txt"), readme).context("failed to write README.txt")?;

    println!("Generated fixture:");
    println!("  output:           {}", output.display());
    println!("  tags:             {}", tags.len());
    println!("  entries:          {entry_count}");
    println!("  tags per entry:   {min_tags}..={max_tags}");
    println!("  average tags:     {average_tags:.2}");
    println!("  archived entries: {archived_entries}");
    println!("  seed:             {seed}");

    Ok(())
}

fn write_jsonl<T: serde::Serialize>(path: PathBuf, values: &[T]) -> Result<()> {
    let mut writer = BufWriter::new(File::create(path)?);
    for value in values {
        serde_json::to_writer(&mut writer, value)?;
        writer.write_all(b"\n")?;
    }
    writer.flush()?;
    Ok(())
}

fn generate_tags(rng: &mut StdRng, count: usize, words: &[String]) -> Vec<Tag> {
    (0..count)
        .map(|i| {
            let id = TagId(Uuid::now_v7());
            let name = if i > 0 && rng.random_bool(0.25) {
                // Intentionally repeat names so fixtures exercise ambiguous tags.
                format_tag_name(words, rng.random_range(0..i))
            } else {
                format_tag_name(words, i)
            };
            let specifier = rng
                .random_bool(0.35)
                .then(|| words[rng.random_range(0..words.len())].clone());

            let aliases = if rng.random_bool(0.20) {
                vec![format!("{name}-alternate")]
            } else {
                Vec::new()
            };

            Tag {
                id,
                name,
                specifier,
                description: format!(
                    "{} {} collection",
                    words[rng.random_range(0..words.len())],
                    words[rng.random_range(0..words.len())]
                ),
                aliases,
            }
        })
        .collect()
}

fn generate_entries<'a>(
    rng: &'a mut StdRng,
    count: usize,
    tags: &'a [Tag],
    min_tags: usize,
    max_tags: usize,
    words: &'a [String],
) -> impl Iterator<Item = Entry> + 'a {
    const ARCHIVERS: &[Option<&str>] = &[None, Some("single-file"), Some("curl"), Some("yt-dlp")];

    (0..count).map(move |i| {
        let id = EntryId(Uuid::now_v7());

        let first = &words[rng.random_range(0..words.len())];
        let second = &words[rng.random_range(0..words.len())];
        let url = format!("https://example.com/{first}/{second}/{i}");

        let archiver = ARCHIVERS[rng.random_range(0..ARCHIVERS.len())].map(str::to_string);

        let mut meta = std::collections::BTreeMap::new();
        if let Some(name) = archiver {
            let archive_path = format!("archives/{name}/{id}.html");
            meta.insert("archived".to_string(), "true".to_string());
            meta.insert("archiver".to_string(), name);
            meta.insert("archive_path".to_string(), archive_path);
        }

        let tag_ids = generate_tag_ids(rng, tags, min_tags, max_tags);

        Entry {
            id,
            url,
            description: format!("A {first} {second} resource"),
            tag_ids,
            meta,
        }
    })
}

fn format_tag_name(words: &[String], index: usize) -> String {
    let combinations = words.len() * words.len();
    let first = index % words.len();
    let second = (index / words.len()) % words.len();
    let name = format!("{}-{}", words[first], words[second]);
    if index >= combinations {
        format!("{name}-{}", index / combinations + 1)
    } else {
        name
    }
}

fn generate_tag_ids(
    rng: &mut StdRng,
    tags: &[Tag],
    min_tags: usize,
    max_tags: usize,
) -> Vec<TagId> {
    if tags.is_empty() {
        return Vec::new();
    }

    let min_tags = min_tags.min(tags.len());
    let max_tags = max_tags.min(tags.len()).max(min_tags);

    let count = rng.random_range(min_tags..=max_tags);

    let mut selected = Vec::with_capacity(count);

    while selected.len() < count {
        let tag = &tags[rng.random_range(0..tags.len())];

        if !selected.contains(&tag.id) {
            selected.push(tag.id);
        }
    }

    selected
}
