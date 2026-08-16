use std::collections::HashMap;
use std::path::{Path, PathBuf};
use tracing::{info, warn};

use super::discovery::{DiscoveredPlugin, PluginDiscovery};
use super::manifest::PluginManifest;

/// A loaded plugin ready for use
#[derive(Debug, Clone)]
pub struct LoadedPlugin {
    /// Plugin name
    pub name: String,
    /// Plugin root directory
    pub root: PathBuf,
    /// Manifest (if present)
    pub manifest: Option<PluginManifest>,
    /// Whether the plugin is enabled
    pub enabled: bool,
    /// Discovered components
    pub components: super::discovery::PluginComponents,
}

/// Manages plugin lifecycle: discovery, loading, enabling, disabling
pub struct PluginLoader {
    /// Loaded plugins by name
    plugins: HashMap<String, LoadedPlugin>,
    /// Plugin search directories
    search_dirs: Vec<PathBuf>,
    /// Install directory for cached plugins
    install_dir: Option<PathBuf>,
}

impl PluginLoader {
    pub fn new() -> Self {
        let install_dir = dirs::data_dir()
            .map(|d| d.join("cherrypi").join("plugins"));

        Self {
            plugins: HashMap::new(),
            search_dirs: Vec::new(),
            install_dir,
        }
    }

    /// Add a directory to search for plugins
    pub fn add_search_dir(&mut self, dir: PathBuf) {
        self.search_dirs.push(dir);
    }

    /// Get the install directory, if set
    pub fn install_dir(&self) -> Option<&PathBuf> {
        self.install_dir.as_ref()
    }

    /// Set the install directory
    pub fn set_install_dir(&mut self, dir: PathBuf) {
        self.install_dir = Some(dir);
    }

    /// Discover and load all plugins from search directories
    pub fn discover_all(&mut self) -> Vec<String> {
        let mut loaded = Vec::new();

        for dir in &self.search_dirs.clone() {
            if let Ok(entries) = std::fs::read_dir(dir) {
                for entry in entries.flatten() {
                    let path = entry.path();
                    if path.is_dir() {
                        if let Some(plugin) = PluginDiscovery::discover(&path) {
                            let name = plugin.name.clone();
                            self.load_plugin(plugin);
                            loaded.push(name);
                        }
                    }
                }
            }
        }

        // Also check install directory
        if let Some(ref install_dir) = self.install_dir {
            if install_dir.is_dir() {
                if let Ok(entries) = std::fs::read_dir(install_dir) {
                    for entry in entries.flatten() {
                        let path = entry.path();
                        if path.is_dir() {
                            if let Some(plugin) = PluginDiscovery::discover(&path) {
                                let name = plugin.name.clone();
                                self.load_plugin(plugin);
                                loaded.push(name);
                            }
                        }
                    }
                }
            }
        }

        loaded
    }

    /// Load a single plugin from a directory
    pub fn load_from_dir(&mut self, dir: &Path) -> Option<String> {
        let plugin = PluginDiscovery::discover(dir)?;
        let name = plugin.name.clone();
        self.load_plugin(plugin);
        Some(name)
    }

    fn load_plugin(&mut self, discovered: DiscoveredPlugin) {
        info!("Loading plugin: {} ({} commands, {} skills, {} agents)",
            discovered.name,
            discovered.components.commands.len(),
            discovered.components.skills.len(),
            discovered.components.agents.len(),
        );

        let loaded = LoadedPlugin {
            name: discovered.name.clone(),
            root: discovered.root,
            manifest: discovered.manifest,
            enabled: true,
            components: discovered.components,
        };

        self.plugins.insert(discovered.name, loaded);
    }

    /// Enable a plugin
    pub fn enable(&mut self, name: &str) -> bool {
        if let Some(plugin) = self.plugins.get_mut(name) {
            plugin.enabled = true;
            info!("Plugin enabled: {}", name);
            true
        } else {
            warn!("Plugin not found: {}", name);
            false
        }
    }

