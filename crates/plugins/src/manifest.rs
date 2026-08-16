use serde::{Deserialize, Serialize};
use std::path::PathBuf;

/// Plugin manifest as defined by the Open Plugin Standard v1.0.0
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PluginManifest {
    /// Unique plugin identifier (required)
    pub name: String,

    /// Semantic version
    #[serde(default)]
    pub version: Option<String>,

    /// Brief description
    #[serde(default)]
    pub description: Option<String>,

    /// Author information
    #[serde(default)]
    pub author: Option<Author>,

    /// Documentation URL
    #[serde(default)]
    pub homepage: Option<String>,

    /// Source repository URL
    #[serde(default)]
    pub repository: Option<String>,

    /// SPDX license identifier
    #[serde(default)]
    pub license: Option<String>,

    /// Logo path or URL
    #[serde(default)]
    pub logo: Option<String>,

    /// Search keywords
    #[serde(default)]
    pub keywords: Option<Vec<String>>,

    // Component path fields
    /// Additional command files or directories
    #[serde(default)]
    pub commands: Option<ComponentPaths>,

    /// Additional agent files or directories
    #[serde(default)]
    pub agents: Option<ComponentPaths>,

    /// Additional skill directories
    #[serde(default)]
    pub skills: Option<ComponentPaths>,

    /// Additional rule files or directories
    #[serde(default)]
    pub rules: Option<ComponentPaths>,

    /// Hook config paths or inline configuration
    #[serde(default)]
    pub hooks: Option<ComponentPaths>,

    /// MCP config paths or inline configuration
    #[serde(default)]
    pub mcp_servers: Option<ComponentPaths>,

    /// LSP config paths or inline configuration
    #[serde(default)]
    pub lsp_servers: Option<ComponentPaths>,

    /// Output style files or directories
    #[serde(default)]
    pub output_styles: Option<ComponentPaths>,

    /// Status bar configuration
    #[serde(default)]
    pub status_bar: Option<StatusBarConfig>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Author {
    #[serde(default)]
    pub name: Option<String>,
    #[serde(default)]
    pub email: Option<String>,
    #[serde(default)]
    pub url: Option<String>,
}

/// Status bar plugin configuration
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct StatusBarConfig {
    /// Shell command that outputs status bar text to stdout
    pub command: String,
    /// Refresh interval in milliseconds (default: 2000)
    #[serde(default = "default_refresh_ms")]
    pub refresh_ms: u64,
    /// Optional label shown before the output
    #[serde(default)]
    pub label: Option<String>,
    /// If true, pipe a JSON context object to stdin before reading stdout
    #[serde(default)]
    pub stdin_context: bool,
}

fn default_refresh_ms() -> u64 {
    2000
}

/// Component paths can be a string, array of strings, or an object with exclusive mode
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(untagged)]
pub enum ComponentPaths {
    Simple(String),
    Array(Vec<String>),
    Config {
        paths: Vec<String>,
        #[serde(default)]
        exclusive: bool,
    },
}

impl ComponentPaths {
    /// Get the list of paths
    pub fn paths(&self) -> Vec<String> {
        match self {
            ComponentPaths::Simple(s) => vec![s.clone()],
            ComponentPaths::Array(arr) => arr.clone(),
            ComponentPaths::Config { paths, .. } => paths.clone(),
        }
    }

    /// Whether this replaces default discovery
    pub fn is_exclusive(&self) -> bool {
        match self {
            ComponentPaths::Config { exclusive, .. } => *exclusive,
            _ => false,
        }
    }
}

/// Validates a plugin name according to the spec
pub fn validate_plugin_name(name: &str) -> Result<(), String> {
    if name.is_empty() || name.len() > 64 {
        return Err("Name must be 1-64 characters".into());
    }

    if !name
        .chars()
        .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '-' || c == '.')
    {
        return Err("Name must contain only lowercase alphanumeric, hyphens, and periods".into());
    }

    if !name.starts_with(|c: char| c.is_ascii_alphanumeric())
        || !name.ends_with(|c: char| c.is_ascii_alphanumeric())
    {
        return Err("Name must start and end with an alphanumeric character".into());
    }

    if name.contains("--") || name.contains("..") {
        return Err("Name must not contain consecutive hyphens or periods".into());
    }

    Ok(())
}

/// Possible manifest file locations to check
pub const MANIFEST_PATHS: &[&str] = &[
    ".cherrypi-plugin/plugin.json",
    ".plugin/plugin.json",
];

impl PluginManifest {
    /// Load a manifest from a plugin directory
    pub fn load(plugin_dir: &PathBuf) -> Option<Self> {
        for manifest_path in MANIFEST_PATHS {
            let full_path = plugin_dir.join(manifest_path);
            if full_path.exists() {
                if let Ok(content) = std::fs::read_to_string(&full_path) {
                    if let Ok(manifest) = serde_json::from_str::<PluginManifest>(&content) {
                        return Some(manifest);
                    }
                }
            }
        }
        None
    }

    /// Derive plugin name from directory if no manifest
    pub fn name_from_dir(plugin_dir: &PathBuf) -> String {
        plugin_dir
            .file_name()
            .and_then(|n| n.to_str())
            .unwrap_or("unknown")
            .to_lowercase()
            .replace(' ', "-")
    }
}
