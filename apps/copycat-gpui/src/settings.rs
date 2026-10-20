//! The app's own preferences (not the daemon's config): the tray page size and
//! the floating button / debug overlay flags. One shared entity, saved to a
//! small JSON file on every change, so a relaunch keeps them.
//!
//! The Tauri app keeps the same four in localStorage.

use std::ops::RangeInclusive;
use std::path::PathBuf;

use gpui_kit::*;
use serde::{Deserialize, Serialize};

/// Rows per page the tray dropdown can show.
pub const PAGE_SIZES: RangeInclusive<usize> = 3..=24;

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct Settings {
    /// History rows per page in the tray dropdown.
    pub page_size: usize,
    /// The floating button: on unless switched off.
    pub floating: bool,
    /// One floating button per display; off unless switched on.
    pub every_screen: bool,
    /// The debug overlay.
    pub debug: bool,
}

impl Default for Settings {
    fn default() -> Self {
        Settings { page_size: 6, floating: true, every_screen: false, debug: false }
    }
}

struct SharedSettings(Entity<Settings>);

impl Global for SharedSettings {}

fn path() -> Option<PathBuf> {
    let home = PathBuf::from(std::env::var_os("HOME")?);
    let base = if cfg!(target_os = "macos") {
        home.join("Library/Application Support")
    } else {
        std::env::var_os("XDG_CONFIG_HOME").map(PathBuf::from).unwrap_or_else(|| home.join(".config"))
    };
    Some(base.join("copycat/gui.json"))
}

impl Settings {
    /// Parse a saved file; a missing or broken one is the defaults. The page
    /// size is pulled into range: the dropdown divides by it.
    fn parse(text: &str) -> Settings {
        let mut settings: Settings = serde_json::from_str(text).unwrap_or_default();
        settings.page_size = settings.page_size.clamp(*PAGE_SIZES.start(), *PAGE_SIZES.end());
        settings
    }

    /// Load the file and share it. Once, before any window opens.
    pub fn start(cx: &mut App) {
        let loaded = path().and_then(|p| std::fs::read_to_string(p).ok()).map(|t| Self::parse(&t));
        let entity = cx.new(|_| loaded.unwrap_or_default());
        cx.set_global(SharedSettings(entity));
    }

    pub fn get(cx: &App) -> Entity<Self> {
        cx.global::<SharedSettings>().0.clone()
    }

    /// Edit, keep in range, save, and tell every observer.
    pub fn change(&mut self, cx: &mut Context<Self>, edit: impl FnOnce(&mut Self)) {
        edit(self);
        self.page_size = self.page_size.clamp(*PAGE_SIZES.start(), *PAGE_SIZES.end());
        // ponytail: a tiny file written on the UI thread; a failed write only
        // loses the preference, so it is ignored.
        if let Some(path) = path() {
            let _ = path.parent().map(std::fs::create_dir_all);
            let _ = serde_json::to_string_pretty(self).map(|json| std::fs::write(path, json));
        }
        cx.notify();
    }
}

#[cfg(test)]
mod tests {
    use super::Settings;

    #[test]
    fn a_saved_file_round_trips_and_a_bad_one_is_the_defaults() {
        let saved = Settings { page_size: 9, floating: false, every_screen: true, debug: true };
        let json = serde_json::to_string(&saved).unwrap();
        assert_eq!(Settings::parse(&json), saved);
        assert_eq!(Settings::parse("not json"), Settings::default());
        // Missing keys fall back one by one; the page size cannot reach 0.
        assert_eq!(Settings::parse(r#"{"debug":true,"page_size":0}"#).page_size, 3);
        assert!(Settings::parse(r#"{"debug":true}"#).floating);
    }
}
