//! Turning configured bindings into daemon actions.
//!
//! Bindings are data (§3.6). A key does not "run stack mode"; it names an
//! action the daemon already exposes over IPC, with arguments. That is what
//! keeps the CLI, the TUI, and a hotkey from growing three different ideas of
//! what a stack is.

use copycat_protocol::{Action, Binding, RejectedBinding, Request};

use crate::config::Config;

#[derive(Debug, Default)]
pub struct Bindings {
    pub leader_trigger: Option<String>,
    pub leader_timeout_ms: u64,
    /// Key sequence to action, resolved.
    pub sequences: Vec<(String, Action)>,
    pub hotkeys: Vec<(String, Action)>,
    pub rejected: Vec<RejectedBinding>,
}

impl Bindings {
    pub fn compile(config: &Config) -> Self {
        let mut bindings = Bindings {
            leader_trigger: config
                .leader
                .enabled
                .then(|| config.leader.trigger.clone()),
            // Hand-edited configs are not range-checked on load; the CLI and
            // the apps only ever write 0.5-30 s.
            leader_timeout_ms: config.defaults.leader_timeout_ms.clamp(500, 30_000),
            ..Default::default()
        };

        for binding in &config.leader.bindings {
            let checked = check_sequence(
                &binding.sequence,
                bindings.sequences.iter().map(|(seq, _)| seq.as_str()),
            );
            match checked.and_then(|()| resolve(&binding.action, &binding.args)) {
                Ok(action) => bindings.sequences.push((binding.sequence.clone(), action)),
                Err(reason) => bindings.rejected.push(RejectedBinding {
                    trigger: format!("{} {}", config.leader.trigger, binding.sequence),
                    reason,
                }),
            }
        }

        for hotkey in &config.hotkeys {
            match resolve(&hotkey.action, &hotkey.args) {
                Ok(action) => bindings.hotkeys.push((hotkey.trigger.clone(), action)),
                Err(reason) => bindings.rejected.push(RejectedBinding {
                    trigger: hotkey.trigger.clone(),
                    reason,
                }),
            }
        }

        bindings
    }

    pub fn sequence(&self, key: &str) -> Option<&Action> {
        self.sequences.iter().find(|(seq, _)| seq == key).map(|(_, action)| action)
    }

    /// The bound sequences, for whatever reads keys after the leader.
    pub fn sequence_keys(&self) -> Vec<String> {
        self.sequences.iter().map(|(seq, _)| seq.clone()).collect()
    }

    pub fn describe(&self) -> (Vec<Binding>, Vec<Binding>) {
        let render = |list: &Vec<(String, Action)>| {
            list.iter()
                .map(|(trigger, action)| {
                    let value = serde_json::to_value(action).unwrap_or(serde_json::Value::Null);
                    Binding {
                        trigger: trigger.clone(),
                        action: value
                            .get("action")
                            .and_then(|a| a.as_str())
                            .unwrap_or("?")
                            .to_string(),
                        args: value.get("args").cloned().unwrap_or(serde_json::Value::Null),
                    }
                })
                .collect::<Vec<_>>()
        };
        (render(&self.sequences), render(&self.hotkeys))
    }
}

/// How a leader sequence ended, as the key reader reports it.
// Windows has no key reader yet, so only the timeout is ever reported there.
#[cfg_attr(windows, allow(dead_code))]
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum LeaderEnd {
    /// The keys typed after the leader: a bound sequence, or keys no sequence
    /// continues with. A key with no character ends it with what came before.
    Typed(String),
    /// Too long passed before the next key.
    TimedOut,
    /// Esc.
    Cancelled,
}

/// What the keys typed so far after the leader mean.
#[cfg_attr(windows, allow(dead_code))]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LeaderStep {
    /// A bound sequence: fire it.
    Match,
    /// The start of at least one sequence: keep reading.
    Prefix,
    /// Nothing starts this way: stop now.
    Dead,
}

