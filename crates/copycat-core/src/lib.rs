//! Copycat's state machine.
//!
//! This crate is the product. Everything else — the daemon, the CLI, the TUI,
//! any future GUI — is an adapter that feeds it events and performs the effects
//! it asks for. It touches no operating system, no database, and no async
//! runtime, so the ordering rules that define the product can be tested
//! exhaustively without a clipboard (§6.2).
//!
//! The shape is deliberately small:
//!
//! * [`History`] is an append-only log of external copies plus the logical
//!   views built over it;
//! * [`Session`] is a cursor over a list of clip ids — stack, queue, and group
//!   differ only in how the list is built and how new copies enter it;
//! * [`Core`] owns both, classifies incoming clipboard changes, and turns
//!   commands into [`PasteRequest`]s that a caller performs and then confirms.
//!
//! The last part is what keeps effects testable: `Core` never writes to a
//! clipboard. It hands back what should be written, and the caller reports
//! whether it worked. A paste that fails does not advance a cursor because the
//! confirmation never arrives, not because of an error path someone remembered
//! to write.

mod clip;
mod core_state;
mod error;
mod history;
mod session;

pub use clip::{
    ClipEvent, ClipId, ClipPayload, ClipSource, ClipSummary, ContentHash, Representation,
    TEXT_HTML, TEXT_PLAIN,
};
pub use core_state::{
    Core, CoreConfig, CoreStatus, Observation, PasteRequest, SessionStarted,
    DEFAULT_SUPPRESSION_WINDOW_MS,
};
pub use error::{CoreError, ErrorKind, Result};
pub use history::{DuplicatePolicy, History, ViewEntry, PREVIEW_CHARS};
pub use session::{Session, SessionMode, SessionState, SessionSummary};
