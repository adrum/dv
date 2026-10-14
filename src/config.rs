//! `~/.dv/config.toml` — user preferences, plain text and hand-editable.

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};

use crate::paths;
use crate::releases::DEFAULT_INDEX_URL;

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct Config {
    /// Version used when nothing else pins one.
    pub default: Option<String>,
    /// Where `dv install` reads the .NET release index. Overridden by
    /// `DV_RELEASES_URL`; useful for a mirror.
    pub releases_url: Option<String>,
}

impl Config {
    pub fn load() -> Result<Self> {
        let path = paths::config_path()?;
        let raw = match std::fs::read_to_string(&path) {
            Ok(raw) => raw,
            Err(err) if err.kind() == std::io::ErrorKind::NotFound => return Ok(Self::default()),
            Err(err) => {
                return Err(err).with_context(|| format!("could not read {}", path.display()));
            }
        };
        toml::from_str(&raw).with_context(|| {
            format!(
                "{} is not valid dv config — fix it by hand or delete it to start over",
                path.display()
            )
        })
    }

    pub fn save(&self) -> Result<()> {
        let path = paths::config_path()?;
        paths::ensure_dir(&paths::home()?)?;
        let body = toml::to_string_pretty(self).context("could not serialize dv config")?;
        std::fs::write(&path, body).with_context(|| format!("could not write {}", path.display()))
    }

    pub fn releases_url(&self) -> String {
        if let Ok(url) = std::env::var("DV_RELEASES_URL")
            && !url.trim().is_empty()
        {
            return url;
        }
        self.releases_url
            .clone()
            .unwrap_or_else(|| DEFAULT_INDEX_URL.to_string())
    }
}
