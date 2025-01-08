use std::{collections::HashMap, fs};

use anyhow::Context;
use serde::{self, Deserialize, Serialize};

const SETTINGS_PATH: &str = "Settings.toml";

/// Holds the settings for the application converted from a TOML file.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default, rename_all = "kebab-case")]
pub struct Settings {
    /// Number of concurrent HTTP Requests to make
    pub concurrent_requests: usize,
    /// Whether files should be placed in a directory named after the server they have been pulled
    /// from.
    pub use_server_name_directories: bool,
    /// Mapping of server names to their respective [Instance] settings.
    pub servers: HashMap<String, Instance>,
}

impl Settings {
    pub fn load() -> anyhow::Result<Self> {
        let s = fs::read_to_string(SETTINGS_PATH)
            .with_context(|| format!("can't read file {}", SETTINGS_PATH))?;

        toml::from_str(&s)
            .with_context(|| format!("can't parse TOML content from {}", SETTINGS_PATH))
    }
}

impl Default for Settings {
    fn default() -> Self {
        Self {
            concurrent_requests: 5,
            use_server_name_directories: true,
            servers: HashMap::new(),
        }
    }
}

/// Holds the settings for a single instance of a server.
#[derive(Default, Debug, Clone, Serialize, Deserialize)]
#[serde(default, rename_all = "kebab-case")]
pub struct Instance {
    /// A URL string pointing to an RSS/Atom feed.
    pub url: String,

    /// Whether to download the full article, or just use the content provided in the feed.
    /// - `None` specifies to download the full article if the feed does not provide any content.
    /// - `Some(false)` specifies to never download the full article.
    /// - `Some(true)` specifies to always download the full article.
    pub download_full_article: Option<bool>,

    /// Whether to filter a full article down to a single element. This does
    /// not apply if [Instance::download_full_article] is `Some(false)`
    ///
    /// Example:
    /// ```html
    /// <!DOCTYPE html>
    /// <html lang="en">
    ///     <head></head>
    ///     <body>
    ///         <nav></nav>
    ///         <main>Main Content!</main>
    ///         <div id="footer"></footer>
    ///     </body>
    /// </html>
    /// ```
    /// becomes
    /// ```html
    /// <main>Main Content!</main>
    /// ```
    pub enable_filter: bool,

    /// A [CSS selector](https://www.w3schools.com/cssref/css_selectors.php)
    /// to filter down a full article to a single element.
    /// The default list of common selectors is used as fallback.
    /// Omit to only use the default list.
    /// This does not apply if [Instance::enable_filter] is `false`
    /// Example:
    /// ```toml
    /// filter-element = "#custom-article"
    /// ```
    /// turns
    /// ```html
    /// <!DOCTYPE html>
    /// <html lang="en">
    ///     <head></head>
    ///     <body>
    ///         <nav></nav>
    ///         <main>
    ///             <div id="#custom-article">Included!</div>
    ///             <div>Not included!</div>
    ///         </main>
    ///         <div id="footer"></footer>
    ///     </body>
    /// </html>
    /// ```
    /// becomes
    /// ```html
    /// <div id="#custom-article">Included!</div>
    /// ```
    pub filter_element: Option<String>,
}
