use thiserror::Error;

#[derive(Error, Debug)]
pub enum CoreError {
    #[error("Configuration error: {0}")]
    Config(String),

    #[error("IO error: {0}")]
    Io(#[from] std::io::Error),

    #[error("Serialization error: {0}")]
    Serialization(#[from] serde_json::Error),

    #[error("Invalid state: {0}")]
    InvalidState(String),

    #[error("Tool not found: {0}")]
    ToolNotFound(String),

    #[error("Provider error: {0}")]
    Provider(String),

    #[error("Plugin error: {0}")]
    Plugin(String),
}
