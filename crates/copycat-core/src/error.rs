//! Errors, and the exit codes the CLI reports them as.
//!
//! Every failure carries a stable machine-readable `code` alongside the
//! human message, so scripts and bindings can branch on the reason without
//! parsing prose.

use serde::{Deserialize, Serialize};
use std::fmt;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ErrorKind {
    InvalidRequest,
    DaemonUnavailable,
    ClipboardUnavailable,
    InputPermission,
    StorageUnavailable,
    NotFound,
    UnsupportedContent,
    PlatformUnavailable,
}

impl ErrorKind {
    /// The process exit code the CLI returns (CLI_SPEC "Exit codes").
    pub fn exit_code(self) -> i32 {
        match self {
            ErrorKind::InvalidRequest => 2,
            ErrorKind::DaemonUnavailable => 3,
            ErrorKind::ClipboardUnavailable => 4,
            ErrorKind::InputPermission => 5,
            ErrorKind::StorageUnavailable => 6,
            ErrorKind::NotFound => 7,
            ErrorKind::UnsupportedContent => 8,
            ErrorKind::PlatformUnavailable => 9,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CoreError {
    pub kind: ErrorKind,
    pub code: String,
    pub message: String,
}

impl CoreError {
    pub fn new(kind: ErrorKind, code: &str, message: impl Into<String>) -> Self {
        CoreError { kind, code: code.to_string(), message: message.into() }
    }

    pub fn not_found(code: &str, message: impl Into<String>) -> Self {
        Self::new(ErrorKind::NotFound, code, message)
    }

    pub fn invalid(code: &str, message: impl Into<String>) -> Self {
        Self::new(ErrorKind::InvalidRequest, code, message)
    }

    pub fn unsupported(code: &str, message: impl Into<String>) -> Self {
        Self::new(ErrorKind::UnsupportedContent, code, message)
    }

    pub fn exit_code(&self) -> i32 {
        self.kind.exit_code()
    }
}

impl fmt::Display for CoreError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.message)
    }
}

impl std::error::Error for CoreError {}

pub type Result<T> = std::result::Result<T, CoreError>;
