use std::collections::HashMap;
use std::process::{Child, Command, Stdio};
use tracing::{error, info, warn};

/// Configuration for an MCP server
#[derive(Debug, Clone)]
pub struct McpServerConfig {
    pub name: String,
    pub command: String,
    pub args: Vec<String>,
    pub env: HashMap<String, String>,
    pub cwd: Option<String>,
}

/// A running MCP server process
pub struct McpServer {
    pub config: McpServerConfig,
    process: Option<Child>,
}

impl McpServer {
    pub fn new(config: McpServerConfig) -> Self {
        Self {
            config,
            process: None,
        }
    }

    /// Start the MCP server process
    pub fn start(&mut self) -> Result<(), String> {
        if self.process.is_some() {
            return Ok(()); // Already running
        }

        info!("Starting MCP server: {}", self.config.name);

        let mut cmd = Command::new(&self.config.command);
        cmd.args(&self.config.args);
        cmd.stdin(Stdio::piped());
        cmd.stdout(Stdio::piped());
        cmd.stderr(Stdio::piped());

        for (key, value) in &self.config.env {
            cmd.env(key, value);
        }

        if let Some(ref cwd) = self.config.cwd {
            cmd.current_dir(cwd);
        }

        match cmd.spawn() {
            Ok(child) => {
                self.process = Some(child);
                info!("MCP server started: {}", self.config.name);
                Ok(())
            }
            Err(e) => {
                error!("Failed to start MCP server {}: {}", self.config.name, e);
                Err(format!("Failed to start MCP server: {}", e))
            }
        }
    }

    /// Stop the MCP server process
    pub fn stop(&mut self) {
        if let Some(ref mut child) = self.process {
            info!("Stopping MCP server: {}", self.config.name);
            let _ = child.kill();
            let _ = child.wait();
            self.process = None;
        }
    }

    /// Check if the server is running
    pub fn is_running(&self) -> bool {
        self.process.is_some()
    }
}

impl Drop for McpServer {
    fn drop(&mut self) {
        self.stop();
    }
}

/// Manager for MCP servers from plugins
pub struct McpManager {
    servers: HashMap<String, McpServer>,
}

impl Default for McpManager {
    fn default() -> Self { Self::new() }
}

impl McpManager {
    pub fn new() -> Self {
        Self {
            servers: HashMap::new(),
        }
    }

    /// Load MCP server configs from a plugin's .mcp.json
    pub fn load_from_config(
        &mut self,
        plugin_name: &str,
        plugin_root: &str,
        config: &serde_json::Value,
    ) {
        let servers = match config.get("mcpServers") {
            Some(s) => s,
            None => return,
        };

        let server_map = match servers.as_object() {
            Some(obj) => obj,
            None => return,
        };

        for (name, server_config) in server_map {
            let command = match server_config.get("command").and_then(|c| c.as_str()) {
                Some(cmd) => cmd.replace("${PLUGIN_ROOT}", plugin_root),
                None => {
                    warn!("MCP server {} missing command", name);
                    continue;
                }
            };

            let args: Vec<String> = server_config
                .get("args")
                .and_then(|a| a.as_array())
                .map(|arr| {
                    arr.iter()
                        .filter_map(|v| v.as_str())
                        .map(|s| s.replace("${PLUGIN_ROOT}", plugin_root))
                        .collect()
                })
                .unwrap_or_default();

            let env: HashMap<String, String> = server_config
                .get("env")
                .and_then(|e| e.as_object())
                .map(|obj| {
                    obj.iter()
                        .map(|(k, v)| {
                            (
                                k.clone(),
                                v.as_str()
                                    .unwrap_or("")
                                    .replace("${PLUGIN_ROOT}", plugin_root),
                            )
                        })
                        .collect()
                })
                .unwrap_or_default();

            let cwd = server_config
                .get("cwd")
                .and_then(|c| c.as_str())
                .map(|s| s.replace("${PLUGIN_ROOT}", plugin_root));

            let full_name = format!("{}:{}", plugin_name, name);
            let config = McpServerConfig {
                name: full_name.clone(),
                command,
                args,
                env,
                cwd,
            };

            self.servers
                .insert(full_name, McpServer::new(config));
        }
    }

    /// Start all registered MCP servers
    pub fn start_all(&mut self) {
        for server in self.servers.values_mut() {
            if let Err(e) = server.start() {
                error!("Failed to start MCP server {}: {}", server.config.name, e);
            }
        }
    }

    /// Stop all MCP servers
    pub fn stop_all(&mut self) {
        for server in self.servers.values_mut() {
            server.stop();
        }
    }

    /// Get a list of running server names
    pub fn running_servers(&self) -> Vec<String> {
        self.servers
            .iter()
            .filter(|(_, s)| s.is_running())
            .map(|(n, _)| n.clone())
            .collect()
    }
}

impl Drop for McpManager {
    fn drop(&mut self) {
        self.stop_all();
    }
}
