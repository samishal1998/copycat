//! Changing bindings in the config file, in place.
//!
//! This edits the document rather than re-serializing it. A config file is
//! something a person writes and comments; rewriting it from a parsed struct
//! would silently delete every comment and reorder every key the first time
//! anyone changed a binding from the TUI. `toml_edit` keeps the file the user
//! wrote and changes only the table being edited.

use std::path::Path;

use anyhow::{Context, Result};
use copycat_protocol::BindingKind;
use toml_edit::{Array, ArrayOfTables, DocumentMut, InlineTable, Item, Table, Value, value};

use crate::config::{LeaderConfig, SUPPORTED_CONFIG_VERSION};

/// The key a binding of this kind is identified by.
fn trigger_key(kind: BindingKind) -> &'static str {
    match kind {
        // A hotkey is a chord; a leader binding is the key pressed after the
        // leader. The config calls them different things because they are
        // different things (§3.6).
        BindingKind::Hotkey => "trigger",
        BindingKind::Leader => "sequence",
        // Keyed by action instead; handled before this is consulted.
        BindingKind::Tui => "action",
    }
}

/// Add a binding, or replace whatever already sits on that trigger.
pub fn set_binding(
    path: &Path,
    kind: BindingKind,
    trigger: &str,
    action_name: &str,
    args: &serde_json::Value,
) -> Result<()> {
    let mut doc = load(path)?;

    if kind == BindingKind::Tui {
        // A TUI key is keyed by its action, not its trigger: each action has
        // one entry, and setting it replaces whatever keys it had.
        let keymap = tui_keymap_mut(&mut doc)?;
        keymap[action_name] = value(trigger);
        return write(path, &doc);
    }

    let key = trigger_key(kind);
    let tables = bindings_mut(&mut doc, kind)?;
    let existing = tables
        .iter()
        .position(|table| table.get(key).and_then(Item::as_str) == Some(trigger));
    let index = match existing {
        Some(index) => index,
        None => {
            tables.push(Table::new());
            tables.len() - 1
        }
    };
    let table = tables.get_mut(index).expect("index just resolved");
    fill(table, key, trigger, action_name, args);
    write(path, &doc)
}

fn fill(table: &mut Table, key: &str, trigger: &str, action_name: &str, args: &serde_json::Value) {
    table[key] = value(trigger);
    table["action"] = value(action_name);
    match json_to_toml(args) {
        // An empty argument set is written as no key at all, so a binding that
        // takes none reads the way someone would have written it by hand.
        Some(Value::InlineTable(t)) if t.is_empty() => {
            table.remove("args");
        }
        Some(v) => table["args"] = value(v),
        None => {
            table.remove("args");
        }
    }
}

/// Returns whether a binding was actually there to remove.
///
/// For [`BindingKind::Tui`] the `trigger` argument names the *action*, since
/// that is what identifies a keymap entry; removing it restores the default.
pub fn remove_binding(path: &Path, kind: BindingKind, trigger: &str) -> Result<bool> {
    let mut doc = load(path)?;

    if kind == BindingKind::Tui {
        let keymap = tui_keymap_mut(&mut doc)?;
        let removed = keymap.remove(trigger).is_some();
        if removed {
            write(path, &doc)?;
        }
        return Ok(removed);
    }

    let key = trigger_key(kind);

    let tables = bindings_mut(&mut doc, kind)?;
    let index = tables
        .iter()
        .position(|table| table.get(key).and_then(Item::as_str) == Some(trigger));

    let Some(index) = index else { return Ok(false) };
    tables.remove(index);
    // An empty table array is written as nothing at all, and no leader
    // bindings in the file means the defaults: say "none" explicitly.
    if kind == BindingKind::Leader && tables.is_empty() {
        // Removed and re-inserted, not assigned: the table array's key has no
        // spacing around `=`, and would print `bindings= []`.
        let leader = doc["leader"].as_table_mut().expect("bindings_mut made it a table");
        leader.remove("bindings");
        leader.insert("bindings", value(Array::new()));
    }
    write(path, &doc)?;
    Ok(true)
}

