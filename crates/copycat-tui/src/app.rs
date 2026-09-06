//! TUI state and key handling.
//!
//! Deliberately free of any terminal or socket: keys go in, [`AppRequest`]s
//! come out, and the runner performs them. That makes every binding testable
//! without a pty, and keeps the rule from ADR-003 intact — the TUI decides
//! nothing about clipboard semantics, it only asks the daemon.

use copycat_core::{ClipId, ClipSummary, SessionMode, SessionState};
use copycat_protocol::{
    ActionSpec, ArgKind, BINDABLE_ACTIONS, Binding, BindingKind, DoctorReport, RejectedBinding,
    StatusReport, TuiAction, action_spec,
};
use ratatui::crossterm::event::{KeyCode, KeyEvent, KeyModifiers};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Tab {
    History,
    Session,
    Bindings,
    Diagnostics,
}

impl Tab {
    pub const ALL: [Tab; 4] = [Tab::History, Tab::Session, Tab::Bindings, Tab::Diagnostics];

    pub fn title(self) -> &'static str {
        match self {
            Tab::History => "History",
            Tab::Session => "Session",
            Tab::Bindings => "Bindings",
            Tab::Diagnostics => "Diagnostics",
        }
    }

    fn next(self) -> Tab {
        let index = Tab::ALL.iter().position(|t| *t == self).unwrap_or(0);
        Tab::ALL[(index + 1) % Tab::ALL.len()]
    }

    fn previous(self) -> Tab {
        let index = Tab::ALL.iter().position(|t| *t == self).unwrap_or(0);
        Tab::ALL[(index + Tab::ALL.len() - 1) % Tab::ALL.len()]
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum InputMode {
    Normal,
    Search,
    /// Filling in the binding form.
    Editing,
    /// Pressing keys to see which binding they hit.
    Testing,
}

/// The result of one keystroke in test mode.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct KeyProbe {
    /// The chord, spelled the way a config would.
    pub chord: String,
    /// The row it matched, if it matched one.
    pub matched: Option<usize>,
    /// The leader was hit, so the next key is read as a sequence.
    pub armed: bool,
    /// Something the terminal did to this keystroke before we saw it.
    pub note: Option<String>,
    /// Exactly which modifier bits arrived, for when the chord did not match
    /// and the question becomes "what did the terminal actually send".
    pub raw_modifiers: String,
}

/// Something the runner should ask the daemon to do.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum AppRequest {
    Refresh,
    Paste(ClipId),
    /// Consume the active session's next item. The only paste that moves the
    /// cursor: pasting a clip by id is addressing, not traversal (R12).
    PasteNext,
    Delete(ClipId),
    SetPinned(ClipId, bool),
    StackStart,
    QueueCapture,
    QueueSeal,
    GroupCapture,
    GroupPaste,
    SessionStop,
    SessionReset,
    TogglePause,
    ReloadBindings,
    SetBinding {
        kind: BindingKind,
        trigger: String,
        action: String,
        args: serde_json::Value,
    },
    RemoveBinding {
        kind: BindingKind,
        trigger: String,
    },
    SetLeader {
        trigger: Option<String>,
        enabled: Option<bool>,
    },
}

/// What a row on the bindings screen edits.
///
/// The leader is on that list because that is where anyone would look for it,
/// but it is not a binding: it has a trigger and no action.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BindingTarget {
    Leader,
    Binding(BindingKind),
}

impl BindingTarget {
    pub fn label(self) -> &'static str {
        match self {
            BindingTarget::Leader => "leader",
            BindingTarget::Binding(kind) => kind.as_str(),
        }
    }
}

/// One editable row on the bindings screen.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BindingRow {
    pub target: BindingTarget,
    pub trigger: String,
    pub action: String,
    pub args: serde_json::Value,
    /// Why this binding is not currently firing, if it is not.
    pub inactive: Option<String>,
    /// A TUI key the user changed from its default.
    pub custom: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DraftField {
    Kind,
    /// Leader only: whether it is armed at all.
    Enabled,
    Trigger,
    Action,
    /// One argument of the chosen action, by position in its spec.
    Arg(usize),
}

impl DraftField {
    pub fn label(self) -> &'static str {
        match self {
            DraftField::Kind => "kind",
            DraftField::Enabled => "enabled",
            DraftField::Trigger => "trigger",
            DraftField::Action => "action",
            DraftField::Arg(_) => "",
        }
    }
}

/// The value of one argument, in whatever shape its spec says.
///
/// "Unset" is a real state for every kind: an optional argument left alone
/// is not sent at all, so the daemon's default applies rather than a value
/// the form guessed.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ArgValue {
    /// Index into the enum's values.
    Choice(Option<usize>),
    Flag(Option<bool>),
    /// Ints and text are both typed; an int is validated on submit.
    Text(String),
}

impl ArgValue {
    fn blank(kind: ArgKind) -> Self {
        match kind {
            ArgKind::Enum(_) => ArgValue::Choice(None),
            ArgKind::Bool => ArgValue::Flag(None),
            ArgKind::Int | ArgKind::Text => ArgValue::Text(String::new()),
        }
    }

    /// Read an existing binding's argument back into the form.
    fn from_json(kind: ArgKind, value: Option<&serde_json::Value>) -> Self {
        match (kind, value) {
            (ArgKind::Enum(values), Some(serde_json::Value::String(s))) => {
                ArgValue::Choice(values.iter().position(|v| v == s))
            }
            (ArgKind::Bool, Some(serde_json::Value::Bool(b))) => ArgValue::Flag(Some(*b)),
            (ArgKind::Int, Some(serde_json::Value::Number(n))) => ArgValue::Text(n.to_string()),
            (ArgKind::Text, Some(serde_json::Value::String(s))) => ArgValue::Text(s.clone()),
            _ => ArgValue::blank(kind),
        }
    }

    fn is_unset(&self) -> bool {
        match self {
            ArgValue::Choice(c) => c.is_none(),
            ArgValue::Flag(f) => f.is_none(),
            ArgValue::Text(t) => t.trim().is_empty(),
        }
    }
}

/// The binding being written, before it is sent.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BindingDraft {
    pub target: BindingTarget,
    pub kind: BindingKind,
    pub enabled: bool,
    pub trigger: String,
    /// Index into [`BindingDraft::actions`].
    pub action_index: usize,
    /// One per argument of the chosen action's spec. Empty for TUI actions.
    pub args: Vec<ArgValue>,
    pub field: DraftField,
    /// The binding this replaces, when editing rather than adding. Kept so a
    /// renamed trigger removes the old one instead of leaving both.
    pub replacing: Option<(BindingKind, String)>,
    pub error: Option<String>,
    /// The next key press becomes the trigger.
    pub capturing: bool,
}

impl BindingDraft {
    fn blank() -> Self {
        let mut draft = BindingDraft {
            target: BindingTarget::Binding(BindingKind::Leader),
            kind: BindingKind::Leader,
            enabled: true,
            trigger: String::new(),
            action_index: 0,
            args: Vec::new(),
            field: DraftField::Trigger,
            replacing: None,
            error: None,
            capturing: false,
        };
        draft.select_action(0);
        draft
    }

    fn editing(row: &BindingRow) -> Self {
        let kind = match row.target {
            BindingTarget::Binding(kind) => kind,
            BindingTarget::Leader => BindingKind::Leader,
        };
        let mut draft = BindingDraft {
            target: row.target,
            kind,
            enabled: row.inactive.is_none(),
            trigger: row.trigger.clone(),
            action_index: 0,
            args: Vec::new(),
            field: DraftField::Trigger,
            replacing: match row.target {
                // A TUI entry is identified by its action, everything else by
                // its trigger.
                BindingTarget::Binding(BindingKind::Tui) => {
                    Some((BindingKind::Tui, row.action.clone()))
                }
                BindingTarget::Binding(kind) => Some((kind, row.trigger.clone())),
                BindingTarget::Leader => None,
            },
            error: None,
            capturing: false,
        };
        match draft.actions().iter().position(|name| *name == row.action) {
            Some(index) => {
                draft.select_action(index);
                if let Some(spec) = draft.spec() {
                    draft.args = spec
                        .args
                        .iter()
                        .map(|arg| ArgValue::from_json(arg.kind, row.args.get(arg.name)))
                        .collect();
                }
            }
            None if row.target != BindingTarget::Leader => {
                // Bound by hand to something the form cannot offer. Show the
                // form anyway, but say why the action shown is not the one
                // on disk.
                draft.select_action(0);
                draft.error = Some(format!(
                    "`{}` cannot be bound from here; pick an action or press esc",
                    row.action
                ));
            }
            None => {}
        }
        draft
    }

