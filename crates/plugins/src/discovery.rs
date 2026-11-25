use std::path::{Path, PathBuf};
use tracing::{debug, info, warn};

use super::manifest::PluginManifest;

/// Represents a discovered plugin with its components
#[derive(Debug, Clone)]
pub struct DiscoveredPlugin {
    /// Plugin name (from manifest or directory)
    pub name: String,
    /// Plugin root directory
    pub root: PathBuf,
    /// Parsed manifest (if present)
    pub manifest: Option<PluginManifest>,
    /// Discovered components
    pub components: PluginComponents,
}

/// Components discovered in a plugin
#[derive(Debug, Clone, Default)]
pub struct PluginComponents {
    /// Command skills (flat .md files in commands/)
    pub commands: Vec<CommandSkill>,
    /// Agent skills (subdirectories with SKILL.md in skills/)
    pub skills: Vec<AgentSkill>,
    /// Agent definitions (markdown files in agents/)
    pub agents: Vec<AgentDef>,
    /// Rule files (.mdc files in rules/)
    pub rules: Vec<RuleFile>,
    /// Hook configuration
    pub hooks: Option<serde_json::Value>,
    /// MCP server configurations
    pub mcp_servers: Option<serde_json::Value>,
    /// LSP server configurations
    pub lsp_servers: Option<serde_json::Value>,
    /// Status bar configurations from plugins
    pub status_bars: Vec<super::manifest::StatusBarConfig>,
}

#[derive(Debug, Clone)]
pub struct CommandSkill {
    pub name: String,
    pub path: PathBuf,
    pub description: Option<String>,
    pub disable_model_invocation: bool,
}

#[derive(Debug, Clone)]
pub struct AgentSkill {
    pub name: String,
    pub dir: PathBuf,
    pub skill_md_path: PathBuf,
    pub description: Option<String>,
}

#[derive(Debug, Clone)]
pub struct AgentDef {
    pub name: String,
    pub path: PathBuf,
    pub description: Option<String>,
}

#[derive(Debug, Clone)]
pub struct RuleFile {
    pub name: String,
    pub path: PathBuf,
}

/// Discovers plugins and their components
pub struct PluginDiscovery;

impl PluginDiscovery {
    /// Discover a plugin from a directory
    pub fn discover(plugin_dir: &Path) -> Option<DiscoveredPlugin> {
        if !plugin_dir.is_dir() {
            warn!("Plugin directory does not exist: {:?}", plugin_dir);
            return None;
        }

        let manifest = PluginManifest::load(plugin_dir);
        let name = manifest
            .as_ref()
            .map(|m| m.name.clone())
            .unwrap_or_else(|| PluginManifest::name_from_dir(plugin_dir));

        info!("Discovering plugin: {} at {:?}", name, plugin_dir);

        let mut components = PluginComponents::default();

        // Discover commands
        Self::discover_commands(plugin_dir, &manifest, &mut components);

        // Discover skills
        Self::discover_skills(plugin_dir, &manifest, &mut components);

        // Discover agents
        Self::discover_agents(plugin_dir, &manifest, &mut components);

        // Discover rules
        Self::discover_rules(plugin_dir, &manifest, &mut components);

        // Discover hooks
        Self::discover_hooks(plugin_dir, &manifest, &mut components);

        // Discover MCP servers
        Self::discover_mcp(plugin_dir, &manifest, &mut components);

        // Discover LSP servers
        Self::discover_lsp(plugin_dir, &manifest, &mut components);

        // Discover status bar configs
        Self::discover_status_bar(&manifest, &mut components);

        Some(DiscoveredPlugin {
            name,
            root: plugin_dir.to_path_buf(),
            manifest,
            components,
        })
    }

