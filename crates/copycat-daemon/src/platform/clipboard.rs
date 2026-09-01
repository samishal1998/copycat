//! The real system clipboard, via `arboard`.
//!
//! Text only, deliberately. §4.1 makes plain text the v0.1 requirement and
//! `arboard` can *write* HTML but not read it back, so claiming HTML capture
//! would mean recording something the user never copied. The data model already
//! carries multiple representations (§5.2), so adding a richer backend later is
//! a backend change, not a schema change. `doctor` reports what is readable.

use copycat_core::{ClipPayload, CoreError, ErrorKind, TEXT_PLAIN};

use super::{ClipboardBackend, Result};

pub struct SystemClipboard {
    inner: arboard::Clipboard,
}

impl SystemClipboard {
    pub fn new() -> Result<Self> {
        arboard::Clipboard::new()
            .map(|inner| SystemClipboard { inner })
            .map_err(|e| unavailable(format!("{e}")))
    }
}

fn unavailable(detail: String) -> CoreError {
    CoreError::new(ErrorKind::ClipboardUnavailable, "clipboard_unavailable", detail)
}

impl ClipboardBackend for SystemClipboard {
    fn read(&mut self) -> Result<ClipPayload> {
        match self.inner.get_text() {
            Ok(text) => Ok(ClipPayload::text(text)),
            // An image or a file list on the clipboard is not an error: it is a
            // value this backend cannot represent, and the watcher should carry
            // on rather than log a failure every poll.
            Err(arboard::Error::ContentNotAvailable) => Ok(ClipPayload::default()),
            Err(e) => Err(unavailable(format!("{e}"))),
        }
    }

    fn write(&mut self, payload: &ClipPayload) -> Result<()> {
        let text = payload.as_text().ok_or_else(|| {
            CoreError::unsupported(
                "no_text_representation",
                "this backend can only write text",
            )
        })?;
        self.inner.set_text(text).map_err(|e| unavailable(format!("{e}")))
    }

    fn readable_media_types(&self) -> Vec<String> {
        vec![TEXT_PLAIN.to_string()]
    }

    fn name(&self) -> String {
        "arboard".into()
    }
}