/// Decided where the key is read, which on macOS is inside the event tap and
/// cannot wait for the server.
#[cfg_attr(windows, allow(dead_code))]
pub fn leader_step(sequences: &[String], typed: &str) -> LeaderStep {
    if sequences.iter().any(|seq| seq == typed) {
        LeaderStep::Match
    } else if sequences.iter().any(|seq| seq.starts_with(typed)) {
        LeaderStep::Prefix
    } else {
        LeaderStep::Dead
    }
}

/// Whether `sequence` can be bound alongside `others`.
///
/// Every character must be one the key readers can produce: the macOS tap
/// knows exactly the printable ASCII keys, and X11 reports no more. And no
/// sequence may start another, or the shorter would fire before the longer
/// could be finished. The same sequence in `others` is the binding being
/// replaced, not a conflict.
pub fn check_sequence<'a>(
    sequence: &str,
    others: impl IntoIterator<Item = &'a str>,
) -> Result<(), String> {
    if sequence.is_empty() {
        return Err("a leader sequence needs at least one key".into());
    }
    if let Some(c) = sequence.chars().find(|c| !c.is_ascii_graphic()) {
        return Err(if c.is_whitespace() {
            format!("`{sequence}` contains whitespace; a sequence is keys typed one after another, like `gp`")
        } else {
            format!("`{c}` cannot be read after the leader; sequences use ASCII letters, digits and punctuation")
        });
    }
    for other in others {
        if other != sequence && (other.starts_with(sequence) || sequence.starts_with(other)) {
            return Err(format!(
                "`{sequence}` conflicts with `{other}`: one starts the other, so the longer could never be typed"
            ));
        }
    }
    Ok(())
}

/// Resolve an action name and TOML arguments into a typed [`Action`].
///
/// This goes through the wire format on purpose: a binding and an IPC request
/// must accept exactly the same action names and arguments, and the only way to
/// guarantee that is to use the same parser.
pub(crate) fn resolve(action: &str, args: &toml::Value) -> Result<Action, String> {
    let args = serde_json::to_value(args).map_err(|e| e.to_string())?;

    let mut envelope = serde_json::Map::new();
    envelope.insert("version".into(), copycat_protocol::PROTOCOL_VERSION.into());
    envelope.insert("id".into(), "binding".into());
    envelope.insert("action".into(), action.into());
    envelope.insert("args".into(), args);

    serde_json::from_value::<Request>(envelope.into())
        .map(|request| request.action)
        .map_err(|e| format!("{e}"))
}

#[cfg(test)]
mod tests {
    use super::*;
    use copycat_core::DuplicatePolicy;

    fn config_from(toml_text: &str) -> Config {
        Config::parse(toml_text).unwrap()
    }

    #[test]
    fn the_example_config_compiles_to_real_actions() {
        let example = include_str!("../../../copycat-package/docs/config.example.toml");
        let bindings = Bindings::compile(&config_from(example));

        assert!(
            bindings.rejected.is_empty(),
            "the shipped example must not contain unusable bindings: {:?}",
            bindings.rejected
        );
        assert_eq!(bindings.sequences.len(), 5);
        assert_eq!(bindings.hotkeys.len(), 2);
    }

    #[test]
    fn arguments_reach_the_action() {
        let bindings = Bindings::compile(&config_from(
            r#"
            [[leader.bindings]]
            sequence = "S"
            action = "stack.start"
            args = { duplicates = "preserve" }
            "#,
        ));
        assert_eq!(
            bindings.sequence("S"),
            Some(&Action::StackStart { duplicates: Some(DuplicatePolicy::Preserve) })
        );
    }

    #[test]
    fn a_binding_with_no_arguments_is_fine() {
        let bindings = Bindings::compile(&config_from(
            r#"
            [[leader.bindings]]
            sequence = "q"
            action = "queue.capture"
            "#,
        ));
        assert_eq!(bindings.sequence("q"), Some(&Action::QueueCapture { duplicates: None }));
    }

