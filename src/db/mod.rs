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
    archiver TEXT,
    archive_path TEXT,
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

const INDEXES: &str = r#"
CREATE INDEX IF NOT EXISTS idx_entry_tags_tag_entry ON entry_tags(tag_id, entry_id);
CREATE INDEX IF NOT EXISTS idx_entries_ordinal ON entries(ordinal);
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

pub fn open_store_db(path: &Path) -> Result<Connection> {
    let conn = Connection::open_with_flags(
        path,
        OpenFlags::SQLITE_OPEN_READ_WRITE | OpenFlags::SQLITE_OPEN_CREATE,
    )
    .with_context(|| format!("Failed to open store DB {}", path.display()))?;
    conn.execute_batch(SCHEMA)
        .context("Failed to init schema")?;
    migrate_legacy_schema(&conn).context("Failed to migrate internal integer IDs")?;
    conn.execute_batch(INDEXES)
        .context("Failed to init indexes")?;
    Ok(conn)
}
