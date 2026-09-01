//! The daemon's local-socket protocol.
//!
//! Every interface — CLI, TUI, keyboard bindings, any future GUI — reaches the
//! daemon through this crate and nothing else. That is the point of ADR-003:
//! if a client could reimplement clipboard semantics locally, two interfaces
//! would eventually disagree about what "next" means.
//!
//! The wire format favours inspectability over compactness (§8). A frame is a
//! four-byte big-endian length followed by JSON, so `socat` and a human are
//! adequate debugging tools.
//!
//! ```text
//! {"version":1,"id":"req-1","action":"stack.start","args":{"duplicates":"collapse"}}
//! ```
//!
//! This crate deliberately depends on `copycat-core` and serde and nothing
//! else. A CLI must not link SQLite or a clipboard backend to ask the daemon a
//! question.

mod client;
mod frame;
mod message;
mod report;

pub use client::{APP_DIR, call, default_socket_path, is_running, request};
pub use frame::{FrameError, MAX_FRAME_BYTES, read_frame, write_frame};
pub use message::{
    Action, Binding, BindingKind, Outcome, RejectedBinding, Request, Response, ResultBody, PROTOCOL_VERSION,
};
pub use report::{Capability, CheckStatus, DoctorCheck, DoctorReport, StatusReport};
