//! CherryPi Agent - the core agentic loop and tool system
//!
//! Implements the agent loop that:
//! 1. Takes user input
//! 2. Sends to LLM with tool definitions
//! 3. Executes tool calls
//! 4. Returns results to LLM
//! 5. Repeats until the LLM produces a final response

pub mod agent;
pub mod tools;
pub mod fff_tools;
pub mod context;
pub mod sandbox;
pub mod git_tool;
pub mod render;
pub mod web;

pub use agent::Agent;
pub use tools::BuiltinTools;
pub use context::AgentContext;
