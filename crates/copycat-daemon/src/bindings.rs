//! Turning configured bindings into daemon actions.
//!
//! Bindings are data (§3.6). A key does not "run stack mode"; it names an
//! action the daemon already exposes over IPC, with arguments. That is what
//! keeps the CLI, the TUI, and a hotkey from growing three different ideas of
//! what a stack is.

use copycat_protocol::{Action, Binding, RejectedBinding, Request};

use crate::config::Config;

/// Ends a sequence the key readers read on past: `p…` is `p` and then one or
/// more keys, read until a pause of `leader_wait_ms` (or a key with no
/// character). Never in a bound sequence: `check_sequence` allows ASCII only.
pub const OPEN: char = '…';

/// An action that takes the keys typed after its sequence: the favourites,
/// bound without `keys`.
fn takes_keys(action: &Action) -> bool {
    matches!(action, Action::PasteFavourite { keys } | Action::FavouriteAssign { keys } if keys.is_empty())
}

#[derive(Debug, Default)]
pub struct Bindings {
    pub leader_trigger: Option<String>,
    pub leader_timeout_ms: u64,
    /// How long a bound sequence that starts a longer one waits for the key
    /// that would continue it.
    pub leader_wait_ms: u64,
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
            // the apps only ever write 0.5-30 s and 0.1-5 s.
            leader_timeout_ms: config.defaults.leader_timeout_ms.clamp(500, 30_000),
            leader_wait_ms: config.defaults.leader_wait_ms.clamp(100, 5000),
            ..Default::default()
        };

        for binding in &config.leader.bindings {
            match check_sequence(&binding.sequence).and_then(|()| resolve(&binding.action, &binding.args)) {
                Ok(action) => bindings.sequences.push((binding.sequence.clone(), action)),
                Err(reason) => bindings.rejected.push(RejectedBinding {
                    trigger: format!("{} {}", config.leader.trigger, binding.sequence),
                    reason,
                }),
            }
        }

        for hotkey in &config.hotkeys {
            match resolve(&hotkey.action, &hotkey.args) {
                Ok(action) if takes_keys(&action) => bindings.rejected.push(RejectedBinding {
                    trigger: hotkey.trigger.clone(),
                    reason: "needs the leader, to type the favourite's keys after it; or give it `keys`".into(),
                }),
                Ok(action) => bindings.hotkeys.push((hotkey.trigger.clone(), action)),
                Err(reason) => bindings.rejected.push(RejectedBinding {
                    trigger: hotkey.trigger.clone(),
                    reason,
                }),
            }
        }

        bindings
    }

    /// The action `key` runs: a bound sequence, or one that takes keys
    /// followed by them (`pa` for a favourite on `p`), with them filled in.
    pub fn sequence(&self, key: &str) -> Option<Action> {
        if let Some((_, action)) = self.sequences.iter().find(|(seq, _)| seq == key) {
            return Some(action.clone());
        }
        self.sequences.iter().find_map(|(seq, action)| {
            let keys = key.strip_prefix(seq.as_str()).filter(|k| !k.is_empty() && takes_keys(action))?.to_string();
            Some(match action {
                Action::PasteFavourite { .. } => Action::PasteFavourite { keys },
                _ => Action::FavouriteAssign { keys },
            })
        })
    }

    /// The bound sequences, for whatever reads keys after the leader; one
    /// that takes keys ends in [`OPEN`].
    pub fn sequence_keys(&self) -> Vec<String> {
        self.sequences
            .iter()
            .map(|(seq, action)| if takes_keys(action) { format!("{seq}{OPEN}") } else { seq.clone() })
            .collect()
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
    /// Not an end: a key was accepted and the sequence goes on. The keys so
    /// far, for whatever shows the sequence being typed.
    Typing(String),
}

/// What the keys typed so far after the leader mean.
#[cfg_attr(windows, allow(dead_code))]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LeaderStep {
    /// A bound sequence no other starts with: fire it.
    Match,
    /// A bound sequence that longer ones start with: fire it unless the next
    /// key, within `leader_wait_ms`, continues it.
    MatchOrLonger,
    /// The start of at least one sequence: keep reading.
    Prefix,
    /// Nothing starts this way: stop now.
    Dead,
}

