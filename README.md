# Pantagraph

[English](README.md) | [日本語](README-ja_JP.md)

Index the web through human-assigned tags, with support for archiving content via external tools.

## Features

- **Tag Management**: Flat, human-assigned tags with alias support. Tags with the same name or alias can coexist and be disambiguated cleanly.
- **Entry Management**: Save web URLs with descriptions and tags.
- **Pluggable Archivers**: Save snapshots (HTML, videos, etc.) locally using external archivers (e.g., `single-file`, `yt-dlp`) configurable via `tool.toml`.
- **JSONL-based Storage**: Tags and entries are stored as one JSON record per line (`tags.jsonl`, `entries.jsonl`). Existing TOML data is migrated on first startup.

---

## Installation & Setup

Build with Cargo:

```bash
cargo build --release
```

Or run directly via Cargo:

```bash
cargo run -- <COMMAND>
```

---

## Configuration

### 1. General Configuration (`config.toml`)

Pantagraph looks for `config.toml` in your data directory (e.g. `data/config.toml`) or the current working directory.
A sample configuration is provided in [`config.toml.sample`](tool.toml.sample):

```bash
cp config.toml.sample config.toml
```

#### Duplicate Tag Policies

Duplicate tag names and duplicate qualified references can be configured independently:

```toml
[tag]
same_name_policy = "allow" # "allow" | "warn" | "error"
same_qualified_ref_policy = "error" # "allow" | "warn" | "error"
```

- **`same_name_policy`** (`"allow"` by default): Controls duplicate names without a specifier.
- **`same_qualified_ref_policy`** (`"error"` by default): Controls duplicate `name@specifier` references.
- **`"warn"`** prints a warning and continues; **`"error"`** aborts with conflict details.

---

### 2. Archiver Tool Configuration (`tool.toml`)

Pantagraph allows you to configure external archiving tools flexibly. By default, it looks for `tool.toml` in your data directory (e.g. `data/tool.toml`) or the current working directory.

