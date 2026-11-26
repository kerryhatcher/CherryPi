use std::path::PathBuf;
use std::sync::Arc;
use std::sync::atomic::AtomicBool;
use std::sync::Mutex;
use std::time::Duration;

use async_trait::async_trait;
use cherrypi_core::tool::{Tool, ToolHandler, ToolRegistry, ToolResult};
use fff_search::{FilePicker, FilePickerOptions, FFFMode, SharedFilePicker, SharedFrecency};
use tracing::{info, warn};

use crate::sandbox::Sandbox;
use crate::web;

const MAX_FILE_BYTES: u64 = 1_048_576;
const MAX_TOOL_OUTPUT_BYTES: usize = 65_536;

/// Resolves a tool path (relative or absolute) and rejects `..` components and
/// any path that resolves outside the workspace, including via symlinks.
/// Agent supplied paths are untrusted input.
fn resolve_workspace_path(working_dir: &PathBuf, path: &str) -> Result<PathBuf, String> {
    let relative = std::path::Path::new(path);
    if path.is_empty()
        || relative.components().any(|part| matches!(part, std::path::Component::ParentDir))
    {
        return Err("path must be non-empty and must not contain '..'".into());
    }

    let root = std::fs::canonicalize(working_dir)
        .map_err(|e| format!("cannot resolve workspace: {e}"))?;
    let candidate = root.join(relative);
    let existing = if candidate.exists() {
        std::fs::canonicalize(&candidate).map_err(|e| format!("cannot resolve path: {e}"))?
    } else {
        // A write may create several new directories. Validate the nearest
        // existing ancestor so an existing symlink cannot redirect creation.
        let mut ancestor = candidate.as_path();
        while !ancestor.exists() {
            ancestor = ancestor.parent().ok_or("path has no parent")?;
        }
        std::fs::canonicalize(ancestor)
            .map_err(|e| format!("cannot resolve parent directory: {e}"))?
    };
    if !existing.starts_with(&root) {
        return Err("path escapes the configured workspace".into());
    }
    Ok(candidate)
}

fn truncate_output(mut output: String) -> String {
    if output.len() > MAX_TOOL_OUTPUT_BYTES {
        output.truncate(MAX_TOOL_OUTPUT_BYTES);
        output.push_str("\n[truncated by CherryPi]");
    }
    output
}

/// Initialise the FFF file-search index for `working_dir`.
///
/// Returns `(None, None)` if the working directory cannot be indexed (missing,
/// fs root, home dir guard) so the caller can fall back to subprocess tools.
pub fn init_fff(working_dir: &PathBuf) -> (Option<SharedFilePicker>, Option<SharedFrecency>) {
    let base_path = match working_dir.to_str() {
        Some(s) if !s.is_empty() => s.to_string(),
        _ => {
            warn!("FFF: working_dir has no UTF-8 string form; skipping init");
            return (None, None);
        }
    };

    let picker = SharedFilePicker::default();
    let frecency = SharedFrecency::default();

    match FilePicker::new_with_shared_state(
        picker.clone(),
        frecency.clone(),
        FilePickerOptions {
            base_path,
            enable_mmap_cache: true,
            enable_content_indexing: true,
            watch: true,
            mode: FFFMode::Ai,
            ..Default::default()
        },
    ) {
        Ok(()) => {
            // Block briefly for the initial scan so the first agent query sees
            // real data instead of an empty index. Cap at 10s; the watcher
            // keeps the index warm thereafter.
            let ready = picker.wait_for_scan(Duration::from_secs(10));
            if !ready {
                warn!("FFF: initial scan did not complete within 10s; index may be partial");
            }
            info!("FFF file-search index initialised for {:?}", working_dir);
            (Some(picker), Some(frecency))
        }
        Err(e) => {
            warn!("FFF: failed to init picker for {:?}: {}", working_dir, e);
            (None, None)
        }
    }
}

/// Creates and registers all built-in tools
pub struct BuiltinTools;

