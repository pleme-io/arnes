//! Error type for the arnes-tui rendering surface.

/// Result type used by the rendering helpers.
pub type Result<T> = std::result::Result<T, Error>;

/// Failure mode for terminal drawing. Stays string-based so the rest of
/// the workspace (which uses `anyhow::Error` at boundaries) doesn't have
/// to learn a new error type.
#[derive(Debug, thiserror::Error)]
pub enum Error {
    #[error("io: {0}")]
    Io(String),
}
