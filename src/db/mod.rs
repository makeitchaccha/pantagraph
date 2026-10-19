use std::path::Path;

use anyhow::{Context, Result};
use rusqlite::{Connection, OpenFlags};

const SCHEMA: &str = r#"
CREATE TABLE IF NOT EXISTS tags (
    internal_id INTEGER PRIMARY KEY,
    id TEXT NOT NULL UNIQUE,
    name TEXT NOT NULL,
    specifier TEXT,
    description TEXT NOT NULL DEFAULT '',
    aliases TEXT NOT NULL DEFAULT '[]'
);

CREATE TABLE IF NOT EXISTS entries (
    internal_id INTEGER PRIMARY KEY,
    id TEXT NOT NULL UNIQUE,
    url TEXT NOT NULL,
    description TEXT NOT NULL DEFAULT '',
    ordinal INTEGER NOT NULL
);

CREATE TABLE IF NOT EXISTS entry_tags (
    entry_id INTEGER NOT NULL,
    tag_id INTEGER NOT NULL,
    PRIMARY KEY (entry_id, tag_id),
    FOREIGN KEY (entry_id) REFERENCES entries(internal_id),
    FOREIGN KEY (tag_id) REFERENCES tags(internal_id)
);
"#;

/// Meta tables are created separately, after [`migrate_legacy_schema`]: that
/// migration renames `entries`, and SQLite rewrites FK references in existing
/// tables to point at the renamed table — which would leave `entry_meta`
/// referencing the dropped `entries_legacy`.
const META_SCHEMA: &str = r#"
CREATE TABLE IF NOT EXISTS meta_keys (
    internal_id INTEGER PRIMARY KEY,
    key TEXT NOT NULL UNIQUE
);

CREATE TABLE IF NOT EXISTS entry_meta (
    entry_id INTEGER NOT NULL,
    key_id INTEGER NOT NULL,
    value TEXT NOT NULL,
    PRIMARY KEY (entry_id, key_id),
    FOREIGN KEY (entry_id) REFERENCES entries(internal_id),
    FOREIGN KEY (key_id) REFERENCES meta_keys(internal_id)
);
"#;

const INDEXES: &str = r#"
CREATE INDEX IF NOT EXISTS idx_entry_tags_tag_entry ON entry_tags(tag_id, entry_id);
CREATE INDEX IF NOT EXISTS idx_entries_ordinal ON entries(ordinal);
CREATE INDEX IF NOT EXISTS idx_entry_meta_key_entry ON entry_meta(key_id, entry_id);
"#;

/// Upgrade databases created before internal integer IDs were introduced.
/// UUIDs remain the stable external IDs; only the join table is rewritten.
fn migrate_legacy_schema(conn: &Connection) -> Result<()> {
    let entries_has_internal_id: bool = conn.query_row(
        "SELECT EXISTS(SELECT 1 FROM pragma_table_info('entries') WHERE name = 'internal_id')",
        [],
        |row| row.get(0),
    )?;
    let tags_has_internal_id: bool = conn.query_row(
        "SELECT EXISTS(SELECT 1 FROM pragma_table_info('tags') WHERE name = 'internal_id')",
        [],
        |row| row.get(0),
    )?;
    if entries_has_internal_id && tags_has_internal_id {
        return Ok(());
    }

    conn.execute_batch(
        r#"
        PRAGMA foreign_keys = OFF;
        BEGIN IMMEDIATE;
        ALTER TABLE entries RENAME TO entries_legacy;
        ALTER TABLE tags RENAME TO tags_legacy;
        ALTER TABLE entry_tags RENAME TO entry_tags_legacy;

        CREATE TABLE tags (
            internal_id INTEGER PRIMARY KEY,
            id TEXT NOT NULL UNIQUE,
            name TEXT NOT NULL,
            specifier TEXT,
            description TEXT NOT NULL DEFAULT '',
            aliases TEXT NOT NULL DEFAULT '[]'
        );
        INSERT INTO tags (id, name, specifier, description, aliases)
        SELECT id, name, specifier, description, aliases FROM tags_legacy;

        CREATE TABLE entries (
            internal_id INTEGER PRIMARY KEY,
            id TEXT NOT NULL UNIQUE,
            url TEXT NOT NULL,
            archiver TEXT,
            archive_path TEXT,
            description TEXT NOT NULL DEFAULT '',
            ordinal INTEGER NOT NULL
        );
        INSERT INTO entries (id, url, archiver, archive_path, description, ordinal)
        SELECT id, url, archiver, archive_path, description, ordinal FROM entries_legacy;

        CREATE TABLE entry_tags (
            entry_id INTEGER NOT NULL,
            tag_id INTEGER NOT NULL,
            PRIMARY KEY (entry_id, tag_id),
            FOREIGN KEY (entry_id) REFERENCES entries(internal_id),
            FOREIGN KEY (tag_id) REFERENCES tags(internal_id)
        );
        INSERT OR IGNORE INTO entry_tags (entry_id, tag_id)
        SELECT e.internal_id, t.internal_id
        FROM entry_tags_legacy et
        JOIN entries e ON e.id = et.entry_id
        JOIN tags t ON t.id = et.tag_id;

        DROP TABLE entry_tags_legacy;
        DROP TABLE entries_legacy;
        DROP TABLE tags_legacy;
        CREATE INDEX idx_entry_tags_tag_entry ON entry_tags(tag_id, entry_id);
        CREATE INDEX idx_entries_ordinal ON entries(ordinal);
        COMMIT;
        PRAGMA foreign_keys = ON;
        "#,
    )?;
    Ok(())
}

