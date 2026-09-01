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
            leader_timeout_ms: config.defaults.leader_timeout_ms,
            ..Default::default()
        };

        for binding in &config.leader.bindings {
            match resolve(&binding.action, &binding.args) {
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
