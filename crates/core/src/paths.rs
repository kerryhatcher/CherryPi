use std::path::PathBuf;

/// Platform-standard directory paths for CherryPi.
///
/// Follows the XDG Base Directory specification on Linux,
/// Standard Directories guidelines on macOS, and
/// Known Folder conventions on Windows.
#[derive(Debug, Clone)]
pub struct CherryPiPaths {
    /// Configuration directory
    /// - Linux:   `~/.config/cherrypi/`
    /// - macOS:   `~/Library/Application Support/cherrypi/`
    /// - Windows: `%APPDATA%/cherrypi/`
    pub config_dir: PathBuf,

    /// Data directory (plugins, cache, etc.)
    /// - Linux:   `~/.local/share/cherrypi/`
    /// - macOS:   `~/Library/Application Support/cherrypi/`
    /// - Windows: `%APPDATA%/cherrypi/`
    pub data_dir: PathBuf,

    /// Plugin install directory
    pub plugins_dir: PathBuf,

    /// Conversation history directory
    pub history_dir: PathBuf,

    /// Log directory
    pub log_dir: PathBuf,

    /// Main config file path (TOML)
    pub config_file: PathBuf,

    /// Legacy JSON config path, read only when `config_file` does not exist
    pub legacy_config_file: PathBuf,
}

/// Default user config, written on first run. TOML has no null, so optional
/// settings are shown as comments.
const DEFAULT_CONFIG_TOML: &str = r#"# CherryPi configuration.
# Lookup order: --config <path>, ./.cherrypi.toml, then this file.

[provider]
# "ollama", "openai", "anthropic" or "openrouter"
provider_type = "ollama"
endpoint = "http://localhost:11434"
# api_key = "..."  # openrouter also reads $OPENROUTER_API_KEY when this is unset
model = "deepseek-v4-flash:cloud"
max_context_tokens = 8192
max_output_tokens = 4096
temperature = 0.7

[agent]
max_turns = 50
enabled_tools = ["read", "write", "edit", "bash", "grep", "find", "ls", "git", "web_search", "web_fetch"]
# system_prompt = "..."
# working_dir = "/path/to/project"
# User-Agent for web_search, web_fetch and the page renderer. Some sites hang
# requests from desktop Linux user agents, hence the Windows default.
user_agent = "Mozilla/5.0 (Windows NT 10.0; Win64; x64; rv:160.0) Gecko/20100101 Firefox/160.0"

[plugins]
enabled = true
plugin_dirs = []
timeout_secs = 30
trusted_plugins = []
# install_dir = "..."
# install_bin_dir = "..."

[ui]
theme = "default"
show_line_numbers = true
max_chat_history = 100
"#;

impl CherryPiPaths {
    /// Create all directories and return the paths.
    /// Directories are created if they don't exist.
    pub fn initialize() -> std::io::Result<Self> {
        let config_dir = dirs::config_dir()
            .unwrap_or_else(|| PathBuf::from("."))
            .join("cherrypi");

        let data_dir = dirs::data_dir()
            .unwrap_or_else(|| PathBuf::from("."))
            .join("cherrypi");

        let paths = Self {
            plugins_dir: data_dir.join("plugins"),
            history_dir: data_dir.join("history"),
            log_dir: data_dir.join("logs"),
            config_file: config_dir.join("config.toml"),
            legacy_config_file: config_dir.join("config.json"),
            config_dir,
            data_dir,
        };

        paths.create_dirs()?;
        Ok(paths)
    }

    /// Create all required directories
    fn create_dirs(&self) -> std::io::Result<()> {
        std::fs::create_dir_all(&self.config_dir)?;
        std::fs::create_dir_all(&self.data_dir)?;
        std::fs::create_dir_all(&self.plugins_dir)?;
        std::fs::create_dir_all(&self.history_dir)?;
        std::fs::create_dir_all(&self.log_dir)?;
        Ok(())
    }

    /// Write the default config file if no config exists (TOML or legacy JSON)
    pub fn ensure_default_config(&self) -> std::io::Result<bool> {
        if self.config_file.exists() || self.legacy_config_file.exists() {
            return Ok(false); // Already configured
        }

        std::fs::write(&self.config_file, DEFAULT_CONFIG_TOML)?;
        Ok(true) // Created new
    }
}