/// Move the legacy `entries.archiver` / `entries.archive_path` columns into
/// the normalized meta tables and drop the columns.
///
/// Entries with an archiver get `archiver=<name>`, `archived=true`, and
/// `archive_path=<path>` meta keys. Idempotent: when the columns are absent
/// (fresh databases, or a database already migrated) this is a no-op.
fn migrate_archiver_to_meta(conn: &Connection) -> Result<()> {
    let has_archiver: bool = conn.query_row(
        "SELECT EXISTS(SELECT 1 FROM pragma_table_info('entries') WHERE name = 'archiver')",
        [],
        |row| row.get(0),
    )?;
    if !has_archiver {
        return Ok(());
    }
    conn.execute_batch(
        r#"
        BEGIN IMMEDIATE;
        INSERT OR IGNORE INTO meta_keys (key) VALUES ('archiver');
        INSERT OR IGNORE INTO meta_keys (key) VALUES ('archive_path');
        INSERT OR IGNORE INTO meta_keys (key) VALUES ('archived');

        INSERT OR IGNORE INTO entry_meta (entry_id, key_id, value)
        SELECT e.internal_id, mk.internal_id, e.archiver
        FROM entries e, meta_keys mk
        WHERE e.archiver IS NOT NULL AND mk.key = 'archiver';

        INSERT OR IGNORE INTO entry_meta (entry_id, key_id, value)
        SELECT e.internal_id, mk.internal_id, e.archive_path
        FROM entries e, meta_keys mk
        WHERE e.archive_path IS NOT NULL AND mk.key = 'archive_path';

        INSERT OR IGNORE INTO entry_meta (entry_id, key_id, value)
        SELECT e.internal_id, mk.internal_id, 'true'
        FROM entries e, meta_keys mk
        WHERE e.archiver IS NOT NULL AND mk.key = 'archived';

        ALTER TABLE entries DROP COLUMN archiver;
        ALTER TABLE entries DROP COLUMN archive_path;
        COMMIT;
        "#,
    )?;
    Ok(())
}

pub fn open_store_db(path: &Path) -> Result<Connection> {
    let conn = Connection::open_with_flags(
        path,
        OpenFlags::SQLITE_OPEN_READ_WRITE | OpenFlags::SQLITE_OPEN_CREATE,
    )
    .with_context(|| format!("Failed to open store DB {}", path.display()))?;
    conn.execute_batch(SCHEMA)
        .context("Failed to init schema")?;
    migrate_legacy_schema(&conn).context("Failed to migrate internal integer IDs")?;
    conn.execute_batch(META_SCHEMA)
        .context("Failed to init meta schema")?;
    migrate_archiver_to_meta(&conn).context("Failed to migrate archiver columns to meta")?;
    conn.execute_batch(INDEXES)
        .context("Failed to init indexes")?;
    Ok(conn)
}