/// Decided where the key is read, which on macOS is inside the event tap and
/// cannot wait for the server.
#[cfg_attr(windows, allow(dead_code))]
pub fn leader_step(sequences: &[String], typed: &str) -> LeaderStep {
    // An open sequence's keys so far, if `typed` is on its way through it.
    let tail = |head: &str| typed.strip_prefix(head).filter(|keys| keys.chars().all(|c| c.is_ascii_graphic()));
    let bound = sequences.iter().any(|seq| match seq.strip_suffix(OPEN) {
        Some(head) => tail(head).is_some_and(|keys| !keys.is_empty()),
        None => seq == typed,
    });
    let longer = sequences.iter().any(|seq| match seq.strip_suffix(OPEN) {
        Some(head) => tail(head).is_some() || (head.len() > typed.len() && head.starts_with(typed)),
        None => seq.len() > typed.len() && seq.starts_with(typed),
    });
    match (bound, longer) {
        (true, false) => LeaderStep::Match,
        (true, true) => LeaderStep::MatchOrLonger,
        (false, true) => LeaderStep::Prefix,
        (false, false) => LeaderStep::Dead,
    }
}

/// The keys typed after the leader so far, as both key readers hold them.
#[cfg_attr(windows, allow(dead_code))]
#[derive(Debug, Default)]
pub struct LeaderSequence {
    pub typed: String,
    /// `typed` is bound and longer sequences start with it: the reader waits
    /// `leader_wait_ms` rather than the timeout, and then fires it.
    pub pending: bool,
}

#[cfg_attr(windows, allow(dead_code))]
impl LeaderSequence {
    /// Read one key, `None` for a key with no character. `Some` ends the
    /// sequence; `None` keeps reading, with `pending` saying for how long.
    pub fn key(&mut self, sequences: &[String], key: Option<&str>) -> Option<LeaderEnd> {
        // A key with no character (an arrow) ends it as typed.
        let Some(key) = key else { return Some(LeaderEnd::Typed(std::mem::take(&mut self.typed))) };
        let next = format!("{}{key}", self.typed);
        match leader_step(sequences, &next) {
            LeaderStep::Match => Some(LeaderEnd::Typed(next)),
            // Not a longer one after all: fire the shorter, swallow this key.
            LeaderStep::Dead if self.pending => Some(LeaderEnd::Typed(std::mem::take(&mut self.typed))),
            LeaderStep::Dead => Some(LeaderEnd::Typed(next)),
            step => {
                self.pending = step == LeaderStep::MatchOrLonger;
                self.typed = next;
                None
            }
        }
    }

    /// No key came in time: a pending sequence fires, anything else timed out.
    pub fn expired(self) -> LeaderEnd {
        if self.pending { LeaderEnd::Typed(self.typed) } else { LeaderEnd::TimedOut }
    }
}