    #[test]
    fn an_action_with_no_arguments_at_all_can_be_bound_from_config() {
        // A config binding always sends an empty args table. Unit actions
        // like queue.seal used to reject that shape, so this binding was
        // silently unusable.
        let bindings = Bindings::compile(&config_from(
            r#"
            [[leader.bindings]]
            sequence = "z"
            action = "queue.seal"
            "#,
        ));
        assert!(bindings.rejected.is_empty(), "{:?}", bindings.rejected);
        assert_eq!(bindings.sequence("z"), Some(&Action::QueueSeal));
    }

    #[test]
    fn an_unknown_action_is_rejected_with_a_reason_not_ignored() {
        // A typo in a config file must be visible in `bind list`, not a key
        // that silently does nothing.
        let bindings = Bindings::compile(&config_from(
            r#"
            [[leader.bindings]]
            sequence = "x"
            action = "stack.startt"
            "#,
        ));
        assert!(bindings.sequences.is_empty());
        assert_eq!(bindings.rejected.len(), 1);
        assert!(bindings.rejected[0].trigger.ends_with(" x"));
    }

    #[test]
    fn a_missing_required_argument_is_rejected() {
        let bindings = Bindings::compile(&config_from(
            r#"
            [[hotkeys]]
            trigger = "ctrl+alt+q"
            action = "queue.start"
            "#,
        ));
        assert_eq!(bindings.rejected.len(), 1, "queue.start needs `last`");
    }

    #[test]
    fn leader_keys_fire_wait_or_stop() {
        let sequences = vec!["s".to_string(), "gp".to_string(), "gx".to_string()];
        assert_eq!(leader_step(&sequences, "s"), LeaderStep::Match);
        assert_eq!(leader_step(&sequences, "g"), LeaderStep::Prefix);
        assert_eq!(leader_step(&sequences, "gp"), LeaderStep::Match);
        assert_eq!(leader_step(&sequences, "gq"), LeaderStep::Dead);
        assert_eq!(leader_step(&sequences, "S"), LeaderStep::Dead, "case is not folded");
        assert_eq!(leader_step(&[], "s"), LeaderStep::Dead);
    }

    #[test]
    fn sequences_must_be_readable_and_not_start_one_another() {
        assert!(check_sequence("gp", ["s", "S", "gp"]).is_ok(), "the same sequence is a replace");
        assert!(check_sequence("", []).unwrap_err().contains("at least one key"));
        assert!(check_sequence("g p", []).unwrap_err().contains("whitespace"));
        assert!(check_sequence("é", []).unwrap_err().contains("cannot be read"));
        assert!(check_sequence("gp", ["g"]).unwrap_err().contains("`g`"));
        assert!(check_sequence("g", ["gp"]).unwrap_err().contains("`gp`"));
    }

    #[test]
    fn a_config_with_a_sequence_starting_another_rejects_the_later_one() {
        let bindings = Bindings::compile(&config_from(
            r#"
            [[leader.bindings]]
            sequence = "g"
            action = "queue.seal"

            [[leader.bindings]]
            sequence = "gp"
            action = "queue.seal"
            "#,
        ));
        assert_eq!(bindings.sequence_keys(), vec!["g".to_string()]);
        assert_eq!(bindings.rejected.len(), 1);
        assert!(bindings.rejected[0].reason.contains("`g`"), "{:?}", bindings.rejected);
    }

    #[test]
    fn a_leader_timeout_out_of_range_is_clamped() {
        let timeout = |ms: u64| {
            Bindings::compile(&config_from(&format!("[defaults]\nleader_timeout_ms = {ms}\n"))).leader_timeout_ms
        };
        assert_eq!(timeout(0), 500);
        assert_eq!(timeout(2500), 2500);
        assert_eq!(timeout(600_000), 30_000);
    }

    #[test]
    fn disabling_the_leader_drops_its_trigger_but_keeps_the_sequences_listed() {
        let bindings = Bindings::compile(&config_from(
            r#"
            [leader]
            enabled = false

            [[leader.bindings]]
            sequence = "s"
            action = "stack.start"
            "#,
        ));
        assert!(bindings.leader_trigger.is_none());
        assert_eq!(bindings.sequences.len(), 1);
    }
}
