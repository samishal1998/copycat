//! The app's own preferences (not the daemon's config): the tray page size,
//! the floating button / mascot / debug overlay flags, and the two looks. One shared entity, saved to a
//! small JSON file on every change, so a relaunch keeps them.
//!
//! The Tauri app keeps the same four in localStorage.

use std::ops::RangeInclusive;
use std::path::PathBuf;

use gpui_kit::*;
use serde::{Deserialize, Serialize};

/// Rows per page the tray dropdown can show.
pub const PAGE_SIZES: RangeInclusive<usize> = 3..=24;

/// The UI scales Settings → Appearance offers.
pub const UI_SCALES: [(f32, &str); 5] = [(0.9, "90%"), (1., "100%"), (1.1, "110%"), (1.25, "125%"), (1.5, "150%")];

/// How long the mascot waits before it sleeps, in minutes; 0 is never.
pub const SLEEP_AFTER: [(u64, &str); 5] = [(1, "1 min"), (3, "3 min"), (10, "10 min"), (30, "30 min"), (0, "Never")];

/// How long the floating panel stays open untouched, in seconds; 0 is never.
pub const CLOSE_AFTER: [(u64, &str); 5] = [(5, "5 s"), (8, "8 s"), (15, "15 s"), (30, "30 s"), (0, "Never")];

/// Set by "Sleep now": every mascot sleeps at once, for a demo.
pub struct Nap;

impl Global for Nap {}

/// The offered scale nearest `scale`.
fn ui_scale(scale: f32) -> f32 {
    UI_SCALES.iter().map(|s| s.0).min_by(|a, b| (a - scale).abs().total_cmp(&(b - scale).abs())).unwrap_or(1.)
}

/// Light or dark: as the system, the opposite of it, or fixed.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum Appearance {
    ReverseSystem,
    Dark,
    Light,
    /// Also any value this build does not know, so one bad key resets only
    /// itself (serde wants that variant last).
    #[default]
    #[serde(other)]
    System,
}