/// Whether `sequence` can be bound.
///
/// Every character must be one the key readers can produce: the macOS tap
/// knows exactly the printable ASCII keys, and X11 reports no more. One
/// sequence may start another (`s` and `ss`); the shorter then waits
/// `leader_wait_ms` for the key that would make it the longer.
pub fn check_sequence(sequence: &str) -> Result<(), String> {
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
        assert_eq!(bindings.sequences.len(), 7);
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
            Some(Action::StackStart { duplicates: Some(DuplicatePolicy::Preserve) })
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
        assert_eq!(bindings.sequence("q"), Some(Action::QueueCapture { duplicates: None }));
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
        assert_eq!(bindings.sequence("z"), Some(Action::QueueSeal));
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

        let shared = vec!["s".to_string(), "ss".to_string()];
        assert_eq!(leader_step(&shared, "s"), LeaderStep::MatchOrLonger);
        assert_eq!(leader_step(&shared, "ss"), LeaderStep::Match);
    }

    #[test]
    fn a_sequence_with_longer_ones_waits_then_fires() {
        let sequences = vec!["s".to_string(), "ss".to_string(), "gp".to_string()];
        let typed = |keys: &[Option<&str>]| {
            let mut sequence = LeaderSequence::default();
            for key in keys {
                if let Some(end) = sequence.key(&sequences, *key) {
                    return Err(end);
                }
            }
            Ok(sequence)
        };

        let pending = typed(&[Some("s")]).unwrap();
        assert!(pending.pending);
        assert_eq!(pending.expired(), LeaderEnd::Typed("s".into()), "the wait ran out: fire `s`");
        assert_eq!(typed(&[Some("s"), Some("s")]).unwrap_err(), LeaderEnd::Typed("ss".into()));
        assert_eq!(typed(&[Some("s"), Some("x")]).unwrap_err(), LeaderEnd::Typed("s".into()), "x is swallowed");
        assert_eq!(typed(&[Some("s"), None]).unwrap_err(), LeaderEnd::Typed("s".into()));

        let prefix = typed(&[Some("g")]).unwrap();
        assert!(!prefix.pending);
        assert_eq!(prefix.expired(), LeaderEnd::TimedOut);
        assert_eq!(typed(&[Some("g"), Some("x")]).unwrap_err(), LeaderEnd::Typed("gx".into()));
    }

    #[test]
    fn a_favourite_sequence_reads_keys_until_a_pause() {
        let bindings = Bindings::compile(&config_from(
            r#"
            [[leader.bindings]]
            sequence = "p"
            action = "paste.favourite"

            [[leader.bindings]]
            sequence = "gm"
            action = "favourite.assign"

            [[leader.bindings]]
            sequence = "s"
            action = "stack.start"

            [[hotkeys]]
            trigger = "ctrl+alt+f"
            action = "paste.favourite"

            [[hotkeys]]
            trigger = "ctrl+alt+a"
            action = "paste.favourite"
            args = { keys = "a" }
            "#,
        ));
        let sequences = bindings.sequence_keys();
        assert_eq!(sequences, vec![format!("p{OPEN}"), format!("gm{OPEN}"), "s".to_string()]);
        assert_eq!(leader_step(&sequences, "p"), LeaderStep::Prefix);
        assert_eq!(leader_step(&sequences, "pa"), LeaderStep::MatchOrLonger);
        assert_eq!(leader_step(&sequences, "pab"), LeaderStep::MatchOrLonger);
        assert_eq!(leader_step(&sequences, "pa "), LeaderStep::Dead, "a space ends it");
        assert_eq!(leader_step(&sequences, "g"), LeaderStep::Prefix);
        assert_eq!(leader_step(&sequences, "gmx"), LeaderStep::MatchOrLonger);
        assert_eq!(leader_step(&sequences, "x"), LeaderStep::Dead);

        // The reader waits between keys, then fires what was typed.
        let mut sequence = LeaderSequence::default();
        assert_eq!(sequence.key(&sequences, Some("p")), None);
        assert_eq!(sequence.key(&sequences, Some("a")), None);
        assert_eq!(sequence.key(&sequences, Some("b")), None);
        assert!(sequence.pending);
        assert_eq!(sequence.expired(), LeaderEnd::Typed("pab".into()));

        assert_eq!(bindings.sequence("pab"), Some(Action::PasteFavourite { keys: "ab".into() }));
        assert_eq!(bindings.sequence("gmq"), Some(Action::FavouriteAssign { keys: "q".into() }));
        assert_eq!(bindings.sequence("s"), Some(Action::StackStart { duplicates: None }));
        assert_eq!(bindings.sequence("sx"), None);

        // A hotkey cannot read keys after itself; one with `keys` is fine.
        assert_eq!(bindings.rejected.len(), 1);
        assert_eq!(bindings.rejected[0].trigger, "ctrl+alt+f");
        assert_eq!(bindings.hotkeys.len(), 1);
    }

    #[test]
    fn sequences_must_be_readable() {
        assert!(check_sequence("gp").is_ok());
        assert!(check_sequence("").unwrap_err().contains("at least one key"));
        assert!(check_sequence("g p").unwrap_err().contains("whitespace"));
        assert!(check_sequence("é").unwrap_err().contains("cannot be read"));
    }

    #[test]
    fn a_config_may_bind_a_sequence_that_starts_another() {
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
        assert_eq!(bindings.sequence_keys(), vec!["g".to_string(), "gp".to_string()]);
        assert!(bindings.rejected.is_empty(), "{:?}", bindings.rejected);
    }

    #[test]
    fn a_leader_timeout_out_of_range_is_clamped() {
        let timeout = |ms: u64| {
            Bindings::compile(&config_from(&format!("[defaults]\nleader_timeout_ms = {ms}\n"))).leader_timeout_ms
        };
        assert_eq!(timeout(0), 500);
        assert_eq!(timeout(2500), 2500);
        assert_eq!(timeout(600_000), 30_000);

        let wait = |ms: u64| {
            Bindings::compile(&config_from(&format!("[defaults]\nleader_wait_ms = {ms}\n"))).leader_wait_ms
        };
        assert_eq!(wait(0), 100);
        assert_eq!(wait(400), 400);
        assert_eq!(wait(60_000), 5000);
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
