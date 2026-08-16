use std::path::PathBuf;
use std::sync::Arc;
use std::sync::atomic::AtomicBool;
use std::sync::Mutex;

use cherrypi_core::config::Config;
use cherrypi_core::event::ToolCallEvent;
use cherrypi_core::message::Conversation;
use cherrypi_core::tool::ToolRegistry;
use cherrypi_llm::provider::Provider;
use cherrypi_plugins::loader::PluginLoader;
use cherrypi_plugins::hooks::HookEngine;
use cherrypi_plugins::mcp::McpManager;

use fff_search::{SharedFilePicker, SharedFrecency};
use tokio::sync::mpsc::UnboundedSender;

/// Shared context for the agent
pub struct AgentContext {
    /// Global configuration
    pub config: Config,
    /// Current conversation
    pub conversation: Conversation,
    /// Tool registry
    pub tool_registry: ToolRegistry,
    /// LLM provider
    pub provider: Arc<dyn Provider>,
    /// Plugin loader
    pub plugin_loader: PluginLoader,
    /// Hook engine
    pub hook_engine: HookEngine,
    /// MCP manager
    pub mcp_manager: McpManager,
    /// Working directory
    pub working_dir: PathBuf,
    /// FFF file-search index (long-lived, resident). `None` when FFF failed
    /// to initialise for the working directory, in which case the agent falls
    /// back to subprocess `grep`/`find` tools.
    pub fff_picker: Option<SharedFilePicker>,
    /// FFF frecency database handle. `None` when FFF is disabled.
    pub fff_frecency: Option<SharedFrecency>,
    /// Shared cancellation flag. Set by the TUI to abort in-flight LLM
    /// requests and FFF grep searches that accept an abort signal.
    pub cancel_flag: Arc<AtomicBool>,
    /// Extra binary directories to prepend to PATH when executing bash commands
    pub extra_bin_paths: Arc<Mutex<Vec<PathBuf>>>,

    /// Channel sender for streaming tool call events to the TUI
    pub tool_call_tx: UnboundedSender<ToolCallEvent>,
}

impl AgentContext {
    pub fn new(
        config: Config,
        provider: Arc<dyn Provider>,
        tool_call_tx: UnboundedSender<ToolCallEvent>,
    ) -> Self {
        let working_dir = config
            .agent
            .working_dir
            .as_ref()
            .map(PathBuf::from)
            .unwrap_or_else(|| std::env::current_dir().unwrap_or_else(|_| PathBuf::from(".")));

        let plugin_loader = PluginLoader::new();
        let hook_engine = HookEngine::new();
        let mcp_manager = McpManager::new();

        // Initialise the FFF file-search index. Best-effort: if it fails
        // (e.g. working dir missing, fs root refused) we log and proceed with
        // `None`, which causes `BuiltinTools::register_all` to register the
        // subprocess `grep`/`find` fallbacks instead of the FFF tools.
        let (fff_picker, fff_frecency) = super::tools::init_fff(&working_dir);

        Self {
            extra_bin_paths: Arc::new(Mutex::new(Vec::new())),
            config,
            conversation: Conversation::new(),
            tool_registry: ToolRegistry::new(),
            provider,
            plugin_loader,
            hook_engine,
            mcp_manager,
            working_dir,
            fff_picker,
            fff_frecency,
            cancel_flag: Arc::new(AtomicBool::new(false)),
            tool_call_tx,
        }
    }
}