/// Change the leader chord, whether the leader is armed at all, how long it
/// waits for each key (`[defaults] leader_timeout_ms`), or how long a sequence
/// that starts a longer one waits (`[defaults] leader_wait_ms`).
///
/// `None` leaves a field alone, so turning the leader off does not also forget
/// which chord it was on.
pub fn set_leader(
    path: &Path,
    trigger: Option<&str>,
    enabled: Option<bool>,
    timeout_ms: Option<u64>,
    wait_ms: Option<u64>,
) -> Result<()> {
    let mut doc = load(path)?;

    let leader = doc.entry("leader").or_insert(Item::Table(Table::new()));
    let leader = leader
        .as_table_mut()
        .context("`leader` in the config is not a table")?;

    if let Some(trigger) = trigger {
        leader["trigger"] = value(trigger);
    }
    if let Some(enabled) = enabled {
        leader["enabled"] = value(enabled);
    }
    for (key, ms) in [("leader_timeout_ms", timeout_ms), ("leader_wait_ms", wait_ms)] {
        let Some(ms) = ms else { continue };
        let defaults = doc.entry("defaults").or_insert(Item::Table(Table::new()));
        let defaults = defaults
            .as_table_mut()
            .context("`defaults` in the config is not a table")?;
        defaults[key] = value(i64::try_from(ms)?);
    }

    write(path, &doc)
}

fn load(path: &Path) -> Result<DocumentMut> {
    let text = match std::fs::read_to_string(path) {
        Ok(text) => text,
        // A first binding should not require the user to have written a config
        // first.
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
            format!("version = {SUPPORTED_CONFIG_VERSION}\n")
        }
        Err(e) => return Err(e).with_context(|| format!("reading {}", path.display())),
    };

    let mut doc: DocumentMut = text
        .parse()
        .with_context(|| format!("{} is not valid TOML", path.display()))?;
    if doc.get("version").is_none() {
        doc["version"] = value(SUPPORTED_CONFIG_VERSION as i64);
    }
    Ok(doc)
}

/// `[ui.tui.keymap]`, created on the way if absent.
fn tui_keymap_mut(doc: &mut DocumentMut) -> Result<&mut Table> {
    let mut node = doc.as_table_mut();
    for name in ["ui", "tui", "keymap"] {
        node = node
            .entry(name)
            .or_insert(Item::Table(Table::new()))
            .as_table_mut()
            .with_context(|| format!("`{name}` in the config is not a table"))?;
    }
    Ok(node)
}

fn bindings_mut(doc: &mut DocumentMut, kind: BindingKind) -> Result<&mut ArrayOfTables> {
    let item = match kind {
        // Not an array of tables; callers route Tui through tui_keymap_mut.
        BindingKind::Tui => anyhow::bail!("TUI keys are not stored as a table array"),
        BindingKind::Hotkey => doc
            .entry("hotkeys")
            .or_insert(Item::ArrayOfTables(ArrayOfTables::new())),
        BindingKind::Leader => {
            let leader = doc.entry("leader").or_insert(Item::Table(Table::new()));
            let leader = leader
                .as_table_mut()
                .context("`leader` in the config is not a table")?;
            // No `bindings` key means the defaults are in force. Write them
            // out first, so an edit changes them rather than replacing all
            // of them with the one binding being edited.
            if !leader.contains_key("bindings") {
                let mut tables = ArrayOfTables::new();
                for binding in LeaderConfig::default().bindings {
                    let mut table = Table::new();
                    let args = serde_json::to_value(&binding.args)?;
                    fill(&mut table, "sequence", &binding.sequence, &binding.action, &args);
                    tables.push(table);
                }
                leader.insert("bindings", Item::ArrayOfTables(tables));
            }
            let item = leader.get_mut("bindings").expect("inserted above");
            // `bindings = []`, left behind when the last one was removed.
            // toml_edit will not convert an empty array, so that case is
            // simply a new table array.
            if item.as_array().is_some_and(Array::is_empty) {
                *item = Item::ArrayOfTables(ArrayOfTables::new());
            }
            item
        }
    };
    item.as_array_of_tables_mut()
        .context("the bindings section of the config is not an array of tables")
}

/// Write through a temporary file, so an interrupted save cannot leave the
/// config half-written and unparseable.
fn write(path: &Path, doc: &DocumentMut) -> Result<()> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)
            .with_context(|| format!("creating {}", parent.display()))?;
    }
    let temp = path.with_extension("toml.new");
    std::fs::write(&temp, doc.to_string())
        .with_context(|| format!("writing {}", temp.display()))?;
    std::fs::rename(&temp, path).with_context(|| format!("replacing {}", path.display()))
}

