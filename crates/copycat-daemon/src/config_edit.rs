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
use toml_edit::{ArrayOfTables, DocumentMut, InlineTable, Item, Table, Value, value};

use crate::config::SUPPORTED_CONFIG_VERSION;

/// The key a binding of this kind is identified by.
fn trigger_key(kind: BindingKind) -> &'static str {
    match kind {
        // A hotkey is a chord; a leader binding is the key pressed after the
        // leader. The config calls them different things because they are
        // different things (§3.6).
        BindingKind::Hotkey => "trigger",
        BindingKind::Leader => "sequence",
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

    write(path, &doc)
}

/// Returns whether a binding was actually there to remove.
pub fn remove_binding(path: &Path, kind: BindingKind, trigger: &str) -> Result<bool> {
    let mut doc = load(path)?;
    let key = trigger_key(kind);

    let tables = bindings_mut(&mut doc, kind)?;
    let index = tables
        .iter()
        .position(|table| table.get(key).and_then(Item::as_str) == Some(trigger));

    let Some(index) = index else { return Ok(false) };
    tables.remove(index);
    write(path, &doc)?;
    Ok(true)
}

/// Change the leader chord, or whether the leader is armed at all.
///
/// `None` leaves a field alone, so turning the leader off does not also forget
/// which chord it was on.
pub fn set_leader(path: &Path, trigger: Option<&str>, enabled: Option<bool>) -> Result<()> {
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

fn bindings_mut(doc: &mut DocumentMut, kind: BindingKind) -> Result<&mut ArrayOfTables> {
    let item = match kind {
        BindingKind::Hotkey => doc
            .entry("hotkeys")
            .or_insert(Item::ArrayOfTables(ArrayOfTables::new())),
        BindingKind::Leader => {
            let leader = doc.entry("leader").or_insert(Item::Table(Table::new()));
            let leader = leader
                .as_table_mut()
                .context("`leader` in the config is not a table")?;
            leader
                .entry("bindings")
                .or_insert(Item::ArrayOfTables(ArrayOfTables::new()))
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
        assert_eq!(config.leader.bindings.len(), 1);
    }

    #[test]
    fn setting_the_same_trigger_twice_replaces_rather_than_duplicates() {
        let (_dir, path) = temp();
        set_binding(&path, BindingKind::Leader, "s", "stack.start", &json!(null)).unwrap();
        set_binding(&path, BindingKind::Leader, "s", "queue.capture", &json!(null)).unwrap();

        let config = crate::config::Config::parse(&std::fs::read_to_string(&path).unwrap()).unwrap();
        assert_eq!(config.leader.bindings.len(), 1);
        assert_eq!(config.leader.bindings[0].action, "queue.capture");
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
    fn the_leader_can_be_changed_without_disturbing_its_bindings() {
        let (_dir, path) = temp();
        set_binding(&path, BindingKind::Leader, "s", "stack.start", &json!(null)).unwrap();

        set_leader(&path, Some("ctrl+space"), None).unwrap();

        let config = crate::config::Config::parse(&std::fs::read_to_string(&path).unwrap()).unwrap();
        assert_eq!(config.leader.trigger, "ctrl+space");
        assert_eq!(config.leader.bindings.len(), 1, "the sequences must survive");
        assert!(config.leader.enabled, "and enabled must be left alone");
    }

    #[test]
    fn turning_the_leader_off_remembers_the_chord() {
        let (_dir, path) = temp();
        set_leader(&path, Some("ctrl+alt+space"), None).unwrap();
        set_leader(&path, None, Some(false)).unwrap();

        let config = crate::config::Config::parse(&std::fs::read_to_string(&path).unwrap()).unwrap();
        assert!(!config.leader.enabled);
        assert_eq!(config.leader.trigger, "ctrl+alt+space");
    }

    #[test]
    fn a_malformed_config_is_refused_rather_than_overwritten() {
        let (_dir, path) = temp();
        std::fs::write(&path, "this is not = = toml\n").unwrap();

        assert!(set_binding(&path, BindingKind::Hotkey, "x", "paste.next", &json!(null)).is_err());
        assert_eq!(std::fs::read_to_string(&path).unwrap(), "this is not = = toml\n");
    }
}
