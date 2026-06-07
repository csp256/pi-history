use thiserror::Error;

#[derive(Debug, Error)]
pub enum AppError {
    #[error("I/O error: {0}")]
    Io(#[from] std::io::Error),

    #[error("JSON parsing error: {0}")]
    Json(#[from] serde_json::Error),

    #[error("Pi sessions directory not found at {0}")]
    ProjectsDirNotFound(String),

    #[error("No conversation history found in {0}")]
    NoHistoryFound(String),

    #[error("User cancelled selection")]
    SelectionCancelled,

    #[error("Session not found: {0}")]
    SessionNotFound(String),

    #[error("Failed to run Pi CLI: {0}")]
    PiExecutionError(String),

    #[error("Configuration error: {0}")]
    ConfigError(String),

    #[error("Update error: {0}")]
    UpdateError(String),

    #[error("Semantic search cancelled")]
    SemanticSearchCancelled,
}

pub type Result<T> = std::result::Result<T, AppError>;