A sample configuration is provided in [`tool.toml.sample`](file:///home/yuya/projects/20_sandbox/pantagraph/tool.toml.sample):

```bash
cp tool.toml.sample tool.toml
```

### Example `tool.toml`

```toml
[archiver.single-file]
command = "single-file"
args = [
  "--browser-executable-path=google-chrome",
  "--browser-headless=true",
  "--output-directory={outdir}",
  "{url}"
]

[archiver.ytdlp]
command = "yt-dlp"
args = [
  "-P", "{outdir}",
  "{url}"
]
```

#### Available Placeholders in `args`:
- `{url}`: Target URL to archive.
- `{outdir}` (or `{output}`): Output directory assigned for this entry (`<data_dir>/archives/<entry_id>/`).

---

## Usage

### 1. Tag Commands

#### Create a tag
```bash
pantagraph tag create <NAME> [--specifier <SPEC>] [--description <DESC>] [--alias <ALIAS>...]
# Example:
pantagraph tag create rust -d "Rust programming language" -a rs -a rust-lang
# Or create with a specifier directly (via flag or name@specifier):
pantagraph tag create "rust@lang" -d "Rust programming language"
pantagraph tag create rust -s game -d "Rust survival game"
```

#### List tags
```bash
pantagraph tag list [--name <FILTER>]
```
*Note: Filters by tag name or alias.*

#### Show tag details
```bash
pantagraph tag show <NAME_OR_ALIAS_OR_ID>
```
*Note: If multiple tags share the same name or alias, use `name@specifier` (e.g. `rust@lang`), `name#N` (e.g. `rust#1`, `rust#2`), or the tag's UUID to disambiguate.*

#### Rename a tag
```bash
pantagraph tag rename <NAME_OR_ALIAS_OR_ID> <NEW_NAME>
# Example:
pantagraph tag rename rust@lang rust-lang
```
*Note: If multiple tags share the same name or alias, use `name@specifier`, `name#N`, or the tag's UUID to disambiguate.*

#### Manage tag aliases
Aliases can be used interchangeably with tag names in all commands (e.g., `tag show`, `tag rename`, `entry add --tag`, `entry list --tag`, `entry tag`).

```bash
# Add an alias to a tag
pantagraph tag alias add <NAME_OR_ALIAS_OR_ID> <ALIAS>
# Example:
pantagraph tag alias add rust rs

# Remove an alias from a tag
pantagraph tag alias remove <NAME_OR_ALIAS_OR_ID> <ALIAS>
# Example:
pantagraph tag alias remove rust rs

# List aliases for a tag
pantagraph tag alias list <NAME_OR_ALIAS_OR_ID>
```

---

### 2. Entry Commands

#### Add an entry (URL bookmark only)
By default, `--archiver` is empty (`""`), which saves the entry as a simple URL bookmark without running an external tool:
```bash
pantagraph entry add <URL> --tag <TAG> [-d <DESC>]
# Example:
pantagraph entry add https://doc.rust-lang.org --tag rust -d "Official Rust Documentation"
```

#### Add an entry with an external archiver
Specify `--archiver=<NAME>` to invoke the corresponding archiver defined in `tool.toml`:
```bash
pantagraph entry add <URL> --archiver single-file --tag <TAG>
```
This runs the archiver, saves snapshots into `<data_dir>/archives/<entry_id>/`, and records archive metadata in `entries.jsonl` under `meta` (including `archived`, `archiver`, and `archive_path`).

#### List entries
```bash
# List all entries
pantagraph entry list

# Filter by tag (repeatable: matches entries having ALL specified tags)
pantagraph entry list --tag rust --tag dev

# Filter by archiver
pantagraph entry list --archiver single-file

# Filter URL-only bookmarks (no archiver)
pantagraph entry list --archiver ""
```

#### Show entry details
```bash
pantagraph entry show <ENTRY_ID>
```

#### Add a tag to an existing entry
```bash
pantagraph entry tag <ENTRY_ID> <TAG>
```

---

## Data Storage

Data is stored in the data directory (`data/` by default, customizable with `--data-dir <PATH>`):

```
data/
├── entries.jsonl
├── tags.jsonl
├── tool.toml        # (Optional location for tool.toml)
└── archives/
    └── <entry_id>/  # Files generated by external archivers
```
#### Meta attributes

Meta attributes are stored under the `meta` field in `entries.jsonl` (a key–value map of strings). Archiver-related information is recorded in `meta`:
- `archived` (string): `"true"` if an external archiver was successfully executed.
- `archiver` (string): Name of the archiver used.
- `archive_path` (string): Filesystem path or identifier recorded by the archiver (typically `<data_dir>/archives/<entry_id>/`).

Example:
```json
{"id":"...","url":"...","description":"...","tag_ids":["..."],"meta":{"archived":"true","archiver":"single-file","archive_path":"archives/<entry_id>/"}}
```

You can also manage arbitrary meta keys via:
- `pantagraph entry meta set <ENTRY_ID> <KEY> <VALUE>`
- `pantagraph entry meta remove <ENTRY_ID> <KEY>`
- `pantagraph entry meta list <ENTRY_ID>`

# Query Syntax

Entries can be filtered using the `--query` option with a boolean expression over tag references. Supported operators:
- Space or `*` for AND (intersection)
- `+` for OR (union)
- `!` for NOT (complement)
- Parentheses `()` for grouping

In addition, `meta:<key>[=<value>]` filters by meta attributes (exact match for value; presence only if value omitted).

Examples:
```bash
# Entries matching both rust AND game tags
pantagraph entry list --query "rust game"
pantagraph entry list --query "rust * game"

# Entries matching rust OR game
pantagraph entry list --query "rust + game"

# Entries with rust but NOT game
pantagraph entry list --query "rust * !game"
pantagraph entry list --query "rust !game"

# Entries archived with yt-dlp
pantagraph entry list --query "meta:archiver=yt-dlp"

# Entries that have an archiver (any value)
pantagraph entry list --query "meta:archiver"

# Entries without any archiver
pantagraph entry list --query "!meta:archiver"

# Complex combination
pantagraph entry list --query "(rust + cpp) * meta:archived=true"
```
