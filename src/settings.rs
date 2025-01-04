use std::{collections::HashMap, fs};

use anyhow::Context;
use serde::{self, Deserialize, Serialize};

const SETTINGS_PATH: &str = "Settings.toml";

/// Holds the settings for the application converted from a TOML file.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default, rename_all = "kebab-case")]
pub struct Settings {
    /// Mapping of server names to their respective [Instance] settings.
    pub servers: HashMap<String, Instance>,
    /// Whether files should be placed in a directory named after the server they have been pulled
    /// from.
    pub use_server_name_directories: bool,
}

impl Settings {
    pub fn load() -> anyhow::Result<Self> {
        let s = fs::read_to_string(SETTINGS_PATH)
            .with_context(|| format!("can't read file {}", SETTINGS_PATH))?;

        toml::from_str(&s)
            .with_context(|| format!("can't parse TOML content from {}", SETTINGS_PATH))
            .map_err(Into::into)
    }
}

impl Default for Settings {
    fn default() -> Self {
        Self {
            servers: HashMap::new(),
            use_server_name_directories: true,
        }
    }
}

/// Holds the settings for a single instance of a server.
#[derive(Default, Debug, Clone, Serialize, Deserialize)]
#[serde(default, rename_all = "kebab-case")]
pub struct Instance {
    /// A URL string pointing to an RSS/Atom feed.
    pub url: String,
}