impl BuiltinTools {
    /// Register all built-in tools into the registry.
    ///
    /// When `fff_picker` is `Some`, the FFF-backed `grep`, `find`, and
    /// `multi_grep` tools are registered (long-lived index, fuzzy fallback,
    /// git-aware, paginated). When `None`, the subprocess `grep`/`find`
    /// fallbacks are registered so the agent always has working search.
    ///
    /// `extra_bin_paths` — additional directories to prepend to `$PATH`
    /// when executing bash commands (e.g. CherryPi's private cargo bin dir).
    ///
    /// `user_agent` — the User-Agent for the web tools and page renderer.
    pub fn register_all(
        registry: &mut ToolRegistry,
        working_dir: PathBuf,
        fff_picker: Option<SharedFilePicker>,
        cancel_flag: Arc<AtomicBool>,
        extra_bin_paths: Option<Arc<Mutex<Vec<PathBuf>>>>,
        enabled_tools: &[String],
        user_agent: &str,
    ) {
        web::set_user_agent(user_agent);
        #[cfg(feature = "render")]
        if let Err(e) = cherrypi_fetch::configure(cherrypi_fetch::RendererConfig {
            user_agent: web::user_agent().to_string(),
        }) {
            warn!("Page renderer keeps its earlier settings: {}", e);
        }
        let enabled = |name: &str| enabled_tools.iter().any(|tool| tool == name);
        if enabled("read") { registry.register(Arc::new(ReadFileTool::new(working_dir.clone()))); }
        if enabled("write") { registry.register(Arc::new(WriteFileTool::new(working_dir.clone()))); }
        if enabled("edit") { registry.register(Arc::new(EditFileTool::new(working_dir.clone()))); }
        if enabled("bash") { registry.register(Arc::new(BashTool::new(working_dir.clone(), extra_bin_paths))); }
        if enabled("ls") { registry.register(Arc::new(ListDirTool::new(working_dir.clone()))); }
        if enabled("web_search") || enabled("web_fetch") {
            match web::client() {
                Ok(client) => {
                    if enabled("web_search") { registry.register(Arc::new(WebSearchTool { client: client.clone() })); }
                    if enabled("web_fetch") { registry.register(Arc::new(WebFetchTool { client })); }
                }
                Err(e) => warn!("Web tools disabled: {}", e),
            }
        }
        if enabled("git") { registry.register(Arc::new(crate::git_tool::GitTool::new(working_dir.clone()))); }

        match fff_picker {
            Some(picker) if enabled("grep") || enabled("find") => {
                use crate::fff_tools::{FffFindTool, FffGrepTool, FffMultiGrepTool};
                if enabled("find") { registry.register(Arc::new(FffFindTool::new(picker.clone(), working_dir.clone()))); }
                if enabled("grep") {
                    registry.register(Arc::new(FffGrepTool::new(picker.clone(), working_dir.clone(), cancel_flag)));
                    registry.register(Arc::new(FffMultiGrepTool::new(picker.clone(), working_dir.clone())));
                }
            }
            _ => {
                info!("FFF picker unavailable; registering subprocess grep/find fallbacks");
                if enabled("grep") { registry.register(Arc::new(GrepTool::new(working_dir.clone()))); }
                if enabled("find") { registry.register(Arc::new(FindTool::new(working_dir.clone()))); }
            }
        }
    }
}

// --- Read File Tool ---

struct ReadFileTool {
    working_dir: PathBuf,
}

impl ReadFileTool {
    fn new(working_dir: PathBuf) -> Self {
        Self { working_dir }
    }
}

#[async_trait]
impl ToolHandler for ReadFileTool {
    fn definition(&self) -> Tool {
        Tool {
            name: "read".into(),
            description: "Read the contents of a file. Supports text files and images.".into(),
            parameters: serde_json::json!({
                "type": "object",
                "properties": {
                    "path": {
                        "type": "string",
                        "description": "Path to the file to read (relative or absolute)"
                    },
                    "offset": {
                        "type": "number",
                        "description": "Line number to start reading from (1-indexed)"
                    },
                    "limit": {
                        "type": "number",
                        "description": "Maximum number of lines to read"
                    }
                },
                "required": ["path"]
            }),
        }
    }

