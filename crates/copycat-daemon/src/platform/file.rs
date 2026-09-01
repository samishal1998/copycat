//! A file-backed clipboard.
//!
//! Selected with `--clipboard file`. Its reason to exist is that a daemon on a
//! machine with no display — CI, a container, this repository's own integration
//! tests — still has to be exercisable end to end. A test writes the file and
//! the watcher notices, exactly as it would notice another application copying.
//!
//! Keeping this a real backend rather than a debug IPC action matters: the
//! production request path stays free of test hooks.

use std::path::PathBuf;

use copycat_core::{ClipPayload, CoreError, ErrorKind, TEXT_PLAIN};

use super::{ClipboardBackend, PasteInjector, Result};

pub struct FileClipboard {
    path: PathBuf,
}

impl FileClipboard {
    pub fn new(path: PathBuf) -> Self {
        FileClipboard { path }
    }
}

fn io_error(error: std::io::Error) -> CoreError {
    CoreError::new(ErrorKind::ClipboardUnavailable, "clipboard_unavailable", format!("{error}"))
}

impl ClipboardBackend for FileClipboard {
    fn read(&mut self) -> Result<ClipPayload> {
        match std::fs::read_to_string(&self.path) {
            Ok(text) => Ok(ClipPayload::text(text)),
            // An absent file is an empty clipboard, not a fault.
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(ClipPayload::default()),
            Err(e) => Err(io_error(e)),
        }
    }

    fn write(&mut self, payload: &ClipPayload) -> Result<()> {
        let text = payload.as_text().ok_or_else(|| {
            CoreError::unsupported("no_text_representation", "this backend only handles text")
        })?;
        std::fs::write(&self.path, text).map_err(io_error)
    }

    /// Modification time and size. Rewriting the same text still moves the
    /// mtime, so this backend can represent a repeat copy — which is what lets
    /// the duplicate policy be tested end to end.
    fn change_token(&mut self) -> Option<u64> {
        let meta = std::fs::metadata(&self.path).ok()?;
        let modified = meta
            .modified()
            .ok()?
            .duration_since(std::time::UNIX_EPOCH)
            .ok()?
            .as_nanos() as u64;
        Some(modified ^ (meta.len().rotate_left(32)))
    }

    fn readable_media_types(&self) -> Vec<String> {
        vec![TEXT_PLAIN.to_string()]
    }

    fn name(&self) -> String {
        format!("file:{}", self.path.display())
    }
}

/// Accepts the paste chord and does nothing with it.
///
/// Paired with [`FileClipboard`] so the full paste transaction — write, inject,
/// confirm — can be exercised where there is no focused application to receive
/// a keystroke.
pub struct NoopInjector;

impl PasteInjector for NoopInjector {
    fn inject(&mut self) -> Result<()> {
        Ok(())
    }

    fn name(&self) -> String {
        "noop (no keystroke is delivered)".into()
    }
}
