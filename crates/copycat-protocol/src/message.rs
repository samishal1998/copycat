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
    /// Whatever the paste chord means in the active mode: a stack pops, a
    /// queue advances (sealing itself first if still capturing), a group
    /// pastes its aggregate (R21). This is the paste the daemon performs when
    /// it intercepts the user's own Ctrl/Cmd+V; `inject: false` is that path,
    /// where the user's keystroke does the pasting.
    #[serde(rename = "paste.mode")]
    PasteMode {
        #[serde(default = "default_true")]
        inject: bool,
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
    /// Add a binding, or replace the one already on that trigger.
    #[serde(rename = "bind.set")]
    BindSet {
        kind: BindingKind,
        /// The chord for a hotkey, or the key sequence for a leader binding.
        trigger: String,
        action: String,
        #[serde(default)]
        args: serde_json::Value,
    },
    #[serde(rename = "bind.remove")]
    BindRemove { kind: BindingKind, trigger: String },
    /// Change the leader chord itself, or turn the leader off.
    ///
    /// Separate from `bind.set` because the leader is not a binding: it has a
    /// trigger but no action, and everything else keys off it.
    #[serde(rename = "bind.leader")]
    BindLeader {
        #[serde(default)]
        trigger: Option<String>,
        #[serde(default)]
        enabled: Option<bool>,
    },
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

fn default_true() -> bool {
    true
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
        /// TUI keymap entries the user has changed from the defaults. The
        /// trigger is the key, the action a [`TuiAction`] name.
        #[serde(default)]
        tui: Vec<Binding>,
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

/// Which of the two binding classes a change applies to (§3.6).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum BindingKind {
    /// One system-wide chord.
    Hotkey,
    /// A key pressed after the leader.
    Leader,
    /// A key inside the TUI. Not a daemon binding at all — the daemon only
    /// stores it — but it lives on the same screen because that is where
    /// anyone looks for "what does this key do".
    Tui,
}

impl BindingKind {
    pub fn as_str(self) -> &'static str {
        match self {
            BindingKind::Hotkey => "hotkey",
            BindingKind::Leader => "leader",
            BindingKind::Tui => "tui",
        }
    }
}

/// Everything a key can do inside the TUI.
///
/// Defined here rather than in the TUI so the daemon can refuse a keymap entry
/// that names an action which does not exist, the same way it refuses a
/// hotkey bound to an unknown daemon action.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TuiAction {
    Quit,
    Help,
    NextTab,
    PrevTab,
    TabHistory,
    TabSession,
    TabBindings,
    TabDiagnostics,
    Down,
    Up,
    Top,
    Bottom,
    Refresh,
    Search,
    ToggleRaw,
    /// What "enter" means on the current screen: paste by id on History,
    /// paste next on Session, edit on Bindings.
    Confirm,
    PasteNext,
    Delete,
    Pin,
    Add,
    Edit,
    Test,
    TogglePause,
    StackStart,
    QueueCapture,
    QueueSeal,
    GroupCapture,
    GroupPaste,
    SessionStop,
    SessionReset,
}

impl TuiAction {
    pub const ALL: [TuiAction; 30] = [
        TuiAction::Quit,
        TuiAction::Help,
        TuiAction::NextTab,
        TuiAction::PrevTab,
        TuiAction::TabHistory,
        TuiAction::TabSession,
        TuiAction::TabBindings,
        TuiAction::TabDiagnostics,
        TuiAction::Down,
        TuiAction::Up,
        TuiAction::Top,
        TuiAction::Bottom,
        TuiAction::Refresh,
        TuiAction::Search,
        TuiAction::ToggleRaw,
        TuiAction::Confirm,
        TuiAction::PasteNext,
        TuiAction::Delete,
        TuiAction::Pin,
        TuiAction::Add,
        TuiAction::Edit,
        TuiAction::Test,
        TuiAction::TogglePause,
        TuiAction::StackStart,
        TuiAction::QueueCapture,
        TuiAction::QueueSeal,
        TuiAction::GroupCapture,
        TuiAction::GroupPaste,
        TuiAction::SessionStop,
        TuiAction::SessionReset,
    ];

