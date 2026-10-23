//! Optional, portable per-user configuration. Environment and CLI win.

use std::path::{Path, PathBuf};

use serde::Deserialize;

use crate::error::{Error, Result};
use crate::paths::lexical_abs;

#[derive(Debug, Default, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Config {
    pub library: Option<PathBuf>,
    pub remote: Option<String>,
    pub sync_command: Option<PathBuf>,
    pub claude_home: Option<PathBuf>,
    pub codex_home: Option<PathBuf>,
    pub agents_dir: Option<PathBuf>,
    pub pi_skills: Option<PathBuf>,
    pub cursor_skills: Option<PathBuf>,
}

impl Config {
    pub fn load(home: &Path) -> Result<Self> {
        let explicit = std::env::var_os("SKILLHERD_CONFIG").filter(|s| !s.is_empty());
        let path = explicit.clone().map(PathBuf::from).unwrap_or_else(|| {
            std::env::var_os("XDG_CONFIG_HOME")
                .filter(|s| !s.is_empty())
                .map(PathBuf::from)
                .unwrap_or_else(|| home.join(".config"))
                .join("skillherd/config.json")
        });
        let text = match std::fs::read_to_string(&path) {
            Ok(text) => text,
            Err(err) if err.kind() == std::io::ErrorKind::NotFound && explicit.is_none() => {
                return Ok(Self::default());
            }
            Err(err) => {
                return Err(Error::config_error(format!(
                    "reading {}: {err}",
                    path.display()
                )));
            }
        };
        serde_json::from_str(&text)
            .map_err(|err| Error::config_error(format!("parsing {}: {err}", path.display())))
    }
}

/// Config paths are home-relative (not checkout-relative); `~/` is supported.
pub fn config_path(home: &Path, path: &Path) -> PathBuf {
    if path == Path::new("~") {
        home.to_path_buf()
    } else if let Ok(rest) = path.strip_prefix("~/") {
        crate::paths::normalize(&home.join(rest))
    } else {
        lexical_abs(home, path)
    }
}
