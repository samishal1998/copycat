//! Configuration: TOML in, validated values out.

use std::path::Path;

use anyhow::{Context, Result};
use copycat_core::{CoreConfig, DuplicatePolicy};
use serde::{Deserialize, Serialize};

/// The highest `version` this binary understands (R19).
pub const SUPPORTED_CONFIG_VERSION: u32 = 1;

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct Config {
    pub version: u32,
    pub history: HistoryConfig,
    pub privacy: PrivacyConfig,
    pub defaults: DefaultsConfig,
    pub platform: PlatformConfig,
    pub leader: LeaderConfig,
    #[serde(rename = "hotkeys")]
    pub hotkeys: Vec<HotkeyBinding>,
    pub ui: UiConfig,
}

impl Default for Config {
    fn default() -> Self {
        Config {
            version: SUPPORTED_CONFIG_VERSION,
            history: HistoryConfig::default(),
            privacy: PrivacyConfig::default(),
            defaults: DefaultsConfig::default(),
            platform: PlatformConfig::default(),
            leader: LeaderConfig::default(),
            hotkeys: Vec::new(),
            ui: UiConfig::default(),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct HistoryConfig {
    pub hot_items: usize,
    pub persist: bool,
    pub retention_days: u32,
    pub capture_images: bool,
    pub capture_files: bool,
    pub capture_html: bool,
    pub max_item_bytes: usize,
    /// How many persisted payloads a search will decrypt before giving up and
    /// reporting truncation (R18).
    pub search_scan_limit: usize,
}

impl Default for HistoryConfig {
    fn default() -> Self {
        HistoryConfig {
            hot_items: 100,
            persist: true,
            retention_days: 90,
            capture_images: false,
            capture_files: false,
            capture_html: true,
            max_item_bytes: 8 * 1024 * 1024,
            search_scan_limit: 2000,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct PrivacyConfig {
    pub encrypt_persistent_payloads: bool,
    pub pause_on_lock_screen: bool,
    /// Permits bounded previews at debug level. Full payload bytes are never
    /// logged regardless of this setting (§23.3).
    pub log_payloads: bool,
    /// Whether a `0600` key file is acceptable when the OS keyring is missing
    /// (ADR-013). With this off and no keyring, the daemon runs memory-only.
    pub allow_key_file_fallback: bool,
}

impl Default for PrivacyConfig {
    fn default() -> Self {
        PrivacyConfig {
            encrypt_persistent_payloads: true,
            // R20: off until a platform implementation exists.
            pause_on_lock_screen: false,
            log_payloads: false,
            allow_key_file_fallback: true,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct DefaultsConfig {
    pub duplicate_policy: DuplicatePolicy,
    pub group_delimiter: String,
    pub leader_timeout_ms: u64,
    pub suppression_window_ms: i64,
}

impl Default for DefaultsConfig {
    fn default() -> Self {
        DefaultsConfig {
            duplicate_policy: DuplicatePolicy::Collapse,
            group_delimiter: "\n".to_string(),
            leader_timeout_ms: 1200,
            suppression_window_ms: copycat_core::DEFAULT_SUPPRESSION_WINDOW_MS,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct PlatformConfig {
    pub watch_interval_ms: u64,
}

impl Default for PlatformConfig {
    fn default() -> Self {
        PlatformConfig { watch_interval_ms: 250 }
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct LeaderConfig {
    pub enabled: bool,
    pub trigger: String,
    pub bindings: Vec<LeaderBinding>,
}

impl Default for LeaderConfig {
    fn default() -> Self {
        LeaderConfig {
            enabled: true,
            trigger: "ctrl+alt+space".to_string(),
            bindings: Vec::new(),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct LeaderBinding {
    pub sequence: String,
    pub action: String,
    #[serde(default = "empty_args")]
    pub args: toml::Value,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct HotkeyBinding {
    pub trigger: String,
    pub action: String,
    #[serde(default = "empty_args")]
    pub args: toml::Value,
}

/// A binding with no arguments carries an empty table rather than a null, so
/// argument handling has one shape instead of two.
fn empty_args() -> toml::Value {
    toml::Value::Table(Default::default())
}

#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct UiConfig {
    pub tui: TuiConfig,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct TuiConfig {
    pub preview_lines: usize,
    pub show_duplicate_runs: bool,
}

impl Default for TuiConfig {
    fn default() -> Self {
        TuiConfig { preview_lines: 3, show_duplicate_runs: true }
    }
}

impl Config {
    /// Load from disk. A missing file is the default configuration, not an
    /// error: Copycat should work before anyone has written one.
    pub fn load(path: &Path) -> Result<Self> {
        match std::fs::read_to_string(path) {
            Ok(text) => Config::parse(&text)
                .with_context(|| format!("reading {}", path.display())),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(Config::default()),
            Err(e) => Err(e).with_context(|| format!("reading {}", path.display())),
        }
    }

    pub fn parse(text: &str) -> Result<Self> {
        // Check the version before the body: a config from a newer Copycat will
        // fail on unknown keys, and "unknown field `foo`" is a much worse
        // message than "written for version 2" (R19).
        #[derive(Deserialize)]
        struct VersionOnly {
            #[serde(default)]
            version: u32,
        }
        let probe: VersionOnly = toml::from_str(text).unwrap_or(VersionOnly { version: 0 });
        if probe.version > SUPPORTED_CONFIG_VERSION {
            anyhow::bail!(
                "config is written for version {} but this build supports up to version {}",
                probe.version,
                SUPPORTED_CONFIG_VERSION
            );
        }

        let config: Config = toml::from_str(text)?;
        config.validate()?;
        Ok(config)
    }

    fn validate(&self) -> Result<()> {
        if self.history.hot_items == 0 {
            anyhow::bail!("history.hot_items must be at least 1");
        }
        if self.history.max_item_bytes == 0 {
            anyhow::bail!("history.max_item_bytes must be at least 1");
        }
        if self.platform.watch_interval_ms == 0 {
            anyhow::bail!("platform.watch_interval_ms must be at least 1");
        }
        if self.defaults.suppression_window_ms < 0 {
            anyhow::bail!("defaults.suppression_window_ms cannot be negative");
        }
        for binding in &self.leader.bindings {
            if binding.sequence.is_empty() {
                anyhow::bail!("a leader binding has an empty sequence");
            }
        }
        Ok(())
    }

    pub fn core(&self) -> CoreConfig {
        CoreConfig {
            hot_items: self.history.hot_items,
            duplicate_policy: self.defaults.duplicate_policy,
            group_delimiter: self.defaults.group_delimiter.clone(),
            suppression_window_ms: self.defaults.suppression_window_ms,
            max_item_bytes: self.history.max_item_bytes,
        }
    }

    /// Media types worth capturing, given the config.
    pub fn wants_media_type(&self, media_type: &str) -> bool {
        match media_type {
            copycat_core::TEXT_PLAIN => true,
            copycat_core::TEXT_HTML => self.history.capture_html,
            t if t.starts_with("image/") => self.history.capture_images,
            "text/uri-list" => self.history.capture_files,
            _ => false,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_missing_file_is_the_default_config() {
        let config = Config::load(Path::new("/nonexistent/copycat.toml")).unwrap();
        assert_eq!(config, Config::default());
    }

    #[test]
    fn the_shipped_example_parses() {
        // Guards against the documented example drifting from the schema.
        let example = include_str!("../../../copycat-package/docs/config.example.toml");
        let config = Config::parse(example).expect("docs/config.example.toml must stay valid");
        assert_eq!(config.leader.bindings.len(), 5);
        assert_eq!(config.hotkeys.len(), 2);
        assert_eq!(config.history.search_scan_limit, 2000);
        assert!(!config.privacy.pause_on_lock_screen, "R20: defaults off");
    }

    #[test]
    fn a_future_version_is_refused_by_version_not_by_unknown_keys() {
        // R19: the message must name the versions, not the first strange key.
        let error = Config::parse("version = 99\nsomething_new = true\n").unwrap_err().to_string();
        assert!(error.contains("version 99"), "{error}");
        assert!(error.contains("version 1"), "{error}");
    }

    #[test]
    fn a_misspelled_key_is_an_error_rather_than_being_ignored() {
        let error = Config::parse("[history]\nhot_itmes = 5\n").unwrap_err().to_string();
        assert!(error.contains("hot_itmes"), "{error}");
    }

    #[test]
    fn nonsense_values_are_rejected() {
        assert!(Config::parse("[history]\nhot_items = 0\n").is_err());
        assert!(Config::parse("[platform]\nwatch_interval_ms = 0\n").is_err());
    }

    #[test]
    fn capture_toggles_gate_media_types() {
        let mut config = Config::default();
        assert!(config.wants_media_type("text/plain"));
        assert!(config.wants_media_type("text/html"));
        assert!(!config.wants_media_type("image/png"));

        config.history.capture_images = true;
        assert!(config.wants_media_type("image/png"));
    }
}