    /// The actions this kind of binding can name, in picker order.
    pub fn actions(&self) -> Vec<&'static str> {
        match self.kind {
            BindingKind::Tui => TuiAction::ALL.iter().map(|a| a.as_str()).collect(),
            _ => BINDABLE_ACTIONS.iter().map(|a| a.name).collect(),
        }
    }

    pub fn action_name(&self) -> &'static str {
        let actions = self.actions();
        actions.get(self.action_index).copied().unwrap_or(actions[0])
    }

    /// The chosen daemon action's spec. `None` for a TUI action, which takes
    /// no arguments.
    pub fn spec(&self) -> Option<&'static ActionSpec> {
        match self.kind {
            BindingKind::Tui => None,
            _ => action_spec(self.action_name()),
        }
    }

    pub fn action_summary(&self) -> &'static str {
        self.spec().map(|s| s.summary).unwrap_or("")
    }

    /// Choose an action and reset its arguments to unset.
    fn select_action(&mut self, index: usize) {
        let count = self.actions().len();
        self.action_index = index % count.max(1);
        self.args = self
            .spec()
            .map(|spec| spec.args.iter().map(|a| ArgValue::blank(a.kind)).collect())
            .unwrap_or_default();
    }

    /// Only the fields this target actually has. The leader has no action, a
    /// TUI action no arguments, and an action's arguments are exactly its
    /// spec's.
    pub fn fields(&self) -> Vec<DraftField> {
        match self.target {
            BindingTarget::Leader => vec![DraftField::Enabled, DraftField::Trigger],
            BindingTarget::Binding(BindingKind::Tui) => {
                vec![DraftField::Kind, DraftField::Trigger, DraftField::Action]
            }
            BindingTarget::Binding(_) => {
                let mut fields = vec![DraftField::Kind, DraftField::Trigger, DraftField::Action];
                fields.extend((0..self.args.len()).map(DraftField::Arg));
                fields
            }
        }
    }

    fn step(&mut self, forward: bool) {
        let fields = self.fields();
        let index = fields.iter().position(|f| *f == self.field).unwrap_or(0);
        let len = fields.len();
        self.field = fields[if forward { (index + 1) % len } else { (index + len - 1) % len }];
    }

    /// Whether the focused field is chosen from options rather than typed.
    pub fn is_selector(&self, field: DraftField) -> bool {
        match field {
            DraftField::Kind | DraftField::Enabled | DraftField::Action => true,
            DraftField::Trigger => false,
            DraftField::Arg(i) => !matches!(self.args.get(i), Some(ArgValue::Text(_))),
        }
    }

    /// Move a selector field one step. `forward` is right/space, else left.
    fn cycle(&mut self, forward: bool) {
        match self.field {
            DraftField::Kind => {
                self.kind = match (self.kind, forward) {
                    (BindingKind::Leader, true) | (BindingKind::Tui, false) => BindingKind::Hotkey,
                    (BindingKind::Hotkey, true) | (BindingKind::Leader, false) => BindingKind::Tui,
                    (BindingKind::Tui, true) | (BindingKind::Hotkey, false) => BindingKind::Leader,
                };
                self.target = BindingTarget::Binding(self.kind);
                // A different kind offers a different action list.
                self.select_action(0);
            }
            DraftField::Enabled => self.enabled = !self.enabled,
            DraftField::Action => {
                let count = self.actions().len();
                let next = if forward {
                    (self.action_index + 1) % count
                } else {
                    (self.action_index + count - 1) % count
                };
                self.select_action(next);
            }
            DraftField::Arg(i) => {
                let Some(spec) = self.spec() else { return };
                let Some(arg) = spec.args.get(i) else { return };
                let Some(value) = self.args.get_mut(i) else { return };
                match (arg.kind, value) {
                    // Options run: unset, then each value, and round again.
                    (ArgKind::Enum(values), ArgValue::Choice(choice)) => {
                        let n = values.len() + 1;
                        let current = choice.map(|c| c + 1).unwrap_or(0);
                        let next = if forward { (current + 1) % n } else { (current + n - 1) % n };
                        *choice = if next == 0 { None } else { Some(next - 1) };
                    }
                    (ArgKind::Bool, ArgValue::Flag(flag)) => {
                        *flag = match (*flag, forward) {
                            (None, true) | (Some(false), false) => Some(true),
                            (Some(true), true) | (None, false) => Some(false),
                            (Some(false), true) | (Some(true), false) => None,
                        };
                    }
                    _ => {}
                }
            }
            DraftField::Trigger => {}
        }
    }

    fn text_mut(&mut self) -> Option<&mut String> {
        match self.field {
            DraftField::Trigger => Some(&mut self.trigger),
            DraftField::Arg(i) => match self.args.get_mut(i) {
                Some(ArgValue::Text(text)) => Some(text),
                _ => None,
            },
            _ => None,
        }
    }

    /// What a field shows. Selectors render their options; the chosen one is
    /// bracketed.
    pub fn value(&self, field: DraftField) -> String {
        let bracket = |options: &[&str], chosen: Option<usize>| -> String {
            options
                .iter()
                .enumerate()
                .map(|(i, o)| if Some(i) == chosen { format!("[{o}]") } else { o.to_string() })
                .collect::<Vec<_>>()
                .join("  ")
        };
        match field {
            DraftField::Kind => bracket(
                &["leader", "hotkey", "tui"],
                Some(match self.kind {
                    BindingKind::Leader => 0,
                    BindingKind::Hotkey => 1,
                    BindingKind::Tui => 2,
                }),
            ),
            DraftField::Enabled => bracket(&["yes", "no"], Some(if self.enabled { 0 } else { 1 })),
            DraftField::Trigger => self.trigger.clone(),
            DraftField::Action => self.action_name().to_string(),
            DraftField::Arg(i) => match (self.spec().and_then(|s| s.args.get(i)), self.args.get(i)) {
                (Some(arg), Some(ArgValue::Choice(choice))) => {
                    let ArgKind::Enum(values) = arg.kind else { return String::new() };
                    let mut options = vec!["(unset)"];
                    options.extend_from_slice(values);
                    bracket(&options, Some(choice.map(|c| c + 1).unwrap_or(0)))
                }
                (Some(_), Some(ArgValue::Flag(flag))) => bracket(
                    &["(unset)", "yes", "no"],
                    Some(match flag {
                        None => 0,
                        Some(true) => 1,
                        Some(false) => 2,
                    }),
                ),
                (Some(_), Some(ArgValue::Text(text))) => text.clone(),
                _ => String::new(),
            },
        }
    }

    /// The label for a field, which for an argument is its name.
    pub fn label(&self, field: DraftField) -> String {
        match field {
            DraftField::Arg(i) => self
                .spec()
                .and_then(|s| s.args.get(i))
                .map(|a| a.name.to_string())
                .unwrap_or_default(),
            other => other.label().to_string(),
        }
    }

    /// The one-line explanation shown under the focused field.
    pub fn help(&self, field: DraftField) -> String {
        match field {
            DraftField::Kind => "leader: a key after the leader · hotkey: a system-wide chord · tui: a key in this screen".into(),
            DraftField::Enabled => "space toggles".into(),
            DraftField::Trigger => match self.kind {
                BindingKind::Hotkey => "a chord like ctrl+alt+v — or ctrl+r, then press it".into(),
                BindingKind::Leader if self.target == BindingTarget::Leader => {
                    "the leader chord, like ctrl+alt+space — or ctrl+r, then press it".into()
                }
                BindingKind::Leader => "the key pressed after the leader — or ctrl+r, then press it".into(),
                BindingKind::Tui => "a key, a sequence like dd, or a chord — or ctrl+r, then press it".into(),
            },
            DraftField::Action => format!("← → to choose · {}", self.action_summary()),
            DraftField::Arg(i) => match self.spec().and_then(|s| s.args.get(i)) {
                Some(arg) => format!(
                    "{}{}{}",
                    match arg.kind {
                        ArgKind::Enum(_) | ArgKind::Bool => "← → to choose · ",
                        ArgKind::Int => "a whole number · ",
                        ArgKind::Text => "",
                    },
                    if arg.required { "required · " } else { "" },
                    arg.summary
                ),
                None => String::new(),
            },
        }
    }

    /// Turn the form into requests, or say what is wrong with it.
    fn submit(&self) -> Result<Vec<AppRequest>, String> {
        if self.trigger.trim().is_empty() {
            return Err("a trigger is required".into());
        }
        if self.target == BindingTarget::Leader {
            return Ok(vec![AppRequest::SetLeader {
                trigger: Some(self.trigger.trim().to_string()),
                enabled: Some(self.enabled),
            }]);
        }

        let mut args = serde_json::Map::new();
        if let Some(spec) = self.spec() {
            for (arg, value) in spec.args.iter().zip(&self.args) {
                if value.is_unset() {
                    if arg.required {
                        return Err(format!("{} is required", arg.name));
                    }
                    continue;
                }
                let json = match (arg.kind, value) {
                    (ArgKind::Enum(values), ArgValue::Choice(Some(i))) => {
                        serde_json::Value::String(values[*i].into())
                    }
                    (ArgKind::Bool, ArgValue::Flag(Some(b))) => serde_json::Value::Bool(*b),
                    (ArgKind::Int, ArgValue::Text(text)) => match text.trim().parse::<u64>() {
                        Ok(n) => serde_json::Value::from(n),
                        Err(_) => return Err(format!("{} must be a whole number", arg.name)),
                    },
                    (ArgKind::Text, ArgValue::Text(text)) => serde_json::Value::String(text.clone()),
                    _ => continue,
                };
                args.insert(arg.name.into(), json);
            }
        }
        let args = if args.is_empty() { serde_json::Value::Null } else { serde_json::Value::Object(args) };

        let mut requests = Vec::new();
        // Renaming has to delete the old entry, or an edit would quietly
        // leave two bindings where there was one. A TUI entry is identified
        // by its action, everything else by its trigger.
        if let Some((kind, identity)) = &self.replacing {
            let now = if self.kind == BindingKind::Tui { self.action_name() } else { self.trigger.trim() };
            if *kind != self.kind || identity != now {
                requests.push(AppRequest::RemoveBinding {
                    kind: *kind,
                    trigger: identity.clone(),
                });
            }
        }
        requests.push(AppRequest::SetBinding {
            kind: self.kind,
            trigger: self.trigger.trim().to_string(),
            action: self.action_name().to_string(),
            args,
        });
        Ok(requests)
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Message {
    pub text: String,
    pub is_error: bool,
}

#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub struct BindingsView {
    pub leader: Option<String>,
    pub sequences: Vec<Binding>,
    pub hotkeys: Vec<Binding>,
    /// Keymap entries the user changed. Anything not listed is at its default.
    pub tui: Vec<Binding>,
    pub rejected: Vec<RejectedBinding>,
}

/// The TUI's own keys, resolved: defaults with the user's overrides applied.
///
/// Every binding is a *sequence* of key tokens, which is what makes `dd` and
/// a single `n` the same kind of thing. A key spec like `dd` is one token per
/// character; a special name like `enter` or a chord like `ctrl+c` is one
/// token on its own.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Keymap {
    bindings: Vec<(Vec<String>, TuiAction)>,
}