    fn discover_commands(
        plugin_dir: &Path,
        manifest: &Option<PluginManifest>,
        components: &mut PluginComponents,
    ) {
        let commands_dir = plugin_dir.join("commands");
        if commands_dir.is_dir() {
            if let Ok(entries) = std::fs::read_dir(&commands_dir) {
                for entry in entries.flatten() {
                    let path = entry.path();
                    if path.extension().is_some_and(|e| e == "md") {
                        let name = path.file_stem()
                            .and_then(|s| s.to_str())
                            .unwrap_or("unknown")
                            .to_string();

                        let (description, disable_model) = Self::parse_command_frontmatter(&path);

                        debug!("  Found command: {}", name);
                        components.commands.push(CommandSkill {
                            name,
                            path,
                            description,
                            disable_model_invocation: disable_model,
                        });
                    }
                }
            }
        }

        // Also check manifest custom paths
        if let Some(manifest) = manifest {
            if let Some(cmd_paths) = &manifest.commands {
                for cmd_path in cmd_paths.paths() {
                    let full_path = plugin_dir.join(cmd_path.trim_start_matches("./"));
                    if full_path.is_file() && full_path.extension().is_some_and(|e| e == "md") {
                        let name = full_path.file_stem()
                            .and_then(|s| s.to_str())
                            .unwrap_or("unknown")
                            .to_string();
                        let (description, disable_model) = Self::parse_command_frontmatter(&full_path);
                        components.commands.push(CommandSkill {
                            name,
                            path: full_path,
                            description,
                            disable_model_invocation: disable_model,
                        });
                    }
                }
            }
        }
    }

    fn discover_skills(
        plugin_dir: &Path,
        _manifest: &Option<PluginManifest>,
        components: &mut PluginComponents,
    ) {
        let skills_dir = plugin_dir.join("skills");
        if skills_dir.is_dir() {
            if let Ok(entries) = std::fs::read_dir(&skills_dir) {
                for entry in entries.flatten() {
                    let dir = entry.path();
                    if dir.is_dir() {
                        let skill_md = dir.join("SKILL.md");
                        if skill_md.exists() {
                            let name = dir.file_name()
                                .and_then(|s| s.to_str())
                                .unwrap_or("unknown")
                                .to_string();

                            let description = Self::parse_skill_description(&skill_md);

                            debug!("  Found skill: {}", name);
                            components.skills.push(AgentSkill {
                                name,
                                dir,
                                skill_md_path: skill_md,
                                description,
                            });
                        }
                    }
                }
            }
        }

        // Root SKILL.md fallback
        if components.skills.is_empty() {
            let root_skill = plugin_dir.join("SKILL.md");
            if root_skill.exists() {
                let name = PluginManifest::name_from_dir(plugin_dir);
                let description = Self::parse_skill_description(&root_skill);
                components.skills.push(AgentSkill {
                    name,
                    dir: plugin_dir.to_path_buf(),
                    skill_md_path: root_skill,
                    description,
                });
            }
        }
    }

    fn discover_agents(
        plugin_dir: &Path,
        _manifest: &Option<PluginManifest>,
        components: &mut PluginComponents,
    ) {
        let agents_dir = plugin_dir.join("agents");
        if agents_dir.is_dir() {
            if let Ok(entries) = std::fs::read_dir(&agents_dir) {
                for entry in entries.flatten() {
                    let path = entry.path();
                    if path.extension().is_some_and(|e| e == "md") {
                        let name = path.file_stem()
                            .and_then(|s| s.to_str())
                            .unwrap_or("unknown")
                            .to_string();
                        let description = Self::parse_agent_description(&path);
                        debug!("  Found agent: {}", name);
                        components.agents.push(AgentDef {
                            name,
                            path,
                            description,
                        });
                    }
                }
            }
        }
    }

    fn discover_rules(
        plugin_dir: &Path,
        _manifest: &Option<PluginManifest>,
        components: &mut PluginComponents,
    ) {
        let rules_dir = plugin_dir.join("rules");
        if rules_dir.is_dir() {
            if let Ok(entries) = std::fs::read_dir(&rules_dir) {
                for entry in entries.flatten() {
                    let path = entry.path();
                    if path.extension().is_some_and(|e| e == "mdc") {
                        let name = path.file_stem()
                            .and_then(|s| s.to_str())
                            .unwrap_or("unknown")
                            .to_string();
                        debug!("  Found rule: {}", name);
                        components.rules.push(RuleFile { name, path });
                    }
                }
            }
        }
    }

