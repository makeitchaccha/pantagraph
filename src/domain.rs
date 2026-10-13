use serde::{Deserialize, Serialize};
use std::fmt;
use uuid::Uuid;

/// Unique identifier for a Tag.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(transparent)]
pub struct TagId(pub Uuid);

impl TagId {
    pub fn new() -> Self {
        Self(Uuid::now_v7())
    }
}

impl Default for TagId {
    fn default() -> Self {
        Self::new()
    }
}

impl fmt::Display for TagId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        self.0.fmt(f)
    }
}

impl std::str::FromStr for TagId {
    type Err = uuid::Error;
    fn from_str(s: &str) -> Result<Self, Self::Err> {
        Ok(Self(s.parse()?))
    }
}

/// Unique identifier for an Entry.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(transparent)]
pub struct EntryId(pub Uuid);

impl EntryId {
    pub fn new() -> Self {
        Self(Uuid::now_v7())
    }
}

impl Default for EntryId {
    fn default() -> Self {
        Self::new()
    }
}

impl fmt::Display for EntryId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        self.0.fmt(f)
    }
}

impl std::str::FromStr for EntryId {
    type Err = uuid::Error;
    fn from_str(s: &str) -> Result<Self, Self::Err> {
        Ok(Self(s.parse()?))
    }
}

/// A human-assigned label for categorizing web content.
///
/// Tags are flat (no hierarchy). Multiple tags with the same `name` can exist
/// — they are distinguished by `id`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Tag {
    pub id: TagId,
    pub name: String,
    pub description: String,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub aliases: Vec<String>,
}

impl Tag {
    pub fn new(name: impl Into<String>, description: impl Into<String>) -> Self {
        Self {
            id: TagId::new(),
            name: name.into(),
            description: description.into(),
            aliases: Vec::new(),
        }
    }

    pub fn with_aliases(
        name: impl Into<String>,
        description: impl Into<String>,
        aliases: Vec<String>,
    ) -> Self {
        let mut tag = Self::new(name, description);
        tag.aliases = aliases;
        tag
    }
}

/// An entry representing indexed content (bookmark URL or archived content).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Entry {
    pub id: EntryId,
    pub url: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub archiver: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub archive_path: Option<String>,
    #[serde(default)]
    pub description: String,
    pub tag_ids: Vec<TagId>,
}

impl Entry {
    pub fn new(
        url: impl Into<String>,
        archiver: Option<String>,
        archive_path: Option<String>,
        description: impl Into<String>,
        tag_ids: Vec<TagId>,
    ) -> Self {
        Self {
            id: EntryId::new(),
            url: url.into(),
            archiver,
            archive_path,
            description: description.into(),
            tag_ids,
        }
    }
}

