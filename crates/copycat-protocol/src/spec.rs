//! What each bindable action is called, what it takes, and what the values
//! can be — the schema a form needs so nobody has to type `queue.start` or
//! `{"last":5}` from memory.
//!
//! Serde cannot describe an enum at run time, so this is written by hand and
//! kept honest by a test that builds a request from every spec's defaults and
//! checks it deserializes into a real [`Action`](crate::Action). An action
//! that is not here cannot be bound to a key: the rest need an id or a query,
//! or only make sense from a terminal.

/// How an argument is edited and validated.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ArgKind {
    /// One of a fixed set of words.
    Enum(&'static [&'static str]),
    /// A whole number, zero or more.
    Int,
    Bool,
    Text,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ArgSpec {
    pub name: &'static str,
    pub kind: ArgKind,
    /// A required argument has no default; the daemon refuses the binding
    /// without it.
    pub required: bool,
    pub summary: &'static str,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ActionSpec {
    pub name: &'static str,
    pub summary: &'static str,
    pub args: &'static [ArgSpec],
}

const DUPLICATES: ArgSpec = ArgSpec {
    name: "duplicates",
    kind: ArgKind::Enum(&["collapse", "preserve"]),
    required: false,
    summary: "fold adjacent repeat copies, or keep every one",
};

const RAW: ArgSpec = ArgSpec {
    name: "raw",
    kind: ArgKind::Bool,
    required: false,
    summary: "index the raw log rather than the collapsed view",
};

const DELIMITER: ArgSpec = ArgSpec {
    name: "delimiter",
    kind: ArgKind::Text,
    required: false,
    summary: "text placed between entries; a newline if unset",
};

/// Every daemon action a key can be bound to, in the order a picker shows them.
pub const BINDABLE_ACTIONS: &[ActionSpec] = &[
    ActionSpec {
        name: "paste.mode",
        summary: "whatever the paste chord means now: pop, advance, or paste the group",
        args: &[ArgSpec {
            name: "inject",
            kind: ArgKind::Bool,
            required: false,
            summary: "send the paste chord too (default yes)",
        }],
    },
    ActionSpec {
        name: "paste.next",
        summary: "consume the active session's next item",
        args: &[ArgSpec {
            name: "peek",
            kind: ArgKind::Bool,
            required: false,
            summary: "paste it without advancing",
        }],
    },
    ActionSpec { name: "paste.latest", summary: "paste the newest clip", args: &[RAW] },
    ActionSpec {
        name: "paste.offset",
        summary: "paste an older clip by position: 1 is the one before latest",
        args: &[
            ArgSpec {
                name: "offset",
                kind: ArgKind::Int,
                required: true,
                summary: "zero-based, from the newest clip",
            },
            RAW,
        ],
    },
    ActionSpec {
        name: "stack.start",
        summary: "begin LIFO traversal of history",
        args: &[DUPLICATES],
    },
    ActionSpec {
        name: "queue.start",
        summary: "snapshot the newest N clips and paste them oldest first",
        args: &[
            ArgSpec {
                name: "last",
                kind: ArgKind::Int,
                required: true,
                summary: "how many of the newest clips to take",
            },
            DUPLICATES,
        ],
    },
    ActionSpec {
        name: "queue.capture",
        summary: "collect everything copied from now on; first paste seals it",
        args: &[DUPLICATES],
    },
    ActionSpec { name: "queue.seal", summary: "stop collecting and make the queue traversable", args: &[] },
    ActionSpec {
        name: "group.capture",
        summary: "collect copies to paste as one value",
        args: &[DELIMITER, DUPLICATES],
    },
    ActionSpec { name: "group.paste", summary: "paste the captured group as one value", args: &[] },
    ActionSpec {
        name: "group.paste_last",
        summary: "paste the newest N clips joined as one value",
        args: &[
            ArgSpec {
                name: "last",
                kind: ArgKind::Int,
                required: true,
                summary: "how many of the newest clips to join",
            },
            DELIMITER,
            RAW,
        ],
    },
    ActionSpec { name: "session.stop", summary: "end the active session", args: &[] },
    ActionSpec { name: "session.reset", summary: "return the cursor to the start", args: &[] },
    ActionSpec { name: "history.pause", summary: "stop recording copies", args: &[] },
    ActionSpec { name: "history.resume", summary: "resume recording copies", args: &[] },
    ActionSpec {
        name: "history.clear",
        summary: "delete recorded history",
        args: &[ArgSpec {
            name: "keep_pinned",
            kind: ArgKind::Bool,
            required: false,
            summary: "spare pinned clips",
        }],
    },
];

