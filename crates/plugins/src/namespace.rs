/// Namespace utilities for plugin components
///
/// All plugin components are namespaced as `{plugin-name}:{component-name}`
/// to prevent conflicts between plugins.
/// Format a namespaced component name
pub fn namespace(plugin_name: &str, component_name: &str) -> String {
    format!("{}:{}", plugin_name, component_name)
}

/// Parse a namespaced name into (plugin, component)
pub fn parse_namespaced(name: &str) -> Option<(&str, &str)> {
    name.split_once(':')
}

/// Format a slash command name
pub fn slash_command(plugin_name: &str, command_name: &str) -> String {
    format!("/{}:{}", plugin_name, command_name)
}

/// Check if a name is namespaced
pub fn is_namespaced(name: &str) -> bool {
    name.contains(':')
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_namespace() {
        assert_eq!(namespace("deploy-tools", "status"), "deploy-tools:status");
    }

    #[test]
    fn test_parse_namespaced() {
        assert_eq!(
            parse_namespaced("deploy-tools:status"),
            Some(("deploy-tools", "status"))
        );
        assert_eq!(parse_namespaced("no-namespace"), None);
    }

    #[test]
    fn test_slash_command() {
        assert_eq!(slash_command("deploy-tools", "status"), "/deploy-tools:status");
    }

    #[test]
    fn test_is_namespaced() {
        assert!(is_namespaced("plugin:component"));
        assert!(!is_namespaced("plain"));
    }
}
