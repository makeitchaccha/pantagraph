use std::collections::HashMap;
use std::fs;
use std::path::Path;
use std::process::Command;

use anyhow::{bail, Context, Result};
use serde::Deserialize;

#[derive(Debug, Clone, Deserialize, Default)]
pub struct ArchiverToolConfig {
    /// Command or path to executable
    pub command: Option<String>,
    /// Arguments template or fixed list of args
    #[serde(default)]
    pub args: Vec<String>,
    /// Environment variables to pass to the process
    #[serde(default)]
    pub env: HashMap<String, String>,
}

#[derive(Debug, Clone, Deserialize, Default)]
pub struct ToolConfig {
    #[serde(default)]
    pub archiver: HashMap<String, ArchiverToolConfig>,
}

impl ToolConfig {
    /// Load `tool.toml` from the specified config path or data directory.
    pub fn load_from_dir(dir: &Path) -> Result<Self> {
        let path = dir.join("tool.toml");
        if !path.exists() {
            // Also check current working directory as fallback
            let cwd_path = Path::new("tool.toml");
            if cwd_path.exists() {
                return Self::load_from_file(cwd_path);
            }
            return Ok(Self::default());
        }
        Self::load_from_file(&path)
    }

    pub fn load_from_file(path: &Path) -> Result<Self> {
        let content = fs::read_to_string(path)
            .with_context(|| format!("Failed to read {}", path.display()))?;
        let config: ToolConfig = toml::from_str(&content)
            .with_context(|| format!("Failed to parse {}", path.display()))?;
        Ok(config)
    }

    /// Run archiver for a given url and output directory.
    pub fn execute_archiver(&self, archiver_name: &str, url: &str, outdir: &Path) -> Result<()> {
        let config = self.archiver.get(archiver_name).ok_or_else(|| {
            anyhow::anyhow!("Archiver '{}' not configured in tool.toml", archiver_name)
        })?;

        let binary = config.command.as_deref().unwrap_or(archiver_name);

        if config.args.is_empty() {
            bail!(
                "Archiver '{}' has no args specified in tool.toml. Arguments must be explicitly specified.",
                archiver_name
            );
        }

        let mut cmd = Command::new(binary);

        for (k, v) in &config.env {
            cmd.env(k, v);
        }

        let outdir_str = outdir.to_string_lossy().to_string();

        for arg in &config.args {
            let replaced = arg
                .replace("{url}", url)
                .replace("{outdir}", &outdir_str)
                // keep {output} as synonym for {outdir} for flexibility
                .replace("{output}", &outdir_str);
            cmd.arg(replaced);
        }

        let status = cmd
            .status()
            .with_context(|| format!("Failed to execute archiver command '{binary}'"))?;

        if !status.success() {
            bail!(
                "Archiver '{}' failed with exit status {}",
                archiver_name,
                status
            );
        }

        Ok(())
    }
}
