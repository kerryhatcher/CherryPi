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

    /// Main config file path
    pub config_file: PathBuf,
}

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
            config_file: config_dir.join("config.json"),
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

    /// Write the default config file if it doesn't exist
    pub fn ensure_default_config(&self) -> std::io::Result<bool> {
        if self.config_file.exists() {
            return Ok(false); // Already exists
        }

        let default_config = serde_json::json!({
            "provider": {
                "provider_type": "ollama",
                "endpoint": "http://localhost:11434",
                "api_key": null,
                "model": "llama3.2",
                "max_context_tokens": 8192,
                "max_output_tokens": 4096,
                "temperature": 0.7,
                "extra": {}
            },
            "agent": {
                "system_prompt": null,
                "max_turns": 50,
                "enabled_tools": [
                    "read", "write", "edit", "bash",
                    "grep", "find", "ls",
                    "web_search", "web_fetch"
                ],
                "working_dir": null
            },
            "plugins": {
                "enabled": true,
                "plugin_dirs": [],
                "install_dir": null,
                "install_bin_dir": null,
                "timeout_secs": 30
            },
            "ui": {
                "theme": "default",
                "show_line_numbers": true,
                "max_chat_history": 100
            }
        });

        let content = serde_json::to_string_pretty(&default_config)?;
        std::fs::write(&self.config_file, content)?;
        Ok(true) // Created new
    }
}
