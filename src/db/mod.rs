use std::path::Path;

use anyhow::{Context, Result};
use rusqlite::{Connection, OpenFlags};

const SCHEMA: &str = r#"
CREATE TABLE IF NOT EXISTS tags (
    id TEXT PRIMARY KEY,
    name TEXT NOT NULL,
    specifier TEXT,
    description TEXT NOT NULL DEFAULT '',
    aliases TEXT NOT NULL DEFAULT '[]'
);

CREATE TABLE IF NOT EXISTS entries (
    id TEXT PRIMARY KEY,
    url TEXT NOT NULL,
    archiver TEXT,
    archive_path TEXT,
    description TEXT NOT NULL DEFAULT '',
    ordinal INTEGER NOT NULL
);

CREATE TABLE IF NOT EXISTS entry_tags (
    entry_id TEXT NOT NULL,
    tag_id TEXT NOT NULL,
    PRIMARY KEY (entry_id, tag_id),
    FOREIGN KEY (entry_id) REFERENCES entries(id),
    FOREIGN KEY (tag_id) REFERENCES tags(id)
);
"#;

pub fn open_store_db(path: &Path) -> Result<Connection> {
    let conn = Connection::open_with_flags(
        path,
        OpenFlags::SQLITE_OPEN_READ_WRITE | OpenFlags::SQLITE_OPEN_CREATE,
    )
    .with_context(|| format!("Failed to open store DB {}", path.display()))?;
    conn.execute_batch(SCHEMA)
        .context("Failed to init schema")?;
    Ok(conn)
}
