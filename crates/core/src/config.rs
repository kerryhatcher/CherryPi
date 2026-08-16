use serde::{Deserialize, Serialize};

/// Global configuration for CherryPi
#[derive(Debug, Clone, Serialize, Deserialize)]
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

impl Default for Config {
    fn default() -> Self {
        Self {
            provider: ProviderConfig::default(),
            agent: AgentConfig::default(),
            plugins: PluginConfig::default(),
            ui: UiConfig::default(),
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
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
            model: "llama3.2".to_string(),
            max_context_tokens: 8192,
            max_output_tokens: 4096,
            temperature: 0.7,
            extra: Default::default(),
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
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
}

impl Default for PluginConfig {
    fn default() -> Self {
        Self {
            enabled: true,
            plugin_dirs: vec![],
            install_dir: None,
            install_bin_dir: None,
            timeout_secs: 30,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
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