impl Keymap {
    pub fn resolve(overrides: &[Binding]) -> Self {
        let mut bindings = Vec::new();
        for action in TuiAction::ALL {
            let custom: Vec<&str> = overrides
                .iter()
                .filter(|b| b.action == action.as_str())
                .map(|b| b.trigger.as_str())
                .collect();
            // Any override for an action replaces all of that action's
            // defaults, so unbinding a default is possible.
            let specs: Vec<&str> = if custom.is_empty() {
                action.default_keys().to_vec()
            } else {
                custom
            };
            for spec in specs {
                for one in spec.split_whitespace() {
                    bindings.push((parse_key_spec(one), action));
                }
            }
        }
        Keymap { bindings }
    }

    /// Every action bound to exactly this sequence.
    fn exact(&self, sequence: &[String]) -> Vec<TuiAction> {
        self.bindings
            .iter()
            .filter(|(keys, _)| keys.as_slice() == sequence)
            .map(|(_, action)| *action)
            .collect()
    }

    /// Whether some longer sequence begins with this one.
    fn is_prefix(&self, sequence: &[String]) -> bool {
        self.bindings
            .iter()
            .any(|(keys, _)| keys.len() > sequence.len() && keys.starts_with(sequence))
    }

    /// The keys for an action, as a person would write them.
    pub fn keys_for(&self, action: TuiAction) -> Vec<String> {
        self.bindings
            .iter()
            .filter(|(_, a)| *a == action)
            .map(|(keys, _)| keys.join(""))
            .collect()
    }

    /// The first key for an action, for hints.
    pub fn key_for(&self, action: TuiAction) -> String {
        self.keys_for(action).into_iter().next().unwrap_or_default()
    }
}

impl Default for Keymap {
    fn default() -> Self {
        Keymap::resolve(&[])
    }
}

/// Turn a key spec into its token sequence.
fn parse_key_spec(spec: &str) -> Vec<String> {
    let lower = spec.to_ascii_lowercase();
    let special = matches!(
        lower.as_str(),
        "enter" | "tab" | "backtab" | "space" | "esc" | "escape" | "backspace" | "delete"
            | "up" | "down" | "left" | "right" | "home" | "end" | "pageup" | "pagedown"
    ) || lower.starts_with('f') && lower[1..].parse::<u8>().is_ok();
    if special || spec.contains('+') {
        vec![lower]
    } else {
        spec.chars().map(|c| c.to_string()).collect()
    }
}

/// One key event as a keymap token. Letters keep their case, because `s` and
/// `S` are different bindings; modifiers other than shift become a chord.
fn key_token(key: KeyEvent) -> String {
    let base = match key.code {
        KeyCode::Char(' ') => "space".to_string(),
        KeyCode::Char(c) => c.to_string(),
        KeyCode::Enter => "enter".into(),
        KeyCode::Tab => "tab".into(),
        KeyCode::BackTab => "backtab".into(),
        KeyCode::Esc => "esc".into(),
        KeyCode::Backspace => "backspace".into(),
        KeyCode::Delete => "delete".into(),
        KeyCode::Up => "up".into(),
        KeyCode::Down => "down".into(),
        KeyCode::Left => "left".into(),
        KeyCode::Right => "right".into(),
        KeyCode::Home => "home".into(),
        KeyCode::End => "end".into(),
        KeyCode::PageUp => "pageup".into(),
        KeyCode::PageDown => "pagedown".into(),
        KeyCode::F(n) => format!("f{n}"),
        other => format!("{other:?}").to_lowercase(),
    };
    let mut parts = Vec::new();
    if key.modifiers.contains(KeyModifiers::CONTROL) {
        parts.push("ctrl".to_string());
    }
    if key.modifiers.contains(KeyModifiers::ALT) {
        parts.push("alt".to_string());
    }
    if key.modifiers.intersects(KeyModifiers::SUPER | KeyModifiers::META | KeyModifiers::HYPER) {
        parts.push("super".to_string());
    }
    parts.push(base);
    parts.join("+")
}

pub struct App {
    pub tab: Tab,
    pub clips: Vec<ClipSummary>,
    pub selected: usize,
    pub status: Option<StatusReport>,
    pub doctor: Option<DoctorReport>,
    pub bindings: BindingsView,
    pub search: String,
    pub input_mode: InputMode,
    pub message: Option<Message>,
    pub raw: bool,
    pub show_help: bool,
    pub should_quit: bool,
    /// Editable rows on the bindings screen.
    pub binding_rows: Vec<BindingRow>,
    pub binding_selected: usize,
    pub draft: Option<BindingDraft>,
    /// What the last key in test mode resolved to.
    pub probe: Option<KeyProbe>,
    /// Whether the terminal speaks the Kitty keyboard protocol.
    ///
    /// Without it a terminal reports only shift, ctrl and alt — Command and
    /// Super never arrive at all, whatever the user presses.
    pub keyboard_enhanced: bool,
    /// Keys typed that have not resolved into a command yet, shown the way vim
    /// shows a partial command. Empty means the last keystroke completed.
    pub pending: String,
    pub pending_tokens: Vec<String>,
    pub keymap: Keymap,
}

impl Default for App {
    fn default() -> Self {
        App {
            tab: Tab::History,
            clips: Vec::new(),
            selected: 0,
            status: None,
            doctor: None,
            bindings: BindingsView::default(),
            search: String::new(),
            input_mode: InputMode::Normal,
            message: None,
            raw: false,
            show_help: false,
            should_quit: false,
            binding_rows: Vec::new(),
            binding_selected: 0,
            draft: None,
            probe: None,
            keyboard_enhanced: false,
            pending: String::new(),
            pending_tokens: Vec::new(),
            keymap: Keymap::default(),
        }
    }
}

impl App {
    pub fn selected_clip(&self) -> Option<&ClipSummary> {
        self.clips.get(self.selected)
    }

    pub fn selected_binding(&self) -> Option<&BindingRow> {
        self.binding_rows.get(self.binding_selected)
    }

    /// What the next keystroke will act on, for the corner of the screen.
    ///
    /// The session is the mode in the product's sense — it changes what `paste`
    /// means — so it is what belongs here. A text-entry mode displaces it,
    /// because while one is open the keys go into a field instead.
    pub fn mode_label(&self) -> String {
        match self.input_mode {
            InputMode::Search => return "SEARCH".to_string(),
            InputMode::Editing => return "EDIT".to_string(),
            InputMode::Testing => return "TEST".to_string(),
            InputMode::Normal => {}
        }

        let Some(session) = self.status.as_ref().and_then(|s| s.core.session.as_ref()) else {
            return "NORMAL".to_string();
        };
        let name = session.mode.as_str().to_uppercase();
        match session.state {
            // A capture is collecting, so its size is the interesting number.
            SessionState::Capturing => format!("{name} CAPTURE {}", session.size),
            SessionState::Ready => match session.mode {
                SessionMode::Group => format!("{name} {}", session.size),
                _ => format!("{name} {}/{}", session.cursor.min(session.size), session.size),
            },
        }
    }

    /// Rebuild the editable binding list from what the daemon reported.
    pub fn set_bindings(&mut self, view: BindingsView) {
        let inactive_for = |trigger: &str| -> Option<String> {
            view.rejected
                .iter()
                // A rejected leader binding is reported as "<leader> <key>",
                // so match the key at the end as well as the whole trigger.
                .find(|r| r.trigger == trigger || r.trigger.ends_with(&format!(" {trigger}")))
                .map(|r| r.reason.clone())
        };

        let mut rows: Vec<BindingRow> = Vec::new();

        // The leader leads the list. It is the thing every sequence below it
        // depends on, and it is the first thing someone comes here to change.
        rows.push(BindingRow {
            target: BindingTarget::Leader,
            trigger: view.leader.clone().unwrap_or_default(),
            action: String::new(),
            args: serde_json::Value::Null,
            inactive: view.leader.is_none().then(|| "disabled".to_string()),
            custom: false,
        });

        for (kind, list) in
            [(BindingKind::Leader, &view.sequences), (BindingKind::Hotkey, &view.hotkeys)]
        {
            for binding in list.iter() {
                rows.push(BindingRow {
                    target: BindingTarget::Binding(kind),
                    trigger: binding.trigger.clone(),
                    action: binding.action.clone(),
                    args: binding.args.clone(),
                    inactive: inactive_for(&binding.trigger),
                        custom: false,
                });
            }
        }

        // Every TUI action, not only the changed ones: "what does this key
        // do" needs the whole table, and a default is one keystroke from
        // being a custom one.
        let keymap = Keymap::resolve(&view.tui);
        for action in TuiAction::ALL {
            rows.push(BindingRow {
                target: BindingTarget::Binding(BindingKind::Tui),
                trigger: keymap.keys_for(action).join(" "),
                action: action.as_str().to_string(),
                args: serde_json::Value::Null,
                inactive: None,
                custom: view.tui.iter().any(|b| b.action == action.as_str()),
            });
        }
        self.keymap = keymap;

        self.binding_rows = rows;
        self.binding_selected = self.binding_selected.min(self.binding_rows.len().saturating_sub(1));
        self.bindings = view;
    }

    pub fn set_clips(&mut self, clips: Vec<ClipSummary>) {
        self.clips = clips;
        // Keep the cursor inside the list when it shrinks under us — a delete
        // or a clear from another client should not leave it pointing past the
        // end.
        self.selected = self.selected.min(self.clips.len().saturating_sub(1));
    }

    pub fn note(&mut self, text: impl Into<String>) {
        self.message = Some(Message { text: text.into(), is_error: false });
    }

    pub fn error(&mut self, text: impl Into<String>) {
        self.message = Some(Message { text: text.into(), is_error: true });
    }