    fn discover_hooks(
        plugin_dir: &Path,
        manifest: &Option<PluginManifest>,
        components: &mut PluginComponents,
    ) {
        let hooks_file = plugin_dir.join("hooks").join("hooks.json");
        if hooks_file.exists() {
            if let Ok(content) = std::fs::read_to_string(&hooks_file) {
                if let Ok(config) = serde_json::from_str::<serde_json::Value>(&content) {
                    components.hooks = Some(config);
                }
            }
        }

        // Check manifest inline hooks
        if let Some(manifest) = manifest {
            if let Some(hook_paths) = &manifest.hooks {
                for hook_path in hook_paths.paths() {
                    let full_path = plugin_dir.join(hook_path.trim_start_matches("./"));
                    if full_path.exists() {
                        if let Ok(content) = std::fs::read_to_string(&full_path) {
                            if let Ok(config) = serde_json::from_str::<serde_json::Value>(&content) {
                                components.hooks = Some(config);
                            }
                        }
                    }
                }
            }
        }
    }

    fn discover_mcp(
        plugin_dir: &Path,
        _manifest: &Option<PluginManifest>,
        components: &mut PluginComponents,
    ) {
        let mcp_file = plugin_dir.join(".mcp.json");
        if mcp_file.exists() {
            if let Ok(content) = std::fs::read_to_string(&mcp_file) {
                if let Ok(config) = serde_json::from_str::<serde_json::Value>(&content) {
                    components.mcp_servers = Some(config);
                }
            }
        }
    }

    fn discover_lsp(
        plugin_dir: &Path,
        _manifest: &Option<PluginManifest>,
        components: &mut PluginComponents,
    ) {
        let lsp_file = plugin_dir.join(".lsp.json");
        if lsp_file.exists() {
            if let Ok(content) = std::fs::read_to_string(&lsp_file) {
                if let Ok(config) = serde_json::from_str::<serde_json::Value>(&content) {
                    components.lsp_servers = Some(config);
                }
            }
        }
    }

    fn discover_status_bar(
        manifest: &Option<PluginManifest>,
        components: &mut PluginComponents,
    ) {
        if let Some(manifest) = manifest {
            if let Some(ref status_bar) = manifest.status_bar {
                debug!("  Found status bar: {} (refresh: {}ms)", status_bar.command, status_bar.refresh_ms);
                components.status_bars.push(status_bar.clone());
            }
        }
    }

    // --- Frontmatter parsing helpers ---

    fn parse_command_frontmatter(path: &Path) -> (Option<String>, bool) {
        if let Ok(content) = std::fs::read_to_string(path) {
            if let Some(fm) = Self::extract_frontmatter(&content) {
                let description = fm.get("description").and_then(|v| v.as_str()).map(String::from);
                let disable = fm
                    .get("disable-model-invocation")
                    .and_then(|v| v.as_bool())
                    .unwrap_or(false);
                return (description, disable);
            }
        }
        (None, false)
    }

    fn parse_skill_description(path: &Path) -> Option<String> {
        if let Ok(content) = std::fs::read_to_string(path) {
            if let Some(fm) = Self::extract_frontmatter(&content) {
                return fm.get("description").and_then(|v| v.as_str()).map(String::from);
            }
        }
        None
    }

    fn parse_agent_description(path: &Path) -> Option<String> {
        Self::parse_skill_description(path)
    }

    fn extract_frontmatter(content: &str) -> Option<serde_json::Map<String, serde_json::Value>> {
        let content = content.trim();
        if let Some(stripped) = content.strip_prefix("---") {
            let end = stripped.find("---")?;
            let fm_str = &stripped[..end];
            // Try YAML first, then JSON
            serde_yaml::from_str::<serde_json::Value>(fm_str)
                .ok()
                .and_then(|v| v.as_object().cloned())
        } else {
            None
        }
    }
}
