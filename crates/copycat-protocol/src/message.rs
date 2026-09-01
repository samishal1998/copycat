//! Requests, actions, and results.

use copycat_core::{
    ClipId, ClipSummary, CoreError, DuplicatePolicy, SessionStarted, SessionSummary,
};
use serde::{Deserialize, Serialize};

use crate::report::{DoctorReport, StatusReport};

pub const PROTOCOL_VERSION: u32 = 1;

/// One request from a client.
///
/// `action` and `args` are flattened in, so the wire form matches the PRD
/// example exactly rather than nesting the action inside another object.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct Request {
    pub version: u32,
    pub id: String,
    #[serde(flatten)]
    pub action: Action,
}

impl Request {
    pub fn new(id: impl Into<String>, action: Action) -> Self {
        Request { version: PROTOCOL_VERSION, id: id.into(), action }
    }
}

/// Hand-written so `args` can be omitted.
///
/// Serde's adjacent tagging insists on the content field even when every
/// argument has a default, which would force a binding meaning "just paste the
/// next item" to be written `{"action":"paste.next","args":{}}`. This is a
/// human-writable protocol; requiring an empty object would be a papercut in a
/// config file for no gain on the wire.
impl<'de> Deserialize<'de> for Request {
    fn deserialize<D: serde::Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        use serde::de::Error;

        #[derive(Deserialize)]
        struct Raw {
            version: u32,
            id: String,
            action: String,
            #[serde(default)]
            args: Option<serde_json::Value>,
        }

        let raw = Raw::deserialize(deserializer)?;
        let had_args = raw.args.is_some();

        let mut tagged = serde_json::Map::new();
        tagged.insert("action".into(), serde_json::Value::String(raw.action));
        if let Some(args) = raw.args {
            tagged.insert("args".into(), args);
        }

        let action = match serde_json::from_value::<Action>(tagged.clone().into()) {
            Ok(action) => action,
            // A struct variant whose fields all have defaults still needs the
            // content key present; a unit variant rejects it. Try the other
            // shape before giving up.
            Err(first) if !had_args => {
                tagged.insert("args".into(), serde_json::Value::Object(Default::default()));
                serde_json::from_value(tagged.into()).map_err(|_| D::Error::custom(first))?
            }
            Err(e) => return Err(D::Error::custom(e)),
        };

        Ok(Request { version: raw.version, id: raw.id, action })
    }
}

/// Everything the daemon can be asked to do.
///
/// Modes share `session.*` actions: `stack stop`, `queue stop`, and `group end`
/// all mean "end whatever is active", because a user who typed one of them
/// wants no session, and only one session exists at a time (R4).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "action", content = "args", rename_all = "snake_case")]
pub enum Action {
    #[serde(rename = "paste.latest")]
    PasteLatest {
        #[serde(default)]
        raw: bool,
    },
    #[serde(rename = "paste.offset")]
    PasteOffset {
        offset: usize,
        #[serde(default)]
        raw: bool,
    },
    #[serde(rename = "paste.id")]
    PasteId { id: ClipId },
    #[serde(rename = "paste.next")]
    PasteNext {
        #[serde(default)]
        peek: bool,
    },

    #[serde(rename = "stack.start")]
    StackStart {
        #[serde(default)]
        duplicates: Option<DuplicatePolicy>,
    },

    #[serde(rename = "queue.start")]
    QueueStart {
        last: usize,
        #[serde(default)]
        duplicates: Option<DuplicatePolicy>,
    },
    #[serde(rename = "queue.capture")]
    QueueCapture {
        #[serde(default)]
        duplicates: Option<DuplicatePolicy>,
    },
    #[serde(rename = "queue.seal")]
    QueueSeal,

    #[serde(rename = "group.capture")]
    GroupCapture {
        #[serde(default)]
        delimiter: Option<String>,
        #[serde(default)]
        duplicates: Option<DuplicatePolicy>,
    },
    #[serde(rename = "group.paste")]
    GroupPaste,
    #[serde(rename = "group.paste_last")]
    GroupPasteLast {
        last: usize,
        #[serde(default)]
        delimiter: Option<String>,
        #[serde(default)]
        raw: bool,
    },

    #[serde(rename = "session.status")]
    SessionStatus,
    #[serde(rename = "session.stop")]
    SessionStop,
    #[serde(rename = "session.reset")]
    SessionReset,

    #[serde(rename = "history.list")]
    HistoryList {
        #[serde(default = "default_limit")]
        limit: usize,
        #[serde(default)]
        raw: bool,
    },
    #[serde(rename = "history.show")]
    HistoryShow { id: ClipId },
    #[serde(rename = "history.search")]
    HistorySearch {
        query: String,
        #[serde(default = "default_limit")]
        limit: usize,
    },
    #[serde(rename = "history.delete")]
    HistoryDelete { id: ClipId },
    #[serde(rename = "history.clear")]
    HistoryClear {
        #[serde(default)]
        keep_pinned: bool,
    },
    #[serde(rename = "history.pin")]
    HistoryPin { id: ClipId, pinned: bool },
    #[serde(rename = "history.pause")]
    HistoryPause,
    #[serde(rename = "history.resume")]
    HistoryResume,

    #[serde(rename = "bind.list")]
    BindList,
    #[serde(rename = "bind.reload")]
    BindReload,
    #[serde(rename = "config.show")]
    ConfigShow,

    #[serde(rename = "status")]
    Status,
    #[serde(rename = "doctor")]
    Doctor,
    #[serde(rename = "daemon.stop")]
    DaemonStop,
}

