use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::sync::Arc;
use async_trait::async_trait;

/// A tool definition that can be called by the LLM
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Tool {
    /// Unique name of the tool
    pub name: String,
    /// Human-readable description
    pub description: String,
    /// JSON Schema for the tool's parameters
    pub parameters: serde_json::Value,
}

/// A request to call a tool from the LLM
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ToolCall {
    /// Unique ID for this tool call
    pub id: String,
    /// Name of the tool to call
    pub name: String,
    /// Arguments to pass to the tool
    pub arguments: serde_json::Value,
}

/// Result of executing a tool
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ToolResult {
    /// ID of the tool call this result corresponds to
    pub tool_call_id: String,
    /// The output content
    pub content: String,
    /// Whether this result represents an error
    #[serde(default)]
    pub is_error: bool,
}

/// Trait for implementing a tool handler
#[async_trait]
pub trait ToolHandler: Send + Sync {
    /// Get the tool definition
    fn definition(&self) -> Tool;

    /// Execute the tool with given arguments
    async fn execute(&self, arguments: serde_json::Value) -> ToolResult;
}

/// Registry of available tools
#[derive(Default)]
pub struct ToolRegistry {
    tools: HashMap<String, Arc<dyn ToolHandler>>,
}

impl ToolRegistry {
    pub fn new() -> Self {
        Self {
            tools: HashMap::new(),
        }
    }

    /// Register a tool handler
    pub fn register(&mut self, handler: Arc<dyn ToolHandler>) {
        let def = handler.definition();
        self.tools.insert(def.name.clone(), handler);
    }

    /// Get all tool definitions (for sending to LLM)
    pub fn definitions(&self) -> Vec<Tool> {
        self.tools
            .values()
            .map(|h| h.definition())
            .collect()
    }

    /// Execute a tool call
    pub async fn execute(&self, call: &ToolCall) -> Result<ToolResult, String> {
        match self.tools.get(&call.name) {
            Some(handler) => Ok(handler.execute(call.arguments.clone()).await),
            None => Err(format!("Tool not found: {}", call.name)),
        }
    }

    /// Check if a tool exists
    pub fn has(&self, name: &str) -> bool {
        self.tools.contains_key(name)
    }

    /// Get tool names
    pub fn names(&self) -> Vec<String> {
        self.tools.keys().cloned().collect()
    }
}