    /// Disable a plugin
    pub fn disable(&mut self, name: &str) -> bool {
        if let Some(plugin) = self.plugins.get_mut(name) {
            plugin.enabled = false;
            info!("Plugin disabled: {}", name);
            true
        } else {
            warn!("Plugin not found: {}", name);
            false
        }
    }

    /// Get a loaded plugin by name
    pub fn get(&self, name: &str) -> Option<&LoadedPlugin> {
        self.plugins.get(name)
    }

    /// List all loaded plugin names
    pub fn list(&self) -> Vec<String> {
        self.plugins.keys().cloned().collect()
    }

    /// List enabled plugin names
    pub fn enabled_plugins(&self) -> Vec<&LoadedPlugin> {
        self.plugins
            .values()
            .filter(|p| p.enabled)
            .collect()
    }

    /// Get all command skills from enabled plugins
    pub fn all_commands(&self) -> Vec<(String, &super::discovery::CommandSkill)> {
        self.enabled_plugins()
            .iter()
            .flat_map(|p| {
                p.components
                    .commands
                    .iter()
                    .map(move |c| (p.name.clone(), c))
            })
            .collect()
    }

    /// Get all agent skills from enabled plugins
    pub fn all_skills(&self) -> Vec<(String, &super::discovery::AgentSkill)> {
        self.enabled_plugins()
            .iter()
            .flat_map(|p| {
                p.components
                    .skills
                    .iter()
                    .map(move |s| (p.name.clone(), s))
            })
            .collect()
    }

    /// Get all agents from enabled plugins
    pub fn all_agents(&self) -> Vec<(String, &super::discovery::AgentDef)> {
        self.enabled_plugins()
            .iter()
            .flat_map(|p| {
                p.components
                    .agents
                    .iter()
                    .map(move |a| (p.name.clone(), a))
            })
            .collect()
    }

    /// Get all rules from enabled plugins
    pub fn all_rules(&self) -> Vec<(String, &super::discovery::RuleFile)> {
        self.enabled_plugins()
            .iter()
            .flat_map(|p| {
                p.components
                    .rules
                    .iter()
                    .map(move |r| (p.name.clone(), r))
            })
            .collect()
    }

    /// Install a plugin to the cache directory
    pub fn install(&mut self, name: &str, source: &Path) -> Result<(), String> {
        let install_dir = self
            .install_dir
            .as_ref()
            .ok_or("No install directory configured")?;

        let dest = install_dir.join(name);
        if dest.exists() {
            std::fs::remove_dir_all(&dest).map_err(|e| format!("Failed to remove old plugin: {}", e))?;
        }

        // Copy directory recursively
        copy_dir_recursive(source, &dest)
            .map_err(|e| format!("Failed to copy plugin: {}", e))?;

        info!("Plugin installed: {} -> {:?}", name, dest);
        self.load_from_dir(&dest);

        Ok(())
    }

    /// Uninstall a plugin
    pub fn uninstall(&mut self, name: &str) -> Result<(), String> {
        let install_dir = self
            .install_dir
            .as_ref()
            .ok_or("No install directory configured")?;

        let dest = install_dir.join(name);
        if dest.exists() {
            std::fs::remove_dir_all(&dest)
                .map_err(|e| format!("Failed to remove plugin: {}", e))?;
        }

        self.plugins.remove(name);
        info!("Plugin uninstalled: {}", name);
        Ok(())
    }
}

fn copy_dir_recursive(src: &Path, dst: &Path) -> std::io::Result<()> {
    std::fs::create_dir_all(dst)?;
    for entry in std::fs::read_dir(src)? {
        let entry = entry?;
        let ty = entry.file_type()?;
        let src_path = entry.path();
        let dst_path = dst.join(entry.file_name());

        if ty.is_dir() {
            copy_dir_recursive(&src_path, &dst_path)?;
        } else if ty.is_symlink() {
            let target = std::fs::read_link(&src_path)?;
            std::os::unix::fs::symlink(&target, &dst_path)?;
        } else {
            std::fs::copy(&src_path, &dst_path)?;
        }
    }
    Ok(())
}