    async fn execute(&self, arguments: serde_json::Value) -> ToolResult {
        let path = arguments["path"].as_str().unwrap_or("");
        let offset = arguments["offset"].as_u64().map(|o| o as usize).unwrap_or(1);
        let limit = arguments["limit"].as_u64().map(|l| l as usize);

        let full_path = match resolve_workspace_path(&self.working_dir, path) {
            Ok(path) => path,
            Err(e) => return ToolResult { tool_call_id: String::new(), content: e, is_error: true },
        };

        if std::fs::metadata(&full_path).map(|m| m.len() > MAX_FILE_BYTES).unwrap_or(false) {
            return ToolResult { tool_call_id: String::new(), content: "Refusing to read files larger than 1 MiB".into(), is_error: true };
        }

        match std::fs::read_to_string(&full_path) {
            Ok(content) => {
                let lines: Vec<&str> = content.lines().collect();
                let start = (offset - 1).min(lines.len());
                let end = limit.map(|l| (start + l).min(lines.len())).unwrap_or(lines.len());
                let output = lines[start..end].join("\n");

                ToolResult {
                    tool_call_id: String::new(),
                    content: output,
                    is_error: false,
                }
            }
            Err(e) => ToolResult {
                tool_call_id: String::new(),
                content: format!("Error reading file: {}", e),
                is_error: true,
            },
        }
    }
}

// --- Write File Tool ---

struct WriteFileTool {
    working_dir: PathBuf,
}

impl WriteFileTool {
    fn new(working_dir: PathBuf) -> Self {
        Self { working_dir }
    }
}

#[async_trait]
impl ToolHandler for WriteFileTool {
    fn definition(&self) -> Tool {
        Tool {
            name: "write".into(),
            description: "Write content to a file. Creates the file if it doesn't exist, overwrites if it does.".into(),
            parameters: serde_json::json!({
                "type": "object",
                "properties": {
                    "path": {
                        "type": "string",
                        "description": "Path to the file to write (relative or absolute)"
                    },
                    "content": {
                        "type": "string",
                        "description": "Content to write to the file"
                    }
                },
                "required": ["path", "content"]
            }),
        }
    }

    async fn execute(&self, arguments: serde_json::Value) -> ToolResult {
        let path = arguments["path"].as_str().unwrap_or("");
        let content = arguments["content"].as_str().unwrap_or("");

        let full_path = match resolve_workspace_path(&self.working_dir, path) {
            Ok(path) => path,
            Err(e) => return ToolResult { tool_call_id: String::new(), content: e, is_error: true },
        };

        // Create parent directories if needed
        if let Some(parent) = full_path.parent() {
            if let Err(e) = std::fs::create_dir_all(parent) {
                return ToolResult {
                    tool_call_id: String::new(),
                    content: format!("Error creating directories: {}", e),
                    is_error: true,
                };
            }
        }

        match std::fs::write(&full_path, content) {
            Ok(_) => ToolResult {
                tool_call_id: String::new(),
                content: format!("Successfully wrote {} bytes to {}", content.len(), path),
                is_error: false,
            },
            Err(e) => ToolResult {
                tool_call_id: String::new(),
                content: format!("Error writing file: {}", e),
                is_error: true,
            },
        }
    }
}

// --- Edit File Tool ---

struct EditFileTool {
    working_dir: PathBuf,
}

impl EditFileTool {
    fn new(working_dir: PathBuf) -> Self {
        Self { working_dir }
    }
}