    /// Translate a key press into zero or more daemon requests.
    pub fn on_key(&mut self, key: KeyEvent) -> Vec<AppRequest> {
        match self.input_mode {
            InputMode::Search => return self.search_key(key),
            InputMode::Editing => return self.edit_key(key),
            InputMode::Testing => return self.test_key(key),
            InputMode::Normal => {}
        }
        if self.show_help {
            // Any key dismisses help, and only dismisses it: a keystroke aimed
            // at the help screen should not also delete something.
            self.show_help = false;
            self.clear_pending();
            return Vec::new();
        }

        // Two keys live outside the keymap so no keymap can lock someone in:
        // ctrl+c always quits, and escape always abandons whatever is
        // half-typed — and quits when nothing is.
        if key.code == KeyCode::Char('c') && key.modifiers.contains(KeyModifiers::CONTROL) {
            self.should_quit = true;
            return Vec::new();
        }
        if key.code == KeyCode::Esc {
            if self.pending_tokens.is_empty() {
                self.should_quit = true;
            } else {
                self.clear_pending();
            }
            return Vec::new();
        }

        self.message = None;

        let mut sequence = std::mem::take(&mut self.pending_tokens);
        sequence.push(key_token(key));

        let actions = self.keymap.exact(&sequence);
        if actions.is_empty() {
            // Not a command yet, or not one at all. A prefix keeps waiting and
            // stays visible; anything else abandons the sequence without
            // acting, the way vim does, so a mistyped `dx` does nothing.
            if self.keymap.is_prefix(&sequence) {
                self.pending = sequence.join("");
                self.pending_tokens = sequence;
            } else {
                self.pending.clear();
            }
            return Vec::new();
        }
        self.pending.clear();

        // Several actions may share a key across screens — `a` is toggle-raw
        // on History and add on Bindings. The first that applies here wins.
        for action in actions {
            if let Some(requests) = self.perform(action) {
                return requests;
            }
        }
        Vec::new()
    }

    fn clear_pending(&mut self) {
        self.pending.clear();
        self.pending_tokens.clear();
    }

    /// Carry out an action, or `None` if it does not apply on this screen.
    fn perform(&mut self, action: TuiAction) -> Option<Vec<AppRequest>> {
        use TuiAction::*;
        let done = Some(Vec::new());
        match action {
            Quit => {
                self.should_quit = true;
                done
            }
            Help => {
                self.show_help = true;
                done
            }
            NextTab => {
                self.tab = self.tab.next();
                done
            }
            PrevTab => {
                self.tab = self.tab.previous();
                done
            }
            TabHistory => {
                self.tab = Tab::History;
                done
            }
            TabSession => {
                self.tab = Tab::Session;
                done
            }
            TabBindings => {
                self.tab = Tab::Bindings;
                done
            }
            TabDiagnostics => {
                self.tab = Tab::Diagnostics;
                done
            }
            Down => {
                self.move_selection(1);
                done
            }
            Up => {
                self.move_selection(-1);
                done
            }
            Top => {
                self.select(0);
                done
            }
            Bottom => {
                self.select(self.list_len().saturating_sub(1));
                done
            }
            Refresh => Some(vec![AppRequest::Refresh]),
            Search if self.tab == Tab::History => {
                self.input_mode = InputMode::Search;
                self.search.clear();
                done
            }
            ToggleRaw if self.tab == Tab::History => {
                self.raw = !self.raw;
                Some(vec![AppRequest::Refresh])
            }
            Confirm => Some(match self.tab {
                Tab::History => self
                    .selected_clip()
                    .map(|clip| vec![AppRequest::Paste(clip.id)])
                    .unwrap_or_default(),
                // On the session screen, enter means "paste the next one":
                // that is the whole reason the screen exists.
                Tab::Session => vec![AppRequest::PasteNext],
                Tab::Bindings => {
                    self.open_selected_binding();
                    Vec::new()
                }
                Tab::Diagnostics => Vec::new(),
            }),
            PasteNext => Some(vec![AppRequest::PasteNext]),
            Delete if matches!(self.tab, Tab::History | Tab::Bindings) => {
                Some(self.delete_selected())
            }
            Pin if self.tab == Tab::History => Some(
                self.selected_clip()
                    .map(|clip| vec![AppRequest::SetPinned(clip.id, !clip.pinned)])
                    .unwrap_or_default(),
            ),
            Add if self.tab == Tab::Bindings => {
                self.draft = Some(BindingDraft::blank());
                self.input_mode = InputMode::Editing;
                done
            }
            Edit if self.tab == Tab::Bindings => {
                self.open_selected_binding();
                done
            }
            Test if self.tab == Tab::Bindings => {
                self.input_mode = InputMode::Testing;
                self.probe = None;
                done
            }
            TogglePause => Some(vec![AppRequest::TogglePause]),
            // Session controls apply from any screen: they are the fastest
            // path to starting a mode, and hunting for the right tab first
            // would defeat that.
            StackStart => Some(vec![AppRequest::StackStart]),
            QueueCapture => Some(vec![AppRequest::QueueCapture]),
            QueueSeal => Some(vec![AppRequest::QueueSeal]),
            GroupCapture => Some(vec![AppRequest::GroupCapture]),
            GroupPaste => Some(vec![AppRequest::GroupPaste]),
            SessionStop => Some(vec![AppRequest::SessionStop]),
            SessionReset => Some(vec![AppRequest::SessionReset]),
            // Bound, but not on this screen.
            Search | ToggleRaw | Delete | Pin | Add | Edit | Test => None,
        }
    }

    fn open_selected_binding(&mut self) {
        if let Some(row) = self.selected_binding() {
            self.draft = Some(BindingDraft::editing(row));
            self.input_mode = InputMode::Editing;
        }
    }

    fn delete_selected(&mut self) -> Vec<AppRequest> {
        match self.tab {
            Tab::History => self
                .selected_clip()
                .map(|clip| vec![AppRequest::Delete(clip.id)])
                .unwrap_or_default(),
            Tab::Bindings => {
                let Some(row) = self.selected_binding() else { return Vec::new() };
                let (target, trigger, action, custom) =
                    (row.target, row.trigger.clone(), row.action.clone(), row.custom);
                match target {
                    // There is no such thing as no leader, only a disarmed one.
                    BindingTarget::Leader => {
                        self.note("the leader cannot be deleted — press e and set enabled to no");
                        Vec::new()
                    }
                    // A TUI key is identified by its action; removing the
                    // entry puts the default back rather than unbinding it.
                    BindingTarget::Binding(BindingKind::Tui) => {
                        if !custom {
                            self.note(format!("{action} is already at its default"));
                            return Vec::new();
                        }
                        vec![AppRequest::RemoveBinding { kind: BindingKind::Tui, trigger: action }]
                    }
                    BindingTarget::Binding(kind) => {
                        vec![AppRequest::RemoveBinding { kind, trigger }]
                    }
                }
            }
            _ => Vec::new(),
        }
    }

    fn edit_key(&mut self, key: KeyEvent) -> Vec<AppRequest> {
        let Some(draft) = self.draft.as_mut() else {
            self.input_mode = InputMode::Normal;
            return Vec::new();
        };

        // Recording a trigger: the next key press is it, whatever it is.
        if draft.capturing {
            draft.capturing = false;
            if key.code != KeyCode::Esc {
                draft.trigger = match draft.kind {
                    // A chord for a hotkey; the bare key for a sequence or a
                    // TUI key, with its case, since `s` and `S` differ.
                    BindingKind::Hotkey => chord_of(key),
                    _ if draft.target == BindingTarget::Leader => chord_of(key),
                    _ => key_token(key),
                };
                draft.error = None;
            }
            return Vec::new();
        }

        let selector = draft.is_selector(draft.field);
        match key.code {
            KeyCode::Esc => {
                self.draft = None;
                self.input_mode = InputMode::Normal;
            }
            KeyCode::Tab | KeyCode::Down => draft.step(true),
            KeyCode::BackTab | KeyCode::Up => draft.step(false),
            KeyCode::Char('r') if key.modifiers.contains(KeyModifiers::CONTROL)
                && draft.field == DraftField::Trigger =>
            {
                draft.capturing = true;
            }
            KeyCode::Right | KeyCode::Char(' ') if selector => draft.cycle(true),
            KeyCode::Left if selector => draft.cycle(false),
            KeyCode::Enter => {
                return match draft.submit() {
                    Ok(requests) => {
                        self.draft = None;
                        self.input_mode = InputMode::Normal;
                        requests
                    }
                    // Keep the form open on a bad value: retyping it from
                    // scratch because of one typo would be its own papercut.
                    Err(reason) => {
                        draft.error = Some(reason);
                        Vec::new()
                    }
                };
            }
            KeyCode::Backspace => {
                draft.error = None;
                if let Some(text) = draft.text_mut() {
                    text.pop();
                }
            }
            KeyCode::Char(c) => {
                draft.error = None;
                if let Some(text) = draft.text_mut() {
                    text.push(c);
                }
            }
            _ => {}
        }
        Vec::new()
    }

