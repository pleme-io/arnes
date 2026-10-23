use thiserror::Error;

pub type Result<T> = std::result::Result<T, ArnesError>;

#[derive(Debug, Error)]
pub enum ArnesError {
    #[error("invalid role: {0}")]
    InvalidRole(String),
    #[error("tool not found: {0}")]
    ToolNotFound(String),
    #[error("bad tool input for {tool}: {reason}")]
    BadToolInput { tool: String, reason: String },
    #[error("json: {0}")]
    Json(#[from] serde_json::Error),
    #[error("io: {0}")]
    Io(#[from] std::io::Error),
}
