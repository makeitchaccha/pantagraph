use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use std::fs;
use std::path::Path;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "snake_case")]
pub enum DuplicatePolicy {
    #[default]
    Allow,
    Warn,
    Error,
}

#[derive(Debug, Clone, Deserialize)]
pub struct TagConfig {
    #[serde(default)]
    pub same_name_policy: DuplicatePolicy,
    #[serde(default = "default_same_qualified_ref_policy")]
    pub same_qualified_ref_policy: DuplicatePolicy,
}

fn default_same_qualified_ref_policy() -> DuplicatePolicy {
    DuplicatePolicy::Error
}

impl Default for TagConfig {
    fn default() -> Self {
        Self {
            same_name_policy: DuplicatePolicy::Allow,
            same_qualified_ref_policy: DuplicatePolicy::Error,
        }
    }
}

#[derive(Debug, Clone, Deserialize, Default)]
pub struct Config {
    #[serde(default)]
    pub tag: TagConfig,
}

impl Config {
    /// Load `config.toml` from the specified directory or current working directory fallback.
    pub fn load_from_dir(dir: &Path) -> Result<Self> {
        let path = dir.join("config.toml");
        if path.exists() {
            return Self::load_from_file(&path);
        }
        let cwd_path = Path::new("config.toml");
        if cwd_path.exists() {
            return Self::load_from_file(cwd_path);
        }
        Ok(Self::default())
    }

    pub fn load_from_file(path: &Path) -> Result<Self> {
        let content = fs::read_to_string(path)
            .with_context(|| format!("Failed to read {}", path.display()))?;
        let config: Config = toml::from_str(&content)
            .with_context(|| format!("Failed to parse {}", path.display()))?;
        Ok(config)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn defaults_allow_duplicate_names_and_error_on_duplicate_qualified_refs() {
        let cfg = Config::default();
        assert_eq!(cfg.tag.same_name_policy, DuplicatePolicy::Allow);
        assert_eq!(cfg.tag.same_qualified_ref_policy, DuplicatePolicy::Error);
    }

    #[test]
    fn parses_duplicate_policies() {
        let cfg: Config =
            toml::from_str("[tag]\nsame_name_policy = 'warn'\nsame_qualified_ref_policy = 'allow'")
                .unwrap();
        assert_eq!(cfg.tag.same_name_policy, DuplicatePolicy::Warn);
        assert_eq!(cfg.tag.same_qualified_ref_policy, DuplicatePolicy::Allow);
    }
}