fn default_limit() -> usize {
    100
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Response {
    pub version: u32,
    pub id: String,
    #[serde(flatten)]
    pub outcome: Outcome,
}

impl Response {
    pub fn ok(id: impl Into<String>, result: ResultBody) -> Self {
        Response {
            version: PROTOCOL_VERSION,
            id: id.into(),
            outcome: Outcome::Ok { result },
        }
    }

    pub fn error(id: impl Into<String>, error: CoreError) -> Self {
        Response {
            version: PROTOCOL_VERSION,
            id: id.into(),
            outcome: Outcome::Error { error },
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "status", rename_all = "snake_case")]
pub enum Outcome {
    Ok { result: ResultBody },
    Error { error: CoreError },
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum ResultBody {
    Done,
    Pasted {
        clip_id: Option<ClipId>,
        preview: String,
        bytes: usize,
        /// Group entries with no text, skipped rather than fatal (R14).
        skipped_non_text: usize,
        /// Whether the paste chord reached the focused application. False means
        /// the value is on the clipboard and the user must press paste (§4.5).
        injected: bool,
        session: Option<SessionSummary>,
    },
    SessionStarted(SessionStarted),
    Session {
        session: Option<SessionSummary>,
    },
    Clips {
        clips: Vec<ClipSummary>,
        /// Set when a bounded search stopped early (R18).
        truncated: bool,
    },
    Clip {
        clip: ClipSummary,
        /// The full text, for `history show`. `None` for non-text payloads.
        text: Option<String>,
    },
    Removed {
        count: usize,
    },
    Bindings {
        leader: Option<String>,
        sequences: Vec<Binding>,
        hotkeys: Vec<Binding>,
        /// Bindings the platform could not register, with the reason.
        rejected: Vec<RejectedBinding>,
    },
    Config {
        path: String,
        toml: String,
    },
    Status(Box<StatusReport>),
    Doctor(Box<DoctorReport>),
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Binding {
    pub trigger: String,
    pub action: String,
    pub args: serde_json::Value,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RejectedBinding {
    pub trigger: String,
    pub reason: String,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_request_matches_the_documented_wire_form() {
        // §8's worked example, verbatim.
        let request = Request::new(
            "req-123",
            Action::StackStart { duplicates: Some(DuplicatePolicy::Collapse) },
        );
        let json: serde_json::Value = serde_json::to_value(&request).unwrap();

        assert_eq!(json["version"], 1);
        assert_eq!(json["id"], "req-123");
        assert_eq!(json["action"], "stack.start");
        assert_eq!(json["args"]["duplicates"], "collapse");
    }

    #[test]
    fn requests_round_trip() {
        for action in [
            Action::PasteNext { peek: true },
            Action::PasteOffset { offset: 4, raw: true },
            Action::QueueStart { last: 5, duplicates: None },
            Action::Status,
            Action::HistoryPin { id: ClipId(7), pinned: true },
        ] {
            let request = Request::new("id", action.clone());
            let text = serde_json::to_string(&request).unwrap();
            let back: Request = serde_json::from_str(&text).unwrap();
            assert_eq!(back.action, action, "round trip failed for {text}");
        }
    }

    #[test]
    fn omitted_args_fall_back_to_defaults() {
        // A binding that just says `paste.next` must not have to spell out
        // every flag.
        let request: Request =
            serde_json::from_str(r#"{"version":1,"id":"x","action":"paste.next"}"#).unwrap();
        assert_eq!(request.action, Action::PasteNext { peek: false });

        let listed: Request =
            serde_json::from_str(r#"{"version":1,"id":"x","action":"history.list"}"#).unwrap();
        assert_eq!(listed.action, Action::HistoryList { limit: 100, raw: false });
    }

    #[test]
    fn a_unit_action_needs_no_args_either_way() {
        let bare: Request =
            serde_json::from_str(r#"{"version":1,"id":"x","action":"queue.seal"}"#).unwrap();
        assert_eq!(bare.action, Action::QueueSeal);

        let empty: Request =
            serde_json::from_str(r#"{"version":1,"id":"x","action":"status","args":null}"#).unwrap();
        assert_eq!(empty.action, Action::Status);
    }

    #[test]
    fn a_missing_required_argument_is_still_an_error() {
        // The omitted-args affordance must not turn a typo into a default.
        let parsed: Result<Request, _> =
            serde_json::from_str(r#"{"version":1,"id":"x","action":"queue.start"}"#);
        assert!(parsed.is_err(), "queue.start has no default for `last`");
    }

    #[test]
    fn an_unknown_action_is_a_deserialization_error_not_a_silent_default() {
        let parsed: Result<Request, _> =
            serde_json::from_str(r#"{"version":1,"id":"x","action":"paste.everything"}"#);
        assert!(parsed.is_err());
    }

    #[test]
    fn responses_carry_status_and_error_codes() {
        let response = Response::error(
            "req-9",
            CoreError::not_found("session_exhausted", "nothing left"),
        );
        let json: serde_json::Value = serde_json::to_value(&response).unwrap();

        assert_eq!(json["status"], "error");
        assert_eq!(json["error"]["code"], "session_exhausted");
        assert_eq!(json["error"]["kind"], "not_found");

        let back: Response = serde_json::from_value(json).unwrap();
        assert_eq!(back, response);
    }

    #[test]
    fn a_paste_result_round_trips_with_its_session() {
        let body = ResultBody::Pasted {
            clip_id: Some(ClipId(3)),
            preview: "hello".into(),
            bytes: 5,
            skipped_non_text: 0,
            injected: true,
            session: None,
        };
        let text = serde_json::to_string(&body).unwrap();
        assert_eq!(serde_json::from_str::<ResultBody>(&text).unwrap(), body);
    }
}