    /// Match a keystroke against the configured bindings and show which one it
    /// hit — the question "did I bind that correctly" answered directly rather
    /// than by reading the table and comparing by eye.
    fn test_key(&mut self, key: KeyEvent) -> Vec<AppRequest> {
        if key.code == KeyCode::Esc {
            self.input_mode = InputMode::Normal;
            self.probe = None;
            return Vec::new();
        }

        let armed = self.probe.as_ref().is_some_and(|p| p.armed);
        let chord = chord_of(key);

        // After the leader fires, the next key is a sequence, matched exactly:
        // `s` and `S` are deliberately different bindings.
        let matched = if armed {
            literal_key(key).and_then(|typed| {
                self.binding_rows.iter().position(|row| {
                    row.target == BindingTarget::Binding(BindingKind::Leader)
                        && row.trigger == typed
                })
            })
        } else {
            let normalized = copycat_protocol::normalize_trigger(&chord);
            let token = key_token(key);
            self.binding_rows.iter().position(|row| match row.target {
                BindingTarget::Binding(BindingKind::Leader) => false,
                // A TUI key matches by token, and any of its keys will do.
                BindingTarget::Binding(BindingKind::Tui) => {
                    row.trigger.split_whitespace().any(|k| k == token)
                }
                _ => {
                    !row.trigger.is_empty()
                        && copycat_protocol::normalize_trigger(&row.trigger)
                            .eq_ignore_ascii_case(&normalized)
                }
            })
        };

        // Hitting the leader arms the next keystroke instead of ending the probe.
        let hit_leader = matched
            .and_then(|index| self.binding_rows.get(index))
            .is_some_and(|row| row.target == BindingTarget::Leader);

        if let Some(index) = matched {
            self.binding_selected = index;
        }
        self.probe = Some(KeyProbe {
            note: self.terminal_note(key, matched.is_some()),
            raw_modifiers: raw_modifiers(key.modifiers),
            chord: if armed { literal_key(key).unwrap_or(chord) } else { chord },
            matched,
            armed: hit_leader,
        });
        Vec::new()
    }

    /// What the terminal did to a keystroke before it reached us.
    ///
    /// Without this, a chord the terminal mangled or swallowed looks
    /// indistinguishable from a chord that is simply not bound — and the user
    /// goes off to fix a binding that was never wrong.
    fn terminal_note(&self, key: KeyEvent, matched: bool) -> Option<String> {
        // macOS terminals treat Option as a compose key unless told otherwise,
        // so option+v arrives as `√` with no modifier at all.
        if let KeyCode::Char(c) = key.code
            && !c.is_ascii()
            && key.modifiers.is_empty()
        {
            return Some(format!(
                "your terminal turned Option into `{c}` instead of a modifier — \
                 turn on Option-as-Meta to test Option chords"
            ));
        }
        if matched {
            return None;
        }
        if !self.keyboard_enhanced {
            return Some(
                "this terminal reports only shift, ctrl and alt, so cmd/super chords \
                 never reach the TUI at all"
                    .to_string(),
            );
        }
        None
    }

    fn search_key(&mut self, key: KeyEvent) -> Vec<AppRequest> {
        match key.code {
            KeyCode::Esc => {
                self.input_mode = InputMode::Normal;
                self.search.clear();
                return vec![AppRequest::Refresh];
            }
            KeyCode::Enter => {
                self.input_mode = InputMode::Normal;
                return vec![AppRequest::Refresh];
            }
            KeyCode::Backspace => {
                self.search.pop();
            }
            KeyCode::Char(c) => self.search.push(c),
            _ => return Vec::new(),
        }
        vec![AppRequest::Refresh]
    }

    fn list_len(&self) -> usize {
        match self.tab {
            Tab::Bindings => self.binding_rows.len(),
            _ => self.clips.len(),
        }
    }

    fn select(&mut self, index: usize) {
        match self.tab {
            Tab::Bindings => self.binding_selected = index,
            _ => self.selected = index,
        }
    }

    fn move_selection(&mut self, delta: isize) {
        let len = self.list_len();
        if len == 0 {
            self.select(0);
            return;
        }
        let current = match self.tab {
            Tab::Bindings => self.binding_selected,
            _ => self.selected,
        };
        let next = match delta {
            d if d < 0 => current.saturating_sub(d.unsigned_abs()),
            d => (current + d as usize).min(len - 1),
        };
        self.select(next);
    }
}

/// A key event as a config would spell the chord, using this platform's own
/// names for the keys — `cmd` and `option` on a Mac, not `super` and `alt`.
/// Matching normalizes both sides, so the display can be honest without the
/// comparison caring.
fn chord_of(key: KeyEvent) -> String {
    let mut parts = Vec::new();
    if key.modifiers.contains(KeyModifiers::CONTROL) {
        parts.push("ctrl".to_string());
    }
    if key.modifiers.contains(KeyModifiers::ALT) {
        parts.push(alt_name().to_string());
    }
    // SUPER is Command on macOS. It, META and HYPER only ever arrive from a
    // terminal speaking the Kitty keyboard protocol; a legacy terminal cannot
    // express them, which is why the TUI reports that separately.
    if key.modifiers.intersects(KeyModifiers::SUPER | KeyModifiers::META | KeyModifiers::HYPER) {
        parts.push(super_name().to_string());
    }
    if key.modifiers.contains(KeyModifiers::SHIFT) {
        parts.push("shift".to_string());
    }
    parts.push(key_name(key.code));
    parts.join("+")
}

fn super_name() -> &'static str {
    if cfg!(target_os = "macos") {
        "cmd"
    } else if cfg!(windows) {
        "win"
    } else {
        "super"
    }
}

fn alt_name() -> &'static str {
    if cfg!(target_os = "macos") { "option" } else { "alt" }
}

/// The modifier bits exactly as crossterm delivered them.
fn raw_modifiers(modifiers: KeyModifiers) -> String {
    let names = [
        (KeyModifiers::SHIFT, "shift"),
        (KeyModifiers::CONTROL, "ctrl"),
        (KeyModifiers::ALT, "alt"),
        (KeyModifiers::SUPER, "super"),
        (KeyModifiers::HYPER, "hyper"),
        (KeyModifiers::META, "meta"),
    ];
    let set: Vec<&str> = names
        .iter()
        .filter(|(bit, _)| modifiers.contains(*bit))
        .map(|(_, name)| *name)
        .collect();
    if set.is_empty() { "none".to_string() } else { set.join("+") }
}

/// The bare character a key produced, for matching leader sequences.
fn literal_key(key: KeyEvent) -> Option<String> {
    match key.code {
        KeyCode::Char(c) => Some(c.to_string()),
        _ => None,
    }
}