#[async_trait]
impl ToolHandler for EditFileTool {
    fn definition(&self) -> Tool {
        Tool {
            name: "edit".into(),
            description: "Edit a file using exact text replacement. Each edit's oldText must match exactly.".into(),
            parameters: serde_json::json!({
                "type": "object",
                "properties": {
                    "path": {
                        "type": "string",
                        "description": "Path to the file to edit"
                    },
                    "edits": {
                        "type": "array",
                        "items": {
                            "type": "object",
                            "properties": {
                                "oldText": {
                                    "type": "string",
                                    "description": "Exact text to replace"
                                },
                                "newText": {
                                    "type": "string",
                                    "description": "Replacement text"
                                }
                            },
                            "required": ["oldText", "newText"]
                        },
                        "description": "One or more targeted replacements"
                    }
                },
                "required": ["path", "edits"]
            }),
        }
    }

    async fn execute(&self, arguments: serde_json::Value) -> ToolResult {
        let path = arguments["path"].as_str().unwrap_or("");
        let full_path = match resolve_workspace_path(&self.working_dir, path) {
            Ok(path) => path,
            Err(e) => return ToolResult { tool_call_id: String::new(), content: e, is_error: true },
        };

        let content = match std::fs::read_to_string(&full_path) {
            Ok(c) => c,
            Err(e) => {
                return ToolResult {
                    tool_call_id: String::new(),
                    content: format!("Error reading file: {}", e),
                    is_error: true,
                };
            }
        };

        let edits = match arguments["edits"].as_array() {
            Some(e) => e,
            None => {
                return ToolResult {
                    tool_call_id: String::new(),
                    content: "No edits provided".into(),
                    is_error: true,
                };
            }
        };

        let mut new_content = content.clone();
        let mut applied = 0;

        for edit in edits {
            let old_text = edit["oldText"].as_str().unwrap_or("");
            let new_text = edit["newText"].as_str().unwrap_or("");

            if let Some(pos) = new_content.find(old_text) {
                // Check uniqueness
                if new_content[pos + old_text.len()..].contains(old_text) {
                    return ToolResult {
                        tool_call_id: String::new(),
                        content: format!(
                            "oldText '{}' is not unique in the file. Provide more context.",
                            &old_text[..old_text.len().min(50)]
                        ),
                        is_error: true,
                    };
                }
                new_content.replace_range(pos..pos + old_text.len(), new_text);
                applied += 1;
            } else {
                return ToolResult {
                    tool_call_id: String::new(),
                    content: format!(
                        "oldText '{}' not found in file",
                        &old_text[..old_text.len().min(50)]
                    ),
                    is_error: true,
                };
            }
        }

        match std::fs::write(&full_path, &new_content) {
            Ok(_) => ToolResult {
                tool_call_id: String::new(),
                content: format!("Successfully applied {} edit(s) to {}", applied, path),
                is_error: false,
            },
            Err(e) => ToolResult {
                tool_call_id: String::new(),
                content: format!("Error writing file: {}", e),
                is_error: true,
            },
        }
    }
}

// --- Bash Tool ---

struct BashTool {
    working_dir: PathBuf,
    /// Additional directories to prepend to PATH (e.g. CherryPi's private
    /// cargo bin directory where plugin-installed binaries live).
    extra_bin_paths: Option<Arc<Mutex<Vec<PathBuf>>>>,
    /// Secret variables injected by `sops exec-env`. Removed from the child's
    /// environment so commands cannot read or print them.
    secret_env: Vec<String>,
}

impl BashTool {
    fn new(working_dir: PathBuf, extra_bin_paths: Option<Arc<Mutex<Vec<PathBuf>>>>) -> Self {
        Self {
            working_dir,
            extra_bin_paths,
            secret_env: crate::sandbox::sops_secret_names(),
        }
    }
}

#[async_trait]
impl ToolHandler for BashTool {
    fn definition(&self) -> Tool {
        Tool {
            name: "bash".into(),
            description: "Execute a bash command. Returns stdout and stderr. Commands run in a sandbox that can only read and write files inside the working directory.".into(),
            parameters: serde_json::json!({
                "type": "object",
                "properties": {
                    "command": {
                        "type": "string",
                        "description": "Bash command to execute"
                    },
                    "timeout": {
                        "type": "number",
                        "description": "Timeout in seconds (optional)"
                    }
                },
                "required": ["command"]
            }),
        }
    }

