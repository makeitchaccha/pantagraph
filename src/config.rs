use std::fs;
use std::path::Path;
use anyhow::{Context, Result};
use clap::ValueEnum;
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, ValueEnum, Default)]
#[serde(rename_all = "snake_case")]
pub enum TagConflictPolicy {
    /// Disallow conflict: abort immediately when conflict is detected.
    Abort,
    /// Warn on conflict: print warning to stderr and continue creating the tag.
    #[default]
    #[value(name = "warn-only", alias = "warn_only")]
    WarnOnly,
    /// Completely ignore conflict without warnings.
    Ignore,
}

#[derive(Debug, Clone, Deserialize, Default)]
pub struct TagConfig {
    #[serde(default)]
    pub conflict_resolution: TagConflictPolicy,
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
        let config: Config =
            toml::from_str(&content).with_context(|| format!("Failed to parse {}", path.display()))?;
        Ok(config)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_parse_config() {
        let toml_str = r#"
        [tag]
        conflict_resolution = "abort"
        "#;
        let cfg: Config = toml::from_str(toml_str).unwrap();
        assert_eq!(cfg.tag.conflict_resolution, TagConflictPolicy::Abort);

        let toml_str2 = r#"
        [tag]
        conflict_resolution = "warn_only"
        "#;
        let cfg2: Config = toml::from_str(toml_str2).unwrap();
        assert_eq!(cfg2.tag.conflict_resolution, TagConflictPolicy::WarnOnly);

        let toml_str3 = r#"
        [tag]
        conflict_resolution = "ignore"
        "#;
        let cfg3: Config = toml::from_str(toml_str3).unwrap();
        assert_eq!(cfg3.tag.conflict_resolution, TagConflictPolicy::Ignore);
    }
}
