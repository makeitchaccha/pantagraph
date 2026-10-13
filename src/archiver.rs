use std::collections::HashMap;
use std::fs;
use std::path::Path;
use std::process::Command;

use anyhow::{bail, Context, Result};
use serde::Deserialize;

#[derive(Debug, Clone, Deserialize, Default)]
pub struct ArchiverToolConfig {
    /// Command or path to executable (e.g. "single-file")
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
        let config: ToolConfig =
            toml::from_str(&content).with_context(|| format!("Failed to parse {}", path.display()))?;
        Ok(config)
    }

    /// Run archiver for a given url and output path.
    pub fn execute_archiver(
        &self,
        archiver_name: &str,
        url: &str,
        output_path: &Path,
    ) -> Result<()> {
        let config = self.archiver.get(archiver_name).cloned().unwrap_or_default();

        let binary = config
            .command
            .unwrap_or_else(|| archiver_name.to_string());

        let mut cmd = Command::new(&binary);

        for (k, v) in &config.env {
            cmd.env(k, v);
        }

        let output_str = output_path.to_string_lossy().to_string();

        if config.args.is_empty() {
            // Default arguments if not specified in tool.toml
            if archiver_name == "single-file" {
                // If google-chrome / chromium executable exists, pass it if needed, or rely on PATH
                cmd.arg(url).arg(&output_str);
            } else {
                cmd.arg(url).arg(&output_str);
            }
        } else {
            // If user defined custom args with placeholders `{url}` and `{output}`
            let mut has_url = false;
            let mut has_output = false;
            for arg in &config.args {
                let replaced = arg
                    .replace("{url}", url)
                    .replace("{output}", &output_str);
                if arg.contains("{url}") {
                    has_url = true;
                }
                if arg.contains("{output}") {
                    has_output = true;
                }
                cmd.arg(replaced);
            }
            // If placeholders were not used, append url and output at the end
            if !has_url {
                cmd.arg(url);
            }
            if !has_output {
                cmd.arg(&output_str);
            }
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

        if !output_path.exists() {
            bail!(
                "Archiver '{}' completed successfully but output file was not found at {}",
                archiver_name,
                output_path.display()
            );
        }

        Ok(())
    }
}