    async fn execute(&self, arguments: serde_json::Value) -> ToolResult {
        let command = arguments["command"].as_str().unwrap_or("");
        let timeout_secs = arguments["timeout"].as_u64().unwrap_or(30).clamp(1, 300);

        let mut cmd = std::process::Command::new("bash");
        cmd.arg("-c")
            .arg(command)
            .current_dir(&self.working_dir);
        for name in &self.secret_env {
            cmd.env_remove(name);
        }

        // Prepend extra binary directories to PATH so plugin-installed
        // binaries (e.g. gor from the private cargo bin dir) are found.
        if let Some(ref extra_dirs) = self.extra_bin_paths {
            let dirs = extra_dirs.lock().unwrap();
            if !dirs.is_empty() {
                let current = std::env::var("PATH").unwrap_or_default();
                let extra: Vec<String> = dirs.iter().map(|d| d.to_string_lossy().to_string()).collect();
                let mut new_path = extra.join(":");
                if !current.is_empty() {
                    new_path.push(':');
                    new_path.push_str(&current);
                }
                cmd.env("PATH", &new_path);
            }
        }

        // Confine the command to the working directory. If the sandbox cannot be
        // built or enforced, refuse to run rather than run unconfined.
        let extra_read_dirs: Vec<PathBuf> = self
            .extra_bin_paths
            .as_ref()
            .map(|dirs| dirs.lock().unwrap().clone())
            .unwrap_or_default();
        match Sandbox::prepare(&self.working_dir, &extra_read_dirs) {
            Ok(sandbox) => sandbox.confine(&mut cmd),
            Err(e) => {
                return ToolResult { tool_call_id: String::new(), content: e, is_error: true };
            }
        }

        let output = tokio::time::timeout(
            Duration::from_secs(timeout_secs),
            tokio::task::spawn_blocking(move || cmd.output()),
        ).await;

        match output {
            Ok(Ok(Ok(out))) => {
                let stdout = String::from_utf8_lossy(&out.stdout);
                let stderr = String::from_utf8_lossy(&out.stderr);
                let mut result = String::new();

                if !stdout.is_empty() {
                    result.push_str(&stdout);
                }
                if !stderr.is_empty() {
                    if !result.is_empty() {
                        result.push('\n');
                    }
                    result.push_str("STDERR:\n");
                    result.push_str(&stderr);
                }
                if result.is_empty() {
                    result = format!("Command completed with exit code: {}", out.status.code().unwrap_or(-1));
                }

                ToolResult {
                    tool_call_id: String::new(),
                    content: truncate_output(result),
                    is_error: !out.status.success(),
                }
            }
            Ok(Ok(Err(e))) => ToolResult {
                tool_call_id: String::new(),
                content: format!("Error executing command: {}", e),
                is_error: true,
            },
            Ok(Err(e)) => ToolResult { tool_call_id: String::new(), content: format!("Command task failed: {e}"), is_error: true },
            Err(_) => ToolResult { tool_call_id: String::new(), content: format!("Command exceeded {timeout_secs}s timeout"), is_error: true },
        }
    }
}

// --- Grep Tool ---

struct GrepTool {
    working_dir: PathBuf,
}

impl GrepTool {
    fn new(working_dir: PathBuf) -> Self {
        Self { working_dir }
    }
}

#[async_trait]
impl ToolHandler for GrepTool {
    fn definition(&self) -> Tool {
        Tool {
            name: "grep".into(),
            description: "Search file contents using ripgrep. Returns matching lines with file paths.".into(),
            parameters: serde_json::json!({
                "type": "object",
                "properties": {
                    "pattern": {
                        "type": "string",
                        "description": "Search pattern (literal or regex)"
                    },
                    "path": {
                        "type": "string",
                        "description": "Directory or file to search (default: current directory)"
                    },
                    "glob": {
                        "type": "string",
                        "description": "File glob filter, e.g. *.rs"
                    },
                    "ignore_case": {
                        "type": "boolean",
                        "description": "Case-insensitive search"
                    }
                },
                "required": ["pattern"]
            }),
        }
    }