impl Appearance {
    /// The looks the main window offers.
    pub const MAIN: [(Appearance, &'static str); 3] =
        [(Appearance::System, "Auto"), (Appearance::Light, "Light"), (Appearance::Dark, "Dark")];

    /// The floating button's: also the opposite of the system, so it stands
    /// out against the desktop.
    pub const FLOATING: [(Appearance, &'static str); 4] = [
        (Appearance::System, "Auto"),
        (Appearance::Light, "Light"),
        (Appearance::Dark, "Dark"),
        (Appearance::ReverseSystem, "Opposite"),
    ];

    pub fn dark(self, system_dark: bool) -> bool {
        match self {
            Appearance::System => system_dark,
            Appearance::ReverseSystem => !system_dark,
            Appearance::Dark => true,
            Appearance::Light => false,
        }
    }
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct Settings {
    /// History rows per page in the tray dropdown.
    pub page_size: usize,
    /// The floating button: on unless switched off.
    pub floating: bool,
    /// One floating button per display; off unless switched on.
    pub every_screen: bool,
    /// The animated cat in place of the round button; on unless switched off.
    pub mascot: bool,
    /// The leader keys typed so far, in a bubble beside the floating button;
    /// on unless switched off.
    pub leader_bubble: bool,
    /// Minutes with nothing happening before the mascot sleeps; 0 is never.
    pub sleep_after: u64,
    /// The floating panel closes on a click in another app; on unless switched off.
    pub close_on_click_away: bool,
    /// Seconds the floating panel stays open with the pointer away from it; 0 is never.
    pub close_after: u64,
    /// The floating panel closes once something is pasted from it; on unless switched off.
    pub close_on_paste: bool,
    /// The debug overlay.
    pub debug: bool,
    /// The app's windows: main, menu-bar dropdown, debug overlay.
    pub appearance: Appearance,
    /// The floating button and its panel, on their own.
    pub floating_appearance: Appearance,
    /// The size of the main window and the dropdown, one of `UI_SCALES`.
    pub ui_scale: f32,
    /// The size of the floating button, its panel and bubble, one of `UI_SCALES`.
    pub floating_scale: f32,
}

impl Default for Settings {
    fn default() -> Self {
        Settings {
            page_size: 6,
            floating: true,
            every_screen: false,
            mascot: true,
            leader_bubble: true,
            sleep_after: 3,
            close_on_click_away: true,
            close_after: 8,
            close_on_paste: true,
            debug: false,
            appearance: Appearance::System,
            floating_appearance: Appearance::System,
            ui_scale: 1.,
            floating_scale: 1.,
        }
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
    /// Whether the floating panel closes by itself in any way (it then has a pin).
    pub fn auto_closes(&self) -> bool {
        self.close_on_click_away || self.close_after > 0 || self.close_on_paste
    }

    /// Parse a saved file; a missing or broken one is the defaults. The page
    /// size is pulled into range: the dropdown divides by it.
    fn parse(text: &str) -> Settings {
        let mut settings: Settings = serde_json::from_str(text).unwrap_or_default();
        settings.page_size = settings.page_size.clamp(*PAGE_SIZES.start(), *PAGE_SIZES.end());
        settings.ui_scale = ui_scale(settings.ui_scale);
        settings.floating_scale = ui_scale(settings.floating_scale);
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
        self.ui_scale = ui_scale(self.ui_scale);
        self.floating_scale = ui_scale(self.floating_scale);
        // ponytail: a tiny file written on the UI thread; a failed write only
        // loses the preference, so it is ignored.
        if let Some(path) = path() {
            let _ = path.parent().map(std::fs::create_dir_all);
            let _ = serde_json::to_string_pretty(self).map(|json| std::fs::write(path, json));
        }
        cx.notify();
        // A look may have changed, and a window draws in it only when it redraws.
        cx.refresh_windows();
    }
}

#[cfg(test)]
mod tests {
    use super::{Appearance, Settings, UI_SCALES};

    #[test]
    fn a_saved_file_round_trips_and_a_bad_one_is_the_defaults() {
        let saved = Settings {
            page_size: 9,
            floating: false,
            every_screen: true,
            mascot: false,
            leader_bubble: false,
            sleep_after: 0,
            close_on_click_away: false,
            close_after: 30,
            close_on_paste: false,
            debug: true,
            appearance: Appearance::Light,
            floating_appearance: Appearance::ReverseSystem,
            ui_scale: 1.25,
            floating_scale: 1.5,
        };
        let json = serde_json::to_string(&saved).unwrap();
        assert_eq!(Settings::parse(&json), saved);
        assert_eq!(Settings::parse("not json"), Settings::default());
        // Missing keys fall back one by one; the page size cannot reach 0.
        assert_eq!(Settings::parse(r#"{"debug":true,"page_size":0}"#).page_size, 3);
        assert!(Settings::parse(r#"{"debug":true}"#).floating);
        assert!(Settings::parse(r#"{"debug":true}"#).mascot);
        assert!(Settings::parse(r#"{"debug":true}"#).leader_bubble);
        assert_eq!(Settings::parse(r#"{"debug":true}"#).sleep_after, 3);
        let old = Settings::parse(r#"{"debug":true}"#);
        assert!(old.close_on_click_away && old.close_on_paste && old.close_after == 8 && old.auto_closes());
        assert_eq!(Settings::parse(r#"{"floating_appearance":"reverse-system"}"#).floating_appearance, Appearance::ReverseSystem);
        assert!(Settings::parse(r#"{"debug":true,"appearance":"bogus"}"#).debug);
        // The scale is one of the offered steps.
        assert_eq!(Settings::parse(r#"{"debug":true}"#).ui_scale, 1.);
        for (scale, _) in UI_SCALES {
            assert_eq!(Settings::parse(&format!(r#"{{"ui_scale":{scale}}}"#)).ui_scale, scale);
        }
        assert_eq!(Settings::parse(r#"{"ui_scale":1.3}"#).ui_scale, 1.25);
        assert_eq!(Settings::parse(r#"{"ui_scale":0}"#).ui_scale, 0.9);
        assert_eq!(Settings::parse(r#"{"ui_scale":9}"#).ui_scale, 1.5);
        assert_eq!(Settings::parse(r#"{"debug":true}"#).floating_scale, 1.);
        assert_eq!(Settings::parse(r#"{"floating_scale":1.2}"#).floating_scale, 1.25);
        assert!(Appearance::ReverseSystem.dark(false) && !Appearance::ReverseSystem.dark(true));
        assert!(Appearance::System.dark(true) && Appearance::Dark.dark(false) && !Appearance::Light.dark(true));
    }
}
