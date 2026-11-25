use std::path::Path;

use serde::{Deserialize, Serialize};

/// Global configuration for CherryPi
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
#[serde(default)]
pub struct Config {
    /// LLM provider configuration
    pub provider: ProviderConfig,

    /// Agent behavior configuration
    pub agent: AgentConfig,

    /// Plugin system configuration
    pub plugins: PluginConfig,

    /// TUI appearance configuration
    pub ui: UiConfig,
}

impl Config {
    /// Load a config file. Files ending in `.json` are read as JSON (the legacy
    /// format); any other file is read as TOML.
    pub fn load_file(path: &Path) -> Result<Self, String> {
        let content = std::fs::read_to_string(path)
            .map_err(|e| format!("cannot read {}: {e}", path.display()))?;
        let parsed = if path.extension().is_some_and(|ext| ext == "json") {
            serde_json::from_str::<Self>(&content).map_err(|e| e.to_string())
        } else {
            toml::from_str::<Self>(&content).map_err(|e| e.to_string())
        };
        parsed.map_err(|e| format!("invalid config {}: {e}", path.display()))
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct ProviderConfig {
    /// Provider type: "ollama", "openai", "anthropic", "openrouter"
    pub provider_type: String,

    /// API endpoint URL
    pub endpoint: String,

    /// API key (if required)
    pub api_key: Option<String>,

    /// Default model name
    pub model: String,

    /// Maximum context window tokens
    pub max_context_tokens: usize,

    /// Maximum output tokens
    pub max_output_tokens: usize,

    /// Temperature for generation
    pub temperature: f32,

    /// Extra provider-specific parameters
    #[serde(default)]
    pub extra: serde_json::Map<String, serde_json::Value>,
}

impl Default for ProviderConfig {
    fn default() -> Self {
        Self {
            provider_type: "ollama".to_string(),
            endpoint: "http://localhost:11434".to_string(),
            api_key: None,
            model: "deepseek-v4-flash:cloud".to_string(),
            max_context_tokens: 8192,
            max_output_tokens: 4096,
            temperature: 0.7,
            extra: Default::default(),
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct AgentConfig {
    /// System prompt for the agent (None = use built-in default)
    #[serde(default)]
    pub system_prompt: Option<String>,

    /// Maximum conversation turns
    pub max_turns: usize,

    /// Tools enabled by default
    #[serde(default)]
    pub enabled_tools: Vec<String>,

    /// Working directory
    pub working_dir: Option<String>,
}

impl Default for AgentConfig {
    fn default() -> Self {
        Self {
            system_prompt: None,
            max_turns: 50,
            enabled_tools: vec![
                "read".into(),
                "write".into(),
                "edit".into(),
                "bash".into(),
                "grep".into(),
                "find".into(),
                "web_search".into(),
                "web_fetch".into(),
            ],
            working_dir: None,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct PluginConfig {
    /// Whether plugins are enabled
    pub enabled: bool,

    /// Plugin directories to scan
    #[serde(default)]
    pub plugin_dirs: Vec<String>,

    /// Installed plugins directory
    pub install_dir: Option<String>,

    /// Cargo binary directory for installed crates
    #[serde(default)]
    pub install_bin_dir: Option<String>,

    /// Maximum plugin execution timeout in seconds
    pub timeout_secs: u64,

    /// Plugin identifiers explicitly permitted to execute commands/processes.
    #[serde(default)]
    pub trusted_plugins: Vec<String>,
}

impl Default for PluginConfig {
    fn default() -> Self {
        Self {
            enabled: true,
            plugin_dirs: vec![],
            install_dir: None,
            install_bin_dir: None,
            timeout_secs: 30,
            trusted_plugins: vec![],
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct UiConfig {
    /// Color theme
    pub theme: String,

    /// Show line numbers in file viewer
    pub show_line_numbers: bool,

    /// Maximum visible chat history
    pub max_chat_history: usize,
}

impl Default for UiConfig {
    fn default() -> Self {
        Self {
            theme: "default".to_string(),
            show_line_numbers: true,
            max_chat_history: 100,
        }
    }
}

#[cfg(test)]
mod load_tests {
    use super::*;

    fn scratch(name: &str) -> std::path::PathBuf {
        let dir = std::env::temp_dir().join(format!("cherrypi-config-{name}-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    #[test]
    fn loads_toml_and_fills_missing_sections_with_defaults() {
        let path = scratch("toml").join("config.toml");
        std::fs::write(
            &path,
            "[provider]\nprovider_type = \"openrouter\"\nmodel = \"openrouter/free\"\n",
        )
        .unwrap();
        let cfg = Config::load_file(&path).unwrap();
        assert_eq!(cfg.provider.provider_type, "openrouter");
        assert_eq!(cfg.provider.model, "openrouter/free");
        assert_eq!(cfg.agent.max_turns, AgentConfig::default().max_turns);
    }

    #[test]
    fn loads_legacy_json_when_named_json() {
        let path = scratch("json").join("config.json");
        std::fs::write(&path, r#"{"provider":{"provider_type":"ollama","model":"llama3.2"}}"#).unwrap();
        let cfg = Config::load_file(&path).unwrap();
        assert_eq!(cfg.provider.model, "llama3.2");
    }

    #[test]
    fn reports_invalid_toml_with_the_file_name() {
        let path = scratch("bad").join("config.toml");
        std::fs::write(&path, "[provider\nmodel = ").unwrap();
        let err = Config::load_file(&path).unwrap_err();
        assert!(err.contains("config.toml"), "{err}");
    }
}