    async fn execute(&self, arguments: serde_json::Value) -> ToolResult {
        let pattern = arguments["pattern"].as_str().unwrap_or("");
        let search_path = arguments["path"].as_str().unwrap_or(".");
        let glob = arguments["glob"].as_str();
        let ignore_case = arguments["ignore_case"].as_bool().unwrap_or(false);

        let full_path = match resolve_workspace_path(&self.working_dir, search_path) {
            Ok(path) => path,
            Err(e) => return ToolResult { tool_call_id: String::new(), content: e, is_error: true },
        };

        let mut cmd = std::process::Command::new("rg");
        cmd.arg("--line-number");
        cmd.arg("--color=never");

        if ignore_case {
            cmd.arg("--ignore-case");
        }

        if let Some(g) = glob {
            cmd.arg("--glob").arg(g);
        }

        cmd.arg(pattern);
        cmd.arg(&full_path);

        match cmd.output() {
            Ok(out) => {
                let stdout = String::from_utf8_lossy(&out.stdout);
                let result = if stdout.is_empty() {
                    "No matches found.".to_string()
                } else {
                    // Truncate to reasonable size
                    let lines: Vec<&str> = stdout.lines().take(200).collect();
                    lines.join("\n")
                };

                ToolResult {
                    tool_call_id: String::new(),
                    content: result,
                    is_error: false,
                }
            }
            Err(e) => ToolResult {
                tool_call_id: String::new(),
                content: format!("Error searching: {}", e),
                is_error: true,
            },
        }
    }
}

// --- Find Tool ---

struct FindTool {
    working_dir: PathBuf,
}

impl FindTool {
    fn new(working_dir: PathBuf) -> Self {
        Self { working_dir }
    }
}

#[async_trait]
impl ToolHandler for FindTool {
    fn definition(&self) -> Tool {
        Tool {
            name: "find".into(),
            description: "Find files by name or glob pattern.".into(),
            parameters: serde_json::json!({
                "type": "object",
                "properties": {
                    "pattern": {
                        "type": "string",
                        "description": "File name or glob pattern to search for"
                    },
                    "path": {
                        "type": "string",
                        "description": "Directory to search (default: current directory)"
                    }
                },
                "required": ["pattern"]
            }),
        }
    }

    async fn execute(&self, arguments: serde_json::Value) -> ToolResult {
        let pattern = arguments["pattern"].as_str().unwrap_or("*");
        let search_path = arguments["path"].as_str().unwrap_or(".");

        let full_path = match resolve_workspace_path(&self.working_dir, search_path) {
            Ok(path) => path,
            Err(e) => return ToolResult { tool_call_id: String::new(), content: e, is_error: true },
        };

        let output = std::process::Command::new("find")
            .arg(&full_path)
            .arg("-name")
            .arg(pattern)
            .arg("-type")
            .arg("f")
            .arg("-maxdepth")
            .arg("50")
            .output();

        match output {
            Ok(out) => {
                let stdout = String::from_utf8_lossy(&out.stdout);
                let result = if stdout.is_empty() {
                    "No files found.".to_string()
                } else {
                    let lines: Vec<&str> = stdout.lines().take(100).collect();
                    lines.join("\n")
                };

                ToolResult {
                    tool_call_id: String::new(),
                    content: result,
                    is_error: false,
                }
            }
            Err(e) => ToolResult {
                tool_call_id: String::new(),
                content: format!("Error finding files: {}", e),
                is_error: true,
            },
        }
    }
}

// --- List Directory Tool ---

struct ListDirTool {
    working_dir: PathBuf,
}