pub fn action_spec(name: &str) -> Option<&'static ActionSpec> {
    BINDABLE_ACTIONS.iter().find(|spec| spec.name == name)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{Action, Request};

    /// A value of the right shape for an argument, for the drift check.
    fn sample(arg: &ArgSpec) -> serde_json::Value {
        match arg.kind {
            ArgKind::Enum(values) => serde_json::Value::String(values[0].into()),
            ArgKind::Int => serde_json::Value::from(3),
            ArgKind::Bool => serde_json::Value::Bool(true),
            ArgKind::Text => serde_json::Value::String(", ".into()),
        }
    }

    #[test]
    fn every_spec_describes_a_real_action_and_all_of_its_arguments() {
        // Two directions. With every argument supplied, the request must
        // deserialize — so no spec names an argument the action lacks, and
        // every value is of a type the action accepts. With only the required
        // ones supplied it must deserialize too — so nothing marked optional
        // is secretly required.
        for spec in BINDABLE_ACTIONS {
            let mut all = serde_json::Map::new();
            let mut required = serde_json::Map::new();
            for arg in spec.args {
                all.insert(arg.name.into(), sample(arg));
                if arg.required {
                    required.insert(arg.name.into(), sample(arg));
                }
            }
            for (label, args) in [("all", all), ("required-only", required)] {
                let text = serde_json::json!({
                    "version": 1, "id": "x", "action": spec.name, "args": args,
                })
                .to_string();
                serde_json::from_str::<Request>(&text)
                    .unwrap_or_else(|e| panic!("{} ({label}): {e}", spec.name));
            }
        }
    }

    #[test]
    fn required_arguments_really_are_required() {
        // The inverse: leaving a required argument out must fail, or the form
        // would nag about something the daemon does not care about.
        for spec in BINDABLE_ACTIONS {
            for arg in spec.args.iter().filter(|a| a.required) {
                let mut args = serde_json::Map::new();
                for other in spec.args.iter().filter(|a| a.name != arg.name) {
                    args.insert(other.name.into(), sample(other));
                }
                let text = serde_json::json!({
                    "version": 1, "id": "x", "action": spec.name, "args": args,
                })
                .to_string();
                assert!(
                    serde_json::from_str::<Request>(&text).is_err(),
                    "{}: `{}` is marked required but the action accepts its absence",
                    spec.name,
                    arg.name
                );
            }
        }
    }

    #[test]
    fn enum_values_are_the_ones_the_action_accepts() {
        for spec in BINDABLE_ACTIONS {
            for arg in spec.args {
                if let ArgKind::Enum(values) = arg.kind {
                    for value in values {
                        let text = serde_json::json!({
                            "version": 1, "id": "x", "action": spec.name,
                            "args": { arg.name: value },
                        })
                        .to_string();
                        let parsed: Result<Request, _> = serde_json::from_str(&text);
                        // A required sibling may be missing; only a wrong
                        // enum value is what this checks for.
                        if let Err(e) = &parsed {
                            assert!(
                                !e.to_string().contains("unknown variant"),
                                "{}.{} = {value}: {e}",
                                spec.name,
                                arg.name
                            );
                        }
                    }
                }
            }
        }
        let _ = Action::Status;
    }
}
