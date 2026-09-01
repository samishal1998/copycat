//! Status and diagnostics.
//!
//! `doctor` exists because the honest answer to "why did nothing happen when I
//! pressed the key" is usually a missing permission or an unsupported
//! compositor, and a daemon that fails silently makes that unanswerable
//! (ADR-008).

use copycat_core::CoreStatus;
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct StatusReport {
    pub protocol_version: u32,
    pub daemon_version: String,
    pub uptime_ms: u64,
    pub core: CoreStatus,
    /// A preview of what is actually on the system clipboard right now.
    ///
    /// After a Copycat paste this deliberately differs from `core.latest`
    /// (R15). Reporting both is what keeps that from being a surprise.
    pub os_clipboard: Option<String>,
    pub clipboard_backend: String,
    pub watch_interval_ms: u64,
    pub key_storage: String,
    pub persistence: String,
    pub socket_path: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CheckStatus {
    Ok,
    /// Working, but not the way the user probably assumes.
    Degraded,
    Unavailable,
}

impl CheckStatus {
    pub fn glyph(self) -> &'static str {
        match self {
            CheckStatus::Ok => "ok",
            CheckStatus::Degraded => "degraded",
            CheckStatus::Unavailable => "unavailable",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DoctorCheck {
    pub name: String,
    pub status: CheckStatus,
    pub detail: String,
}

impl DoctorCheck {
    pub fn ok(name: &str, detail: impl Into<String>) -> Self {
        DoctorCheck { name: name.into(), status: CheckStatus::Ok, detail: detail.into() }
    }

    pub fn degraded(name: &str, detail: impl Into<String>) -> Self {
        DoctorCheck { name: name.into(), status: CheckStatus::Degraded, detail: detail.into() }
    }

    pub fn unavailable(name: &str, detail: impl Into<String>) -> Self {
        DoctorCheck { name: name.into(), status: CheckStatus::Unavailable, detail: detail.into() }
    }
}

/// A platform capability, named whether or not it is present.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Capability {
    pub name: String,
    pub available: bool,
    pub detail: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DoctorReport {
    pub daemon_version: String,
    pub platform: String,
    /// `x11`, `wayland`, `windows`, `macos`, or `headless`.
    pub display_server: String,
    /// Whether this platform has passed the adapter contract suite (§20.2).
    pub platform_support: String,
    pub checks: Vec<DoctorCheck>,
    pub capabilities: Vec<Capability>,
}

impl DoctorReport {
    /// True when nothing is outright missing.
    pub fn healthy(&self) -> bool {
        self.checks.iter().all(|c| c.status != CheckStatus::Unavailable)
    }
}