impl ListDirTool {
    fn new(working_dir: PathBuf) -> Self {
        Self { working_dir }
    }
}

#[async_trait]
impl ToolHandler for ListDirTool {
    fn definition(&self) -> Tool {
        Tool {
            name: "ls".into(),
            description: "List directory contents.".into(),
            parameters: serde_json::json!({
                "type": "object",
                "properties": {
                    "path": {
                        "type": "string",
                        "description": "Directory to list (default: current directory)"
                    },
                    "all": {
                        "type": "boolean",
                        "description": "Include hidden files"
                    }
                }
            }),
        }
    }

    async fn execute(&self, arguments: serde_json::Value) -> ToolResult {
        let path = arguments["path"].as_str().unwrap_or(".");
        let all = arguments["all"].as_bool().unwrap_or(false);

        let full_path = match resolve_workspace_path(&self.working_dir, path) {
            Ok(path) => path,
            Err(e) => return ToolResult { tool_call_id: String::new(), content: e, is_error: true },
        };

        let mut cmd = std::process::Command::new("ls");
        cmd.arg("-la");
        if all {
            cmd.arg("-a");
        }
        cmd.arg(&full_path);

        match cmd.output() {
            Ok(out) => {
                let stdout = String::from_utf8_lossy(&out.stdout);
                ToolResult {
                    tool_call_id: String::new(),
                    content: stdout.to_string(),
                    is_error: false,
                }
            }
            Err(e) => ToolResult {
                tool_call_id: String::new(),
                content: format!("Error listing directory: {}", e),
                is_error: true,
            },
        }
    }
}

// --- Web Search Tool ---

struct WebSearchTool {
    client: reqwest::Client,
}

#[async_trait]
impl ToolHandler for WebSearchTool {
    fn definition(&self) -> Tool {
        Tool {
            name: "web_search".into(),
            description: "Search the web with the Brave Search API. Returns a JSON array of {title, url, snippet} objects.".into(),
            parameters: serde_json::json!({
                "type": "object",
                "properties": {
                    "query": {
                        "type": "string",
                        "description": "The search query"
                    },
                    "max_results": {
                        "type": "number",
                        "description": "Maximum number of results, 1-20 (default: 5)"
                    }
                },
                "required": ["query"]
            }),
        }
    }

    async fn execute(&self, arguments: serde_json::Value) -> ToolResult {
        let query = arguments["query"].as_str().unwrap_or("").trim();
        if query.is_empty() {
            return tool_error("web_search requires a non-empty query".into());
        }
        let max_results = arguments["max_results"]
            .as_f64()
            .map(|n| n.max(1.0) as u64)
            .unwrap_or(5);

        let api_key = match std::env::var(web::BRAVE_API_KEY_ENV) {
            Ok(key) if !key.trim().is_empty() => key,
            _ => {
                return tool_error(format!(
                    "web_search needs a Brave Search API key. Set {}.",
                    web::BRAVE_API_KEY_ENV
                ))
            }
        };

        match web::brave_search(&self.client, api_key.trim(), query, max_results).await {
            Ok(results) if results.is_empty() => tool_ok("No results found.".into()),
            Ok(results) => match serde_json::to_string_pretty(&results) {
                Ok(json) => tool_ok(json),
                Err(e) => tool_error(format!("Failed to encode search results: {e}")),
            },
            Err(e) => tool_error(e),
        }
    }
}

// --- Web Fetch Tool ---

struct WebFetchTool {
    client: reqwest::Client,
}

#[async_trait]
impl ToolHandler for WebFetchTool {
    fn definition(&self) -> Tool {
        Tool {
            name: "web_fetch".into(),
            description: "Fetch a http(s) URL and return its text content. By default the raw HTML is reduced to plain text. Set render to true for JavaScript-heavy pages (single-page apps), which are rendered in a browser engine first. Rendering is slower.".into(),
            parameters: serde_json::json!({
                "type": "object",
                "properties": {
                    "url": {
                        "type": "string",
                        "description": "URL to fetch and extract content from"
                    },
                    "render": {
                        "type": "boolean",
                        "description": "Render the page with JavaScript before extracting text (default: false)"
                    }
                },
                "required": ["url"]
            }),
        }
    }