/// JSON arguments as they arrive over the wire, into TOML.
///
/// `null` means "no arguments" rather than a TOML null, because TOML has none.
fn json_to_toml(json: &serde_json::Value) -> Option<Value> {
    Some(match json {
        serde_json::Value::Null => return None,
        serde_json::Value::Bool(b) => Value::from(*b),
        serde_json::Value::Number(n) => match (n.as_i64(), n.as_f64()) {
            (Some(i), _) => Value::from(i),
            (None, Some(f)) => Value::from(f),
            _ => return None,
        },
        serde_json::Value::String(s) => Value::from(s.as_str()),
        serde_json::Value::Array(items) => {
            Value::Array(items.iter().filter_map(json_to_toml).collect())
        }
        serde_json::Value::Object(map) => {
            let mut table = InlineTable::new();
            for (key, item) in map {
                if let Some(v) = json_to_toml(item) {
                    table.insert(key, v);
                }
            }
            Value::InlineTable(table)
        }
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn temp() -> (tempfile::TempDir, std::path::PathBuf) {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("config.toml");
        (dir, path)
    }

    #[test]
    fn a_binding_can_be_added_to_a_config_that_does_not_exist_yet() {
        let (_dir, path) = temp();
        set_binding(&path, BindingKind::Hotkey, "ctrl+alt+v", "paste.next", &json!(null)).unwrap();

        let text = std::fs::read_to_string(&path).unwrap();
        assert!(text.contains("[[hotkeys]]"), "{text}");
        assert!(text.contains(r#"trigger = "ctrl+alt+v""#), "{text}");
        assert!(!text.contains("args"), "no arguments should mean no args key: {text}");
        crate::config::Config::parse(&text).expect("the result must still load");
    }

    #[test]
    fn comments_and_unrelated_settings_survive_an_edit() {
        // The reason this module exists. Re-serializing a parsed Config would
        // delete every one of these.
        let (_dir, path) = temp();
        std::fs::write(
            &path,
            "version = 1\n\n\
             # keep more history than the default\n\
             [history]\n\
             hot_items = 500  # deliberately large\n",
        )
        .unwrap();

        set_binding(&path, BindingKind::Leader, "s", "stack.start", &json!({"duplicates":"preserve"}))
            .unwrap();

        let text = std::fs::read_to_string(&path).unwrap();
        assert!(text.contains("# keep more history than the default"), "{text}");
        assert!(text.contains("# deliberately large"), "{text}");
        assert!(text.contains("hot_items = 500"), "{text}");
        assert!(text.contains("[[leader.bindings]]"), "{text}");

        let config = crate::config::Config::parse(&text).unwrap();
        assert_eq!(config.history.hot_items, 500);
        assert_eq!(config.leader.bindings.len(), 14, "the defaults, with `s` replaced");
    }

    #[test]
    fn setting_the_same_trigger_twice_replaces_rather_than_duplicates() {
        let (_dir, path) = temp();
        set_binding(&path, BindingKind::Leader, "s", "stack.start", &json!(null)).unwrap();
        set_binding(&path, BindingKind::Leader, "s", "queue.capture", &json!(null)).unwrap();

        let config = crate::config::Config::parse(&std::fs::read_to_string(&path).unwrap()).unwrap();
        let s: Vec<_> = config.leader.bindings.iter().filter(|b| b.sequence == "s").collect();
        assert_eq!(s.len(), 1);
        assert_eq!(s[0].action, "queue.capture");
    }

    #[test]
    fn arguments_are_written_as_an_inline_table_and_read_back() {
        let (_dir, path) = temp();
        set_binding(&path, BindingKind::Hotkey, "ctrl+alt+2", "paste.offset", &json!({"offset":1}))
            .unwrap();

        let text = std::fs::read_to_string(&path).unwrap();
        assert!(text.contains("args = { offset = 1 }"), "{text}");

        let config = crate::config::Config::parse(&text).unwrap();
        let bindings = crate::bindings::Bindings::compile(&config);
        assert!(bindings.rejected.is_empty(), "{:?}", bindings.rejected);
    }

    #[test]
    fn replacing_a_binding_drops_arguments_that_no_longer_apply() {
        let (_dir, path) = temp();
        set_binding(&path, BindingKind::Leader, "s", "stack.start", &json!({"duplicates":"preserve"}))
            .unwrap();
        set_binding(&path, BindingKind::Leader, "s", "queue.seal", &json!(null)).unwrap();

        let text = std::fs::read_to_string(&path).unwrap();
        assert!(!text.contains("duplicates"), "stale arguments must not linger: {text}");
    }

    #[test]
    fn removing_reports_whether_there_was_anything_to_remove() {
        let (_dir, path) = temp();
        set_binding(&path, BindingKind::Hotkey, "ctrl+alt+v", "paste.next", &json!(null)).unwrap();

        assert!(remove_binding(&path, BindingKind::Hotkey, "ctrl+alt+v").unwrap());
        assert!(!remove_binding(&path, BindingKind::Hotkey, "ctrl+alt+v").unwrap());

        let config = crate::config::Config::parse(&std::fs::read_to_string(&path).unwrap()).unwrap();
        assert!(config.hotkeys.is_empty());
    }

    #[test]
    fn editing_the_default_sequences_keeps_the_rest_of_them() {
        let (_dir, path) = temp();
        set_binding(&path, BindingKind::Leader, "x", "queue.seal", &json!(null)).unwrap();
        assert!(remove_binding(&path, BindingKind::Leader, "h").unwrap(), "a default can be removed");

        let text = std::fs::read_to_string(&path).unwrap();
        assert!(text.contains("args = { offset = 8 }"), "{text}");
        let config = crate::config::Config::parse(&text).unwrap();
        let bindings = crate::bindings::Bindings::compile(&config);
        assert!(bindings.rejected.is_empty(), "{:?}", bindings.rejected);
        assert_eq!(bindings.sequences.len(), 14);
        assert!(bindings.sequence("x").is_some() && bindings.sequence("h").is_none());
    }

    #[test]
    fn removing_the_last_sequence_does_not_bring_the_defaults_back() {
        let (_dir, path) = temp();
        std::fs::write(&path, "version = 1\n\n[[leader.bindings]]\nsequence = \"s\"\naction = \"stack.start\"\n")
            .unwrap();
        assert!(remove_binding(&path, BindingKind::Leader, "s").unwrap());

        let text = std::fs::read_to_string(&path).unwrap();
        assert!(text.contains("bindings = []"), "{text}");
        let config = crate::config::Config::parse(&text).unwrap();
        assert!(config.leader.bindings.is_empty(), "{text}");

        // And the empty list can be added to again.
        set_binding(&path, BindingKind::Leader, "q", "queue.capture", &json!(null)).unwrap();
        let config = crate::config::Config::parse(&std::fs::read_to_string(&path).unwrap()).unwrap();
        assert_eq!(config.leader.bindings.len(), 1);
    }

    #[test]
    fn the_leader_can_be_changed_without_disturbing_its_bindings() {
        let (_dir, path) = temp();
        set_binding(&path, BindingKind::Leader, "s", "stack.start", &json!(null)).unwrap();

        set_leader(&path, Some("ctrl+space"), None, None, None).unwrap();

        let config = crate::config::Config::parse(&std::fs::read_to_string(&path).unwrap()).unwrap();
        assert_eq!(config.leader.trigger, "ctrl+space");
        assert_eq!(config.leader.bindings.len(), 14, "the sequences must survive");
        assert!(config.leader.enabled, "and enabled must be left alone");
    }

    #[test]
    fn turning_the_leader_off_remembers_the_chord() {
        let (_dir, path) = temp();
        set_leader(&path, Some("ctrl+alt+space"), None, None, None).unwrap();
        set_leader(&path, None, Some(false), None, None).unwrap();

        let config = crate::config::Config::parse(&std::fs::read_to_string(&path).unwrap()).unwrap();
        assert!(!config.leader.enabled);
        assert_eq!(config.leader.trigger, "ctrl+alt+space");
    }

    #[test]
    fn the_leader_timeout_is_written_under_defaults_keeping_comments() {
        let (_dir, path) = temp();
        std::fs::write(&path, "version = 1\n\n[defaults]\n# mine\nleader_timeout_ms = 1200\n").unwrap();
        set_leader(&path, None, None, Some(7000), None).unwrap();
        set_leader(&path, None, None, None, Some(250)).unwrap();

        let text = std::fs::read_to_string(&path).unwrap();
        assert!(text.contains("# mine"), "{text}");
        let config = crate::config::Config::parse(&text).unwrap();
        assert_eq!(config.defaults.leader_timeout_ms, 7000);
        assert_eq!(config.defaults.leader_wait_ms, 250);
    }

    #[test]
    fn a_tui_key_is_stored_under_its_action_and_removed_by_it() {
        let (_dir, path) = temp();
        set_binding(&path, BindingKind::Tui, "n", "paste_next", &json!(null)).unwrap();
        set_binding(&path, BindingKind::Tui, "N", "paste_next", &json!(null)).unwrap();

        let text = std::fs::read_to_string(&path).unwrap();
        assert!(text.contains("[ui.tui.keymap]"), "{text}");
        assert_eq!(text.matches("paste_next").count(), 1, "replaced, not duplicated: {text}");
        assert!(text.contains(r#"paste_next = "N""#), "{text}");

        assert!(remove_binding(&path, BindingKind::Tui, "paste_next").unwrap());
        assert!(!remove_binding(&path, BindingKind::Tui, "paste_next").unwrap());
        let config = crate::config::Config::parse(&std::fs::read_to_string(&path).unwrap()).unwrap();
        assert!(config.ui.tui.keymap.is_empty());
    }

    #[test]
    fn a_malformed_config_is_refused_rather_than_overwritten() {
        let (_dir, path) = temp();
        std::fs::write(&path, "this is not = = toml\n").unwrap();

        assert!(set_binding(&path, BindingKind::Hotkey, "x", "paste.next", &json!(null)).is_err());
        assert_eq!(std::fs::read_to_string(&path).unwrap(), "this is not = = toml\n");
    }
}