fn key_name(code: KeyCode) -> String {
    match code {
        // A space arrives as a character but is written as a word in a chord,
        // which is what `ctrl+alt+space` depends on.
        KeyCode::Char(' ') => "space".into(),
        // Shift is already reported as a modifier, so the letter is reported
        // in its unshifted form and the two do not double up.
        KeyCode::Char(c) => c.to_ascii_lowercase().to_string(),
        KeyCode::F(n) => format!("f{n}"),
        KeyCode::Enter => "enter".into(),
        KeyCode::Tab | KeyCode::BackTab => "tab".into(),
        KeyCode::Backspace => "backspace".into(),
        KeyCode::Delete => "delete".into(),
        KeyCode::Insert => "insert".into(),
        KeyCode::Home => "home".into(),
        KeyCode::End => "end".into(),
        KeyCode::PageUp => "pageup".into(),
        KeyCode::PageDown => "pagedown".into(),
        KeyCode::Up => "arrowup".into(),
        KeyCode::Down => "arrowdown".into(),
        KeyCode::Left => "arrowleft".into(),
        KeyCode::Right => "arrowright".into(),
        KeyCode::Esc => "escape".into(),
        other => format!("{other:?}").to_lowercase(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use copycat_core::ContentHash;

    fn key(code: KeyCode) -> KeyEvent {
        KeyEvent::new(code, KeyModifiers::NONE)
    }

    fn clip(id: u64, pinned: bool) -> ClipSummary {
        ClipSummary {
            id: ClipId(id),
            captured_at: 0,
            content_hash: ContentHash([0; 32]),
            media_types: vec!["text/plain".into()],
            byte_len: 1,
            preview: format!("clip {id}"),
            pinned,
            duplicate_run: 1,
        }
    }

    fn app_with_clips() -> App {
        let mut app = App::default();
        app.set_clips(vec![clip(3, false), clip(2, true), clip(1, false)]);
        app
    }

    fn app_on(tab: Tab) -> App {
        App { tab, ..App::default() }
    }

    #[test]
    fn selection_stays_inside_the_list() {
        let mut app = app_with_clips();
        for _ in 0..10 {
            app.on_key(key(KeyCode::Char('j')));
        }
        assert_eq!(app.selected, 2);
        for _ in 0..10 {
            app.on_key(key(KeyCode::Char('k')));
        }
        assert_eq!(app.selected, 0);
    }

    #[test]
    fn selection_survives_the_list_shrinking_underneath_it() {
        let mut app = app_with_clips();
        app.selected = 2;
        app.set_clips(vec![clip(3, false)]);
        assert_eq!(app.selected, 0);
    }

    #[test]
    fn enter_pastes_the_selected_clip() {
        let mut app = app_with_clips();
        app.on_key(key(KeyCode::Char('j')));
        assert_eq!(app.on_key(key(KeyCode::Enter)), vec![AppRequest::Paste(ClipId(2))]);
    }

    #[test]
    fn n_pastes_the_next_item_and_enter_on_the_session_screen_does_too() {
        // The cursor only moves through paste-next. Without a key for it, a
        // stack started from the TUI could never advance - which is exactly
        // what was reported.
        let mut app = app_with_clips();
        assert_eq!(app.on_key(key(KeyCode::Char('n'))), vec![AppRequest::PasteNext]);

        app.tab = Tab::Session;
        assert_eq!(app.on_key(key(KeyCode::Enter)), vec![AppRequest::PasteNext]);
    }

    #[test]
    fn enter_on_history_pastes_by_id_which_does_not_advance() {
        // Deliberate (R12), and worth pinning so nobody "fixes" it into
        // consuming the session by accident.
        let mut app = app_with_clips();
        assert!(matches!(app.on_key(key(KeyCode::Enter))[0], AppRequest::Paste(_)));
    }

    #[test]
    fn a_miss_reports_exactly_which_modifiers_arrived() {
        let mut app = bindings_app();
        app.on_key(key(KeyCode::Char('t')));
        app.on_key(KeyEvent::new(
            KeyCode::Char('q'),
            KeyModifiers::SUPER | KeyModifiers::SHIFT,
        ));
        let probe = app.probe.clone().unwrap();
        assert_eq!(probe.matched, None);
        assert_eq!(probe.raw_modifiers, "shift+super");
    }

    #[test]
    fn pin_toggles_against_the_clip_state() {
        let mut app = app_with_clips();
        assert_eq!(
            app.on_key(key(KeyCode::Char('p'))),
            vec![AppRequest::SetPinned(ClipId(3), true)]
        );
        app.selected = 1; // already pinned
        assert_eq!(
            app.on_key(key(KeyCode::Char('p'))),
            vec![AppRequest::SetPinned(ClipId(2), false)]
        );
    }

    fn bindings_app() -> App {
        let mut app = App { tab: Tab::Bindings, ..App::default() };
        app.set_bindings(BindingsView {
            leader: Some("ctrl+alt+space".into()),
            sequences: vec![Binding {
                trigger: "s".into(),
                action: "stack.start".into(),
                args: serde_json::json!({"duplicates": "collapse"}),
            }],
            hotkeys: vec![Binding {
                trigger: "ctrl+alt+v".into(),
                action: "paste.next".into(),
                args: serde_json::Value::Null,
            }],
            tui: Vec::new(),
            rejected: Vec::new(),
        });
        app
    }

    fn tui_binding(action: &str, key: &str) -> Binding {
        Binding { trigger: key.into(), action: action.into(), args: serde_json::Value::Null }
    }

    fn pick_action(app: &mut App, name: &str) {
        // The action field is a picker; walk it rather than type.
        while app.draft.as_ref().unwrap().field != DraftField::Action {
            app.on_key(key(KeyCode::Tab));
        }
        let mut guard = 0;
        while app.draft.as_ref().unwrap().action_name() != name {
            app.on_key(key(KeyCode::Right));
            guard += 1;
            assert!(guard < 64, "{name} is not offered by the picker");
        }
    }

    fn type_text(app: &mut App, text: &str) {
        for c in text.chars() {
            app.on_key(key(KeyCode::Char(c)));
        }
    }

    #[test]
    fn deleting_takes_two_keys_and_shows_the_first_one_pending() {
        let mut app = app_with_clips();

        assert!(app.on_key(key(KeyCode::Char('d'))).is_empty(), "one d must not delete");
        assert_eq!(app.pending, "d", "the half-typed command has to be visible");

        assert_eq!(app.on_key(key(KeyCode::Char('d'))), vec![AppRequest::Delete(ClipId(3))]);
        assert!(app.pending.is_empty());
    }

    #[test]
    fn a_mistyped_sequence_does_nothing_at_all() {
        // Falling through to the normal handler would make `dx` end the
        // session, which is not what anyone typing `dd` intended.
        let mut app = app_with_clips();
        app.on_key(key(KeyCode::Char('d')));

        assert!(app.on_key(key(KeyCode::Char('x'))).is_empty());
        assert!(app.pending.is_empty());
    }

    #[test]
    fn the_mode_is_the_session_and_falls_back_to_normal() {
        let mut app = App::default();
        assert_eq!(app.mode_label(), "NORMAL");

        app.input_mode = InputMode::Search;
        assert_eq!(app.mode_label(), "SEARCH", "a text field displaces the session");

        app.input_mode = InputMode::Editing;
        assert_eq!(app.mode_label(), "EDIT");
    }

    #[test]
    fn the_leader_leads_the_list_and_is_editable_like_anything_else() {
        let app = bindings_app();
        let leader = &app.binding_rows[0];
        assert_eq!(leader.target, BindingTarget::Leader);
        assert_eq!(leader.trigger, "ctrl+alt+space");
    }

    #[test]
    fn editing_the_leader_submits_a_new_chord() {
        let mut app = bindings_app();
        app.on_key(key(KeyCode::Enter)); // row 0 is the leader

        let draft = app.draft.clone().expect("the leader row should open a form");
        assert_eq!(draft.target, BindingTarget::Leader);
        // The leader has no action, so the form must not offer one.
        assert_eq!(draft.fields(), vec![DraftField::Enabled, DraftField::Trigger]);

        for _ in 0.."ctrl+alt+space".len() {
            app.on_key(key(KeyCode::Backspace));
        }
        type_text(&mut app, "ctrl+space");
        let requests = app.on_key(key(KeyCode::Enter));

        assert_eq!(
            requests,
            vec![AppRequest::SetLeader {
                trigger: Some("ctrl+space".into()),
                enabled: Some(true)
            }]
        );
    }

    #[test]
    fn the_leader_is_disarmed_rather_than_deleted() {
        // There is no such thing as no leader, so dd has to say what to do
        // instead of silently doing nothing.
        let mut app = bindings_app();
        app.on_key(key(KeyCode::Char('d')));
        let requests = app.on_key(key(KeyCode::Char('d')));

        assert!(requests.is_empty());
        let message = app.message.clone().expect("it should say why");
        assert!(message.text.contains("cannot be deleted"), "{}", message.text);
    }

    #[test]
    fn the_leader_can_be_switched_off_from_the_form() {
        let mut app = bindings_app();
        app.on_key(key(KeyCode::Enter));
        app.on_key(key(KeyCode::BackTab)); // trigger -> enabled
        app.on_key(key(KeyCode::Char(' ')));

        assert_eq!(
            app.on_key(key(KeyCode::Enter)),
            vec![AppRequest::SetLeader {
                trigger: Some("ctrl+alt+space".into()),
                enabled: Some(false)
            }]
        );
    }

    #[test]
    fn editing_a_binding_prefills_the_form_and_submits_a_change() {
        let mut app = bindings_app();
        app.on_key(key(KeyCode::Char('j'))); // past the leader
        app.on_key(key(KeyCode::Enter));

        let draft = app.draft.clone().expect("the form should open on the selected row");
        assert_eq!(draft.trigger, "s");
        assert_eq!(draft.action_name(), "stack.start");
        assert_eq!(draft.kind, BindingKind::Leader);
        // The stored {"duplicates":"collapse"} is read back into the selector.
        assert_eq!(draft.args, vec![ArgValue::Choice(Some(0))]);

        pick_action(&mut app, "queue.capture");
        let requests = app.on_key(key(KeyCode::Enter));

        assert_eq!(
            requests,
            vec![AppRequest::SetBinding {
                kind: BindingKind::Leader,
                trigger: "s".into(),
                action: "queue.capture".into(),
                // Choosing a different action resets its arguments: the old
                // ones may not apply, and unset means the daemon's default.
                args: serde_json::Value::Null,
            }]
        );
        assert_eq!(app.input_mode, InputMode::Normal);
    }

    #[test]
    fn renaming_a_trigger_removes_the_old_binding_too() {
        // Otherwise an edit would quietly leave two bindings where there was
        // one, and the old key would keep firing.
        let mut app = bindings_app();
        app.on_key(key(KeyCode::Char('j')));
        app.on_key(key(KeyCode::Char('e')));
        app.on_key(key(KeyCode::Backspace));
        type_text(&mut app, "S");

        let requests = app.on_key(key(KeyCode::Enter));

        assert_eq!(requests.len(), 2, "{requests:?}");
        assert_eq!(
            requests[0],
            AppRequest::RemoveBinding { kind: BindingKind::Leader, trigger: "s".into() }
        );
        assert!(matches!(&requests[1], AppRequest::SetBinding { trigger, .. } if trigger == "S"));
    }

    #[test]
    fn a_required_argument_is_named_and_a_non_number_is_refused() {
        let mut app = bindings_app();
        app.on_key(key(KeyCode::Char('a')));
        type_text(&mut app, "z");
        pick_action(&mut app, "queue.start");

        // Submit with `last` unset.
        assert!(app.on_key(key(KeyCode::Enter)).is_empty());
        let error = app.draft.as_ref().unwrap().error.clone().unwrap();
        assert!(error.contains("last is required"), "{error}");
        assert_eq!(app.input_mode, InputMode::Editing, "the typing must not be thrown away");

        // Now a value that is not a number.
        app.on_key(key(KeyCode::Tab)); // to `last`
        type_text(&mut app, "five");
        assert!(app.on_key(key(KeyCode::Enter)).is_empty());
        let error = app.draft.as_ref().unwrap().error.clone().unwrap();
        assert!(error.contains("whole number"), "{error}");

        // And a good one, with the enum chosen too.
        for _ in 0..4 {
            app.on_key(key(KeyCode::Backspace));
        }
        type_text(&mut app, "5");
        app.on_key(key(KeyCode::Tab)); // to `duplicates`
        app.on_key(key(KeyCode::Right)); // unset -> collapse
        app.on_key(key(KeyCode::Right)); // collapse -> preserve
        let requests = app.on_key(key(KeyCode::Enter));
        assert_eq!(
            requests,
            vec![AppRequest::SetBinding {
                kind: BindingKind::Leader,
                trigger: "z".into(),
                action: "queue.start".into(),
                args: serde_json::json!({"last": 5, "duplicates": "preserve"}),
            }]
        );
    }

    #[test]
    fn the_action_picker_shows_what_each_action_does_and_its_arguments() {
        let mut app = bindings_app();
        app.on_key(key(KeyCode::Char('a')));
        pick_action(&mut app, "group.paste_last");

        let draft = app.draft.as_ref().unwrap();
        assert!(draft.help(DraftField::Action).contains("joined as one value"), "{}", draft.help(DraftField::Action));
        // Exactly the spec's arguments, in order, as form fields.
        let labels: Vec<String> = draft.fields().into_iter().map(|f| draft.label(f)).collect();
        assert_eq!(labels, ["kind", "trigger", "action", "last", "delimiter", "raw"]);
        assert!(draft.help(DraftField::Arg(0)).contains("required"));
    }

    #[test]
    fn an_optional_argument_left_unset_is_not_sent() {
        // So the daemon's default applies rather than a value the form guessed.
        let mut app = bindings_app();
        app.on_key(key(KeyCode::Char('a')));
        type_text(&mut app, "z");
        pick_action(&mut app, "stack.start");
        let requests = app.on_key(key(KeyCode::Enter));
        assert!(matches!(&requests[0], AppRequest::SetBinding { args, .. } if args.is_null()));
    }

    #[test]
    fn ctrl_r_captures_the_next_key_as_the_trigger() {
        let mut app = bindings_app();
        app.on_key(key(KeyCode::Char('a')));
        // kind -> hotkey, so the capture is a chord
        app.on_key(key(KeyCode::BackTab));
        app.on_key(key(KeyCode::Right));
        app.on_key(key(KeyCode::Tab)); // back to trigger
        app.on_key(KeyEvent::new(KeyCode::Char('r'), KeyModifiers::CONTROL));
        assert!(app.draft.as_ref().unwrap().capturing);

        app.on_key(KeyEvent::new(KeyCode::Char('v'), KeyModifiers::CONTROL | KeyModifiers::ALT));

        let draft = app.draft.as_ref().unwrap();
        assert!(!draft.capturing);
        assert_eq!(draft.trigger, "ctrl+alt+v");
    }

    #[test]
    fn a_captured_leader_sequence_keeps_its_case() {
        // `s` and `S` are different bindings, so capture must not fold them.
        let mut app = bindings_app();
        app.on_key(key(KeyCode::Char('a'))); // kind is leader by default
        app.on_key(KeyEvent::new(KeyCode::Char('r'), KeyModifiers::CONTROL));
        app.on_key(KeyEvent::new(KeyCode::Char('S'), KeyModifiers::SHIFT));
        assert_eq!(app.draft.as_ref().unwrap().trigger, "S");
    }

    #[test]
    fn a_binding_to_an_action_the_form_cannot_offer_says_so() {
        // Bound by hand in the config to something with no spec. The form
        // opens, but must not pretend the action shown is the one on disk.
        let mut app = App { tab: Tab::Bindings, ..App::default() };
        app.set_bindings(BindingsView {
            hotkeys: vec![Binding {
                trigger: "ctrl+alt+l".into(),
                action: "history.list".into(),
                args: serde_json::Value::Null,
            }],
            ..Default::default()
        });
        app.on_key(key(KeyCode::Char('j'))); // past the leader
        app.on_key(key(KeyCode::Enter));
        let error = app.draft.as_ref().unwrap().error.clone().expect("it should explain");
        assert!(error.contains("history.list"), "{error}");
        assert!(error.contains("cannot be bound from here"), "{error}");
    }

    #[test]
    fn the_kind_field_toggles_between_the_two_binding_classes() {
        let mut app = bindings_app();
        app.on_key(key(KeyCode::Char('a')));
        app.on_key(key(KeyCode::BackTab)); // trigger -> kind
        assert_eq!(app.draft.as_ref().unwrap().field, DraftField::Kind);

        app.on_key(key(KeyCode::Char(' ')));
        assert_eq!(app.draft.as_ref().unwrap().kind, BindingKind::Hotkey);
    }

    #[test]
    fn deleting_a_binding_removes_the_selected_one() {
        let mut app = bindings_app();
        app.on_key(key(KeyCode::Char('j')));
        app.on_key(key(KeyCode::Char('j'))); // leader, sequence, then the hotkey
        app.on_key(key(KeyCode::Char('d')));

        assert_eq!(
            app.on_key(key(KeyCode::Char('d'))),
            vec![AppRequest::RemoveBinding {
                kind: BindingKind::Hotkey,
                trigger: "ctrl+alt+v".into()
            }]
        );
    }

    #[test]
    fn test_mode_says_which_binding_a_chord_hits() {
        let mut app = bindings_app();
        app.on_key(key(KeyCode::Char('t')));
        assert_eq!(app.input_mode, InputMode::Testing);

        app.on_key(KeyEvent::new(
            KeyCode::Char('v'),
            KeyModifiers::CONTROL | KeyModifiers::ALT,
        ));

        let probe = app.probe.clone().expect("a keystroke should resolve to something");
        assert_eq!(probe.chord, "ctrl+alt+v");
        let matched = &app.binding_rows[probe.matched.expect("ctrl+alt+v is bound")];
        assert_eq!(matched.action, "paste.next");
        assert_eq!(app.binding_selected, probe.matched.unwrap(), "and it scrolls into view");
    }

    #[test]
    fn a_chord_spelled_differently_still_matches() {
        // The binding says ctrl+alt+v; a keystroke reports the same chord no
        // matter which vocabulary the config used.
        let mut app = App { tab: Tab::Bindings, ..App::default() };
        app.set_bindings(BindingsView {
            leader: Some("ctrl+alt+space".into()),
            hotkeys: vec![Binding {
                trigger: "Control+Option+v".into(),
                action: "paste.next".into(),
                args: serde_json::Value::Null,
            }],
            ..Default::default()
        });
        app.on_key(key(KeyCode::Char('t')));

        app.on_key(KeyEvent::new(
            KeyCode::Char('v'),
            KeyModifiers::CONTROL | KeyModifiers::ALT,
        ));

        assert!(app.probe.as_ref().unwrap().matched.is_some(), "Option is Alt");
    }

    #[test]
    fn hitting_the_leader_arms_the_next_key_and_then_matches_a_sequence() {
        let mut app = bindings_app();
        app.on_key(key(KeyCode::Char('t')));

        app.on_key(KeyEvent::new(
            KeyCode::Char(' '),
            KeyModifiers::CONTROL | KeyModifiers::ALT,
        ));
        assert!(app.probe.as_ref().unwrap().armed, "the leader arms rather than resolving");

        app.on_key(key(KeyCode::Char('s')));
        let probe = app.probe.clone().unwrap();
        assert_eq!(probe.chord, "s");
        assert_eq!(
            app.binding_rows[probe.matched.expect("s is a leader sequence")].action,
            "stack.start"
        );
    }

    #[test]
    fn command_is_detected_however_the_terminal_labels_it() {
        // Under the Kitty protocol Command arrives as SUPER; some terminals
        // use META. Both are the same physical key.
        let mut app = App { tab: Tab::Bindings, keyboard_enhanced: true, ..App::default() };
        app.set_bindings(BindingsView {
            leader: Some("ctrl+alt+space".into()),
            hotkeys: vec![Binding {
                trigger: "cmd+v".into(),
                action: "paste.next".into(),
                args: serde_json::Value::Null,
            }],
            ..Default::default()
        });
        app.on_key(key(KeyCode::Char('t')));

        for modifier in [KeyModifiers::SUPER, KeyModifiers::META] {
            app.on_key(KeyEvent::new(KeyCode::Char('v'), modifier));
            let probe = app.probe.clone().unwrap();
            assert_eq!(probe.chord, "super+v", "{modifier:?}");
            assert!(probe.matched.is_some(), "cmd+v should match {modifier:?}");
        }
    }

    #[test]
    fn a_terminal_that_cannot_report_command_says_so_instead_of_no_binding() {
        // Otherwise someone goes off to fix a binding that was never wrong.
        let mut app = bindings_app();
        app.keyboard_enhanced = false;
        app.on_key(key(KeyCode::Char('t')));

        app.on_key(KeyEvent::new(KeyCode::Char('q'), KeyModifiers::CONTROL));

        let note = app.probe.as_ref().unwrap().note.clone().expect("it should explain");
        assert!(note.contains("cmd/super"), "{note}");
    }

    #[test]
    fn option_arriving_as_a_composed_character_is_named_as_such() {
        // macOS terminals turn option+v into `√` with no modifier at all, which
        // otherwise looks like an unbound key.
        let mut app = bindings_app();
        app.on_key(key(KeyCode::Char('t')));

        app.on_key(key(KeyCode::Char('√')));

        let note = app.probe.as_ref().unwrap().note.clone().expect("it should explain");
        assert!(note.contains("Option"), "{note}");
        assert!(note.contains('√'), "{note}");
    }

    #[test]
    fn a_matching_chord_is_not_second_guessed() {
        let mut app = bindings_app();
        app.keyboard_enhanced = false;
        app.on_key(key(KeyCode::Char('t')));

        app.on_key(KeyEvent::new(
            KeyCode::Char('v'),
            KeyModifiers::CONTROL | KeyModifiers::ALT,
        ));

        let probe = app.probe.clone().unwrap();
        assert!(probe.matched.is_some());
        assert_eq!(probe.note, None, "a hit needs no excuse");
    }

    #[test]
    fn an_unbound_chord_reports_no_match_rather_than_the_nearest_one() {
        let mut app = bindings_app();
        app.on_key(key(KeyCode::Char('t')));
        app.on_key(KeyEvent::new(KeyCode::Char('z'), KeyModifiers::CONTROL));

        let probe = app.probe.clone().unwrap();
        assert_eq!(probe.chord, "ctrl+z");
        assert_eq!(probe.matched, None);
    }

    #[test]
    fn test_mode_does_not_run_the_commands_it_is_testing() {
        // `s` starts a stack in normal mode. In test mode it has to stay inert,
        // or checking a binding would trigger it.
        let mut app = bindings_app();
        app.on_key(key(KeyCode::Char('t')));

        assert!(app.on_key(key(KeyCode::Char('s'))).is_empty());
        assert!(app.on_key(key(KeyCode::Char('x'))).is_empty());
        assert!(app.on_key(key(KeyCode::Char('d'))).is_empty());

        app.on_key(key(KeyCode::Esc));
        assert_eq!(app.input_mode, InputMode::Normal);
    }

    // ------------------------------------------------------------ keymap

    #[test]
    fn the_default_keymap_is_what_the_hardcoded_keys_used_to_be() {
        let mut app = app_with_clips();
        assert_eq!(app.on_key(key(KeyCode::Char('n'))), vec![AppRequest::PasteNext]);
        assert_eq!(app.on_key(key(KeyCode::Char('s'))), vec![AppRequest::StackStart]);
        app.on_key(key(KeyCode::Char('j')));
        assert_eq!(app.selected, 1, "j moves down");
        app.on_key(key(KeyCode::Up));
        assert_eq!(app.selected, 0, "and so does the arrow, both bound by default");
    }

    #[test]
    fn a_keymap_override_replaces_the_default_rather_than_adding_to_it() {
        let mut app = app_with_clips();
        app.set_bindings(BindingsView {
            tui: vec![tui_binding("paste_next", "N")],
            ..Default::default()
        });

        assert_eq!(app.on_key(key(KeyCode::Char('N'))), vec![AppRequest::PasteNext]);
        assert!(app.on_key(key(KeyCode::Char('n'))).is_empty(), "the old key is gone");
    }

    #[test]
    fn a_multi_key_sequence_can_be_rebound_and_the_prefix_shows_pending() {
        let mut app = app_with_clips();
        // `z` is bound to nothing, so it can only be the start of a sequence.
        app.set_bindings(BindingsView {
            tui: vec![tui_binding("delete", "zz")],
            ..Default::default()
        });

        assert!(app.on_key(key(KeyCode::Char('z'))).is_empty());
        assert_eq!(app.pending, "z", "half a sequence must stay visible");
        assert_eq!(app.on_key(key(KeyCode::Char('z'))), vec![AppRequest::Delete(ClipId(3))]);
        assert!(app.pending.is_empty());

        // And the old dd is no longer a sequence at all.
        app.on_key(key(KeyCode::Char('d')));
        assert!(app.pending.is_empty(), "d is not a prefix any more");
    }

    #[test]
    fn several_keys_for_one_action_come_from_a_space_separated_spec() {
        let mut app = app_with_clips();
        app.set_bindings(BindingsView {
            tui: vec![tui_binding("down", "j ctrl+n")],
            ..Default::default()
        });
        app.on_key(KeyEvent::new(KeyCode::Char('n'), KeyModifiers::CONTROL));
        assert_eq!(app.selected, 1);
    }

    #[test]
    fn ctrl_c_and_escape_work_whatever_the_keymap_says() {
        // The escape hatches are outside the keymap on purpose.
        let mut app = app_with_clips();
        app.set_bindings(BindingsView {
            tui: vec![tui_binding("quit", "Q")],
            ..Default::default()
        });
        app.on_key(KeyEvent::new(KeyCode::Char('c'), KeyModifiers::CONTROL));
        assert!(app.should_quit);
    }

    #[test]
    fn every_tui_action_is_listed_on_the_bindings_screen_with_its_keys() {
        let app = bindings_app();
        let tui_rows: Vec<&BindingRow> = app
            .binding_rows
            .iter()
            .filter(|r| r.target == BindingTarget::Binding(BindingKind::Tui))
            .collect();
        assert_eq!(tui_rows.len(), TuiAction::ALL.len());

        let delete = tui_rows.iter().find(|r| r.action == "delete").unwrap();
        assert_eq!(delete.trigger, "dd");
        assert!(!delete.custom);
    }

    #[test]
    fn editing_a_tui_key_submits_the_action_it_belongs_to() {
        let mut app = bindings_app();
        let row = app
            .binding_rows
            .iter()
            .position(|r| r.action == "paste_next")
            .unwrap();
        app.binding_selected = row;
        app.on_key(key(KeyCode::Char('e')));

        let draft = app.draft.clone().unwrap();
        assert_eq!(draft.kind, BindingKind::Tui);
        assert_eq!(draft.fields(), vec![DraftField::Kind, DraftField::Trigger, DraftField::Action]);

        app.on_key(key(KeyCode::Backspace));
        type_text(&mut app, "N");
        assert_eq!(
            app.on_key(key(KeyCode::Enter)),
            vec![AppRequest::SetBinding {
                kind: BindingKind::Tui,
                trigger: "N".into(),
                action: "paste_next".into(),
                args: serde_json::Value::Null,
            }]
        );
    }

    #[test]
    fn a_tui_action_is_chosen_from_the_list_rather_than_typed() {
        let mut app = bindings_app();
        app.on_key(key(KeyCode::Char('a')));
        app.on_key(key(KeyCode::BackTab)); // to kind
        app.on_key(key(KeyCode::Right)); // leader -> hotkey
        app.on_key(key(KeyCode::Right)); // hotkey -> tui
        let draft = app.draft.as_ref().unwrap();
        assert_eq!(draft.kind, BindingKind::Tui);
        assert!(draft.actions().contains(&"paste_next"));
        assert!(!draft.actions().contains(&"stack.start"), "daemon actions are not TUI keys");
        assert!(draft.fields().iter().all(|f| !matches!(f, DraftField::Arg(_))), "TUI actions take no arguments");
    }

    #[test]
    fn deleting_a_custom_tui_key_restores_the_default_and_a_default_says_so() {
        let mut app = bindings_app();
        app.set_bindings(BindingsView {
            tui: vec![tui_binding("paste_next", "N")],
            ..app.bindings.clone()
        });
        let row = app.binding_rows.iter().position(|r| r.action == "paste_next").unwrap();
        assert!(app.binding_rows[row].custom);

        app.binding_selected = row;
        app.on_key(key(KeyCode::Char('d')));
        assert_eq!(
            app.on_key(key(KeyCode::Char('d'))),
            vec![AppRequest::RemoveBinding { kind: BindingKind::Tui, trigger: "paste_next".into() }]
        );

        // At default there is nothing to remove; it says so instead of
        // sending a request that would fail.
        let default_row = app.binding_rows.iter().position(|r| r.action == "quit").unwrap();
        app.binding_selected = default_row;
        app.on_key(key(KeyCode::Char('d')));
        assert!(app.on_key(key(KeyCode::Char('d'))).is_empty());
        assert!(app.message.as_ref().unwrap().text.contains("already at its default"));
    }

    #[test]
    fn escape_leaves_the_form_without_saving() {
        let mut app = bindings_app();
        app.on_key(key(KeyCode::Char('a')));
        type_text(&mut app, "x");

        assert!(app.on_key(key(KeyCode::Esc)).is_empty());
        assert!(app.draft.is_none());
        assert_eq!(app.input_mode, InputMode::Normal);
        assert!(!app.should_quit, "escape closes the form rather than the program");
    }

    #[test]
    fn an_empty_list_yields_no_requests() {
        let mut app = App::default();
        assert!(app.on_key(key(KeyCode::Enter)).is_empty());
        assert!(app.on_key(key(KeyCode::Char('d'))).is_empty());
        assert!(app.on_key(key(KeyCode::Char('d'))).is_empty(), "even completed");
        assert!(app.on_key(key(KeyCode::Char('p'))).is_empty());
    }

    #[test]
    fn search_captures_text_instead_of_commands() {
        let mut app = app_with_clips();
        app.on_key(key(KeyCode::Char('/')));
        assert_eq!(app.input_mode, InputMode::Search);

        // 'q' would quit in normal mode; here it is just a letter.
        for c in "qdx".chars() {
            app.on_key(key(KeyCode::Char(c)));
        }
        assert_eq!(app.search, "qdx");
        assert!(!app.should_quit);

        app.on_key(key(KeyCode::Backspace));
        assert_eq!(app.search, "qd");

        app.on_key(key(KeyCode::Esc));
        assert_eq!(app.input_mode, InputMode::Normal);
        assert!(app.search.is_empty(), "escape abandons the search");
        assert!(!app.should_quit, "escape leaves search rather than quitting");
    }

    #[test]
    fn help_swallows_the_key_that_dismisses_it() {
        // Otherwise dismissing help with 'd' would also delete a clip.
        let mut app = app_with_clips();
        app.on_key(key(KeyCode::Char('?')));
        assert!(app.show_help);

        let requests = app.on_key(key(KeyCode::Char('d')));

        assert!(!app.show_help);
        assert!(requests.is_empty());
        assert!(app.pending.is_empty(), "and does not begin a sequence either");
        assert_eq!(app.clips.len(), 3);
    }

    #[test]
    fn tabs_cycle_in_both_directions() {
        let mut app = App::default();
        app.on_key(key(KeyCode::Tab));
        assert_eq!(app.tab, Tab::Session);
        app.on_key(key(KeyCode::BackTab));
        assert_eq!(app.tab, Tab::History);
        app.on_key(key(KeyCode::Char('4')));
        assert_eq!(app.tab, Tab::Diagnostics);
        app.on_key(key(KeyCode::Tab));
        assert_eq!(app.tab, Tab::History, "the last tab wraps to the first");
    }

    #[test]
    fn ctrl_c_quits() {
        let mut app = App::default();
        app.on_key(KeyEvent::new(KeyCode::Char('c'), KeyModifiers::CONTROL));
        assert!(app.should_quit);
    }

    #[test]
    fn plain_c_starts_a_queue_capture_rather_than_quitting() {
        let mut app = App::default();
        assert_eq!(app.on_key(key(KeyCode::Char('c'))), vec![AppRequest::QueueCapture]);
        assert!(!app.should_quit);
    }

    #[test]
    fn session_controls_work_from_every_tab() {
        let mut app = app_on(Tab::Diagnostics);
        assert_eq!(app.on_key(key(KeyCode::Char('s'))), vec![AppRequest::StackStart]);
        assert_eq!(app.on_key(key(KeyCode::Char('x'))), vec![AppRequest::SessionStop]);
    }

    #[test]
    fn toggling_the_raw_view_asks_for_fresh_data() {
        let mut app = app_with_clips();
        assert_eq!(app.on_key(key(KeyCode::Char('a'))), vec![AppRequest::Refresh]);
        assert!(app.raw);
    }
}