    async fn execute(&self, arguments: serde_json::Value) -> ToolResult {
        let url = arguments["url"].as_str().unwrap_or("");
        if arguments["render"].as_bool().unwrap_or(false) {
            return render_fetch(url).await;
        }
        match web::fetch_text(&self.client, url).await {
            Ok(text) => tool_ok(text),
            Err(e) => tool_error(e),
        }
    }
}

/// Render `url` in Servo and format the title and text for the model.
async fn render_fetch(url: &str) -> ToolResult {
    let url = match web::validate_http_url(url).and_then(|u| url::Url::parse(u).map_err(|e| e.to_string())) {
        Ok(url) => url,
        Err(e) => return tool_error(e),
    };
    #[cfg(not(feature = "render"))]
    {
        let _ = url;
        tool_error(
            "Rendering is not available: this build of cherrypi was made without the \"render\" feature. Fetch without render instead."
                .to_string(),
        )
    }
    #[cfg(feature = "render")]
    match cherrypi_fetch::render_page(url, cherrypi_fetch::RENDER_TIMEOUT).await {
        Ok(page) => tool_ok(format_rendered(
            &page.title,
            page.incomplete.as_deref(),
            &page.text,
        )),
        Err(e) => tool_error(format!("Render failed: {e}")),
    }
}

/// Format a rendered page for the model. The whole response, title included,
/// is held to [`web::FETCH_MAX_CHARS`].
#[cfg_attr(not(feature = "render"), allow(dead_code))]
fn format_rendered(title: &str, incomplete: Option<&str>, text: &str) -> String {
    let note = match incomplete {
        Some(reason) => format!("Note: incomplete render: {reason}\n"),
        None => String::new(),
    };
    web::truncate_chars(&format!("Title: {title}\n{note}\n{text}"), web::FETCH_MAX_CHARS)
}

fn tool_ok(content: String) -> ToolResult {
    ToolResult {
        tool_call_id: String::new(),
        content,
        is_error: false,
    }
}

fn tool_error(content: String) -> ToolResult {
    ToolResult {
        tool_call_id: String::new(),
        content,
        is_error: true,
    }
}

// --- Helpers ---

#[cfg(all(test, target_os = "linux"))]
mod bash_secret_env_tests {
    use super::*;

    #[test]
    fn bash_child_does_not_see_secret_env() {
        let name = "CHERRYPI_TEST_SECRET_STRIP";
        std::env::set_var(name, "s3cret");
        let ws = std::env::temp_dir().join(format!("cherrypi-secret-env-{}", std::process::id()));
        std::fs::create_dir_all(&ws).unwrap();

        let mut tool = BashTool::new(ws, None);
        tool.secret_env = vec![name.to_string()];
        let result = tokio::runtime::Runtime::new().unwrap().block_on(
            tool.execute(serde_json::json!({"command": format!("echo \"[${{{name}:-unset}}]\"")})),
        );
        std::env::remove_var(name);

        assert!(!result.content.contains("s3cret"), "secret leaked: {}", result.content);
        assert!(result.content.contains("[unset]"), "{}", result.content);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rendered_response_is_capped_including_the_title() {
        let short = format_rendered("Page", Some("slow"), "body");
        assert_eq!(short, "Title: Page\nNote: incomplete render: slow\n\nbody");

        let huge_title = "t".repeat(web::FETCH_MAX_CHARS * 2);
        let capped = format_rendered(&huge_title, None, "body");
        assert!(capped.ends_with("[truncated]"), "{}", &capped[capped.len() - 40..]);
        assert!(capped.chars().count() <= web::FETCH_MAX_CHARS + "\n[truncated]".len());
    }
}
