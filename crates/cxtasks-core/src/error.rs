//! The one error type every layer returns.
//!
//! There used to be a `Tauri(#[from] tauri::Error)` variant here, and it was
//! the only thing keeping this file from being platform-neutral. It had no
//! explicit construction site — it was produced implicitly by a single `?` on
//! `run_on_main_thread` in `commands/appearance.rs`, which now maps the error
//! itself. Its `#[error]` string read "Menu error", which had not been true
//! for some time.
//!
//! Nothing else here needs a UI framework, so nothing else moved.

use serde::Serialize;
use thiserror::Error;

#[derive(Debug, Error)]
pub enum AppError {
    #[error("Database error: {0}")]
    Database(#[from] rusqlite::Error),

    #[error("IO error: {0}")]
    Io(#[from] std::io::Error),

    #[error("Not found: {0}")]
    NotFound(String),

    #[error("Invalid input: {0}")]
    Validation(String),

    #[error("Dispatch failed: {0}")]
    Dispatch(String),

    #[error("{0}")]
    General(String),
}

// Serialize for IPC — only expose the message, not internals.
impl Serialize for AppError {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: serde::Serializer,
    {
        serializer.serialize_str(&self.to_string())
    }
}