    pub fn as_str(self) -> &'static str {
        match self {
            TuiAction::Quit => "quit",
            TuiAction::Help => "help",
            TuiAction::NextTab => "next_tab",
            TuiAction::PrevTab => "prev_tab",
            TuiAction::TabHistory => "tab_history",
            TuiAction::TabSession => "tab_session",
            TuiAction::TabBindings => "tab_bindings",
            TuiAction::TabDiagnostics => "tab_diagnostics",
            TuiAction::Down => "down",
            TuiAction::Up => "up",
            TuiAction::Top => "top",
            TuiAction::Bottom => "bottom",
            TuiAction::Refresh => "refresh",
            TuiAction::Search => "search",
            TuiAction::ToggleRaw => "toggle_raw",
            TuiAction::Confirm => "confirm",
            TuiAction::PasteNext => "paste_next",
            TuiAction::Delete => "delete",
            TuiAction::Pin => "pin",
            TuiAction::Add => "add",
            TuiAction::Edit => "edit",
            TuiAction::Test => "test",
            TuiAction::TogglePause => "toggle_pause",
            TuiAction::StackStart => "stack_start",
            TuiAction::QueueCapture => "queue_capture",
            TuiAction::QueueSeal => "queue_seal",
            TuiAction::GroupCapture => "group_capture",
            TuiAction::GroupPaste => "group_paste",
            TuiAction::SessionStop => "session_stop",
            TuiAction::SessionReset => "session_reset",
        }
    }

    pub fn parse(name: &str) -> Option<TuiAction> {
        TuiAction::ALL.iter().copied().find(|a| a.as_str() == name)
    }

    /// The keys each action has out of the box.
    ///
    /// Several keys per action where two conventions coexist (`j` and the
    /// arrow), and `dd` for delete: it is the only destructive key, and a
    /// two-key confirm is cheaper than a modal.
    pub fn default_keys(self) -> &'static [&'static str] {
        match self {
            TuiAction::Quit => &["q"],
            TuiAction::Help => &["?"],
            TuiAction::NextTab => &["tab"],
            TuiAction::PrevTab => &["backtab"],
            TuiAction::TabHistory => &["1"],
            TuiAction::TabSession => &["2"],
            TuiAction::TabBindings => &["3"],
            TuiAction::TabDiagnostics => &["4"],
            TuiAction::Down => &["j", "down"],
            TuiAction::Up => &["k", "up"],
            TuiAction::Top => &["home"],
            TuiAction::Bottom => &["end"],
            TuiAction::Refresh => &["r"],
            TuiAction::Search => &["/"],
            TuiAction::ToggleRaw => &["a"],
            TuiAction::Confirm => &["enter"],
            TuiAction::PasteNext => &["n"],
            TuiAction::Delete => &["dd"],
            TuiAction::Pin => &["p"],
            TuiAction::Add => &["a"],
            TuiAction::Edit => &["e"],
            TuiAction::Test => &["t"],
            TuiAction::TogglePause => &["space"],
            TuiAction::StackStart => &["s"],
            TuiAction::QueueCapture => &["c"],
            TuiAction::QueueSeal => &["S"],
            TuiAction::GroupCapture => &["g"],
            TuiAction::GroupPaste => &["G"],
            TuiAction::SessionStop => &["x"],
            TuiAction::SessionReset => &["0"],
        }
    }
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
            Action::PasteMode { inject: false },
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
    fn binding_edits_round_trip_with_their_arguments() {
        let action = Action::BindSet {
            kind: BindingKind::Leader,
            trigger: "s".into(),
            action: "stack.start".into(),
            args: serde_json::json!({ "duplicates": "preserve" }),
        };
        let request = Request::new("id", action.clone());
        let text = serde_json::to_string(&request).unwrap();
        assert_eq!(serde_json::from_str::<Request>(&text).unwrap().action, action);
    }

    #[test]
    fn a_binding_edit_defaults_to_empty_arguments() {
        let request: Request = serde_json::from_str(
            r#"{"version":1,"id":"x","action":"bind.set","args":{"kind":"hotkey","trigger":"ctrl+alt+v","action":"paste.next"}}"#,
        )
        .unwrap();
        assert_eq!(
            request.action,
            Action::BindSet {
                kind: BindingKind::Hotkey,
                trigger: "ctrl+alt+v".into(),
                action: "paste.next".into(),
                args: serde_json::Value::Null,
            }
        );
    }

    #[test]
    fn the_leader_can_be_retriggered_or_switched_off_independently() {
        let retrigger: Request = serde_json::from_str(
            r#"{"version":1,"id":"x","action":"bind.leader","args":{"trigger":"ctrl+space"}}"#,
        )
        .unwrap();
        assert_eq!(
            retrigger.action,
            Action::BindLeader { trigger: Some("ctrl+space".into()), enabled: None }
        );

        let off: Request = serde_json::from_str(
            r#"{"version":1,"id":"x","action":"bind.leader","args":{"enabled":false}}"#,
        )
        .unwrap();
        assert_eq!(off.action, Action::BindLeader { trigger: None, enabled: Some(false) });
    }

    #[test]
    fn every_tui_action_round_trips_through_its_name_and_has_a_default_key() {
        for action in TuiAction::ALL {
            assert_eq!(TuiAction::parse(action.as_str()), Some(action));
            assert!(!action.default_keys().is_empty(), "{action:?} has no default");
        }
        assert_eq!(TuiAction::parse("fly"), None);
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
