//! TUI state and key handling.
//!
//! Deliberately free of any terminal or socket: keys go in, [`AppRequest`]s
//! come out, and the runner performs them. That makes every binding testable
//! without a pty, and keeps the rule from ADR-003 intact — the TUI decides
//! nothing about clipboard semantics, it only asks the daemon.

use copycat_core::{ClipId, ClipSummary, SessionMode, SessionState};
use copycat_protocol::{Binding, BindingKind, DoctorReport, RejectedBinding, StatusReport};
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
}

/// Something the runner should ask the daemon to do.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum AppRequest {
    Refresh,
    Paste(ClipId),
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
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DraftField {
    Kind,
    /// Leader only: whether it is armed at all.
    Enabled,
    Trigger,
    Action,
    Args,
}

impl DraftField {
    pub fn label(self) -> &'static str {
        match self {
            DraftField::Kind => "kind",
            DraftField::Enabled => "enabled",
            DraftField::Trigger => "trigger",
            DraftField::Action => "action",
            DraftField::Args => "args",
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
    pub action: String,
    /// Raw JSON, so anything the protocol accepts can be typed.
    pub args: String,
    pub field: DraftField,
    /// The binding this replaces, when editing rather than adding. Kept so a
    /// renamed trigger removes the old one instead of leaving both.
    pub replacing: Option<(BindingKind, String)>,
    pub error: Option<String>,
}

impl BindingDraft {
    fn blank() -> Self {
        BindingDraft {
            target: BindingTarget::Binding(BindingKind::Leader),
            kind: BindingKind::Leader,
            enabled: true,
            trigger: String::new(),
            action: String::new(),
            args: String::new(),
            field: DraftField::Trigger,
            replacing: None,
            error: None,
        }
    }

    fn editing(row: &BindingRow) -> Self {
        BindingDraft {
            target: row.target,
            kind: match row.target {
                BindingTarget::Binding(kind) => kind,
                BindingTarget::Leader => BindingKind::Leader,
            },
            enabled: row.inactive.is_none(),
            trigger: row.trigger.clone(),
            action: row.action.clone(),
            args: match &row.args {
                serde_json::Value::Null => String::new(),
                other if other.as_object().is_some_and(|o| o.is_empty()) => String::new(),
                other => other.to_string(),
            },
            field: DraftField::Trigger,
            replacing: match row.target {
                BindingTarget::Binding(kind) => Some((kind, row.trigger.clone())),
                BindingTarget::Leader => None,
            },
            error: None,
        }
    }

    /// Only the fields this target actually has. The leader has no action, and
    /// showing it an empty one would invite filling it in.
    pub fn fields(&self) -> &'static [DraftField] {
        match self.target {
            BindingTarget::Leader => &[DraftField::Enabled, DraftField::Trigger],
            BindingTarget::Binding(_) => {
                &[DraftField::Kind, DraftField::Trigger, DraftField::Action, DraftField::Args]
            }
        }
    }

    fn step(&mut self, forward: bool) {
        let fields = self.fields();
        let index = fields.iter().position(|f| *f == self.field).unwrap_or(0);
        let len = fields.len();
        self.field = fields[if forward { (index + 1) % len } else { (index + len - 1) % len }];
    }

    /// Whether the focused field is a switch rather than text.
    fn toggle(&mut self) {
        match self.field {
            DraftField::Kind => {
                self.kind = match self.kind {
                    BindingKind::Leader => BindingKind::Hotkey,
                    BindingKind::Hotkey => BindingKind::Leader,
                }
            }
            DraftField::Enabled => self.enabled = !self.enabled,
            _ => {}
        }
    }

    fn text_mut(&mut self) -> Option<&mut String> {
        match self.field {
            DraftField::Kind | DraftField::Enabled => None,
            DraftField::Trigger => Some(&mut self.trigger),
            DraftField::Action => Some(&mut self.action),
            DraftField::Args => Some(&mut self.args),
        }
    }

    pub fn value(&self, field: DraftField) -> String {
        match field {
            DraftField::Kind => self.kind.as_str().to_string(),
            DraftField::Enabled => if self.enabled { "yes" } else { "no" }.to_string(),
            DraftField::Trigger => self.trigger.clone(),
            DraftField::Action => self.action.clone(),
            DraftField::Args => self.args.clone(),
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
        if self.action.trim().is_empty() {
            return Err("an action is required".into());
        }
        let args = if self.args.trim().is_empty() {
            serde_json::Value::Null
        } else {
            serde_json::from_str(&self.args).map_err(|e| format!("args is not valid JSON: {e}"))?
        };

        let mut requests = Vec::new();
        // Renaming a trigger has to delete the old row, or an edit would
        // quietly leave two bindings where there was one.
        if let Some((kind, trigger)) = &self.replacing
            && (*kind != self.kind || *trigger != self.trigger)
        {
            requests.push(AppRequest::RemoveBinding {
                kind: *kind,
                trigger: trigger.clone(),
            });
        }
        requests.push(AppRequest::SetBinding {
            kind: self.kind,
            trigger: self.trigger.trim().to_string(),
            action: self.action.trim().to_string(),
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
    pub rejected: Vec<RejectedBinding>,
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
                });
            }
        }

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
            self.pending.clear();
            return Vec::new();
        }
        if !self.pending.is_empty() {
            return self.resolve_pending(key);
        }

        self.message = None;

        match key.code {
            KeyCode::Char('c') if key.modifiers.contains(KeyModifiers::CONTROL) => {
                self.should_quit = true;
            }
            KeyCode::Char('q') | KeyCode::Esc => self.should_quit = true,
            KeyCode::Char('?') => self.show_help = true,

            KeyCode::Tab => self.tab = self.tab.next(),
            KeyCode::BackTab => self.tab = self.tab.previous(),
            KeyCode::Char('1') => self.tab = Tab::History,
            KeyCode::Char('2') => self.tab = Tab::Session,
            KeyCode::Char('3') => self.tab = Tab::Bindings,
            KeyCode::Char('4') => self.tab = Tab::Diagnostics,

            KeyCode::Char('j') | KeyCode::Down => self.move_selection(1),
            KeyCode::Char('k') | KeyCode::Up => self.move_selection(-1),
            KeyCode::Home => self.select(0),
            KeyCode::End => self.select(self.list_len().saturating_sub(1)),

            KeyCode::Char('r') => return vec![AppRequest::Refresh],
            KeyCode::Char('/') if self.tab == Tab::History => {
                self.input_mode = InputMode::Search;
                self.search.clear();
            }
            KeyCode::Char('a') if self.tab == Tab::History => {
                self.raw = !self.raw;
                return vec![AppRequest::Refresh];
            }

            // Deleting is a two-key sequence everywhere it appears. It is the
            // only destructive key in the interface, and making it the one
            // command that needs confirming is cheaper than a modal.
            KeyCode::Char('d') if matches!(self.tab, Tab::History | Tab::Bindings) => {
                self.pending.push('d');
            }

            KeyCode::Char('a') if self.tab == Tab::Bindings => {
                self.draft = Some(BindingDraft::blank());
                self.input_mode = InputMode::Editing;
            }
            KeyCode::Char('t') if self.tab == Tab::Bindings => {
                self.input_mode = InputMode::Testing;
                self.probe = None;
            }
            KeyCode::Char('e') if self.tab == Tab::Bindings => {
                if let Some(row) = self.selected_binding() {
                    self.draft = Some(BindingDraft::editing(row));
                    self.input_mode = InputMode::Editing;
                }
            }

            KeyCode::Enter => {
                return match self.tab {
                    Tab::History => self
                        .selected_clip()
                        .map(|clip| vec![AppRequest::Paste(clip.id)])
                        .unwrap_or_default(),
                    // Enter on a binding opens it, which is what selecting a
                    // row implies. Reloading moved to `r` with everything else.
                    Tab::Bindings => {
                        if let Some(row) = self.selected_binding() {
                            self.draft = Some(BindingDraft::editing(row));
                            self.input_mode = InputMode::Editing;
                        }
                        Vec::new()
                    }
                    _ => Vec::new(),
                };
            }
            KeyCode::Char('p') if self.tab == Tab::History => {
                if let Some(clip) = self.selected_clip() {
                    return vec![AppRequest::SetPinned(clip.id, !clip.pinned)];
                }
            }
            KeyCode::Char(' ') => return vec![AppRequest::TogglePause],

            // Session controls. Available from any screen: they are the fastest
            // path to starting a mode, and hunting for the right tab first
            // would defeat that.
            KeyCode::Char('s') => return vec![AppRequest::StackStart],
            KeyCode::Char('c') => return vec![AppRequest::QueueCapture],
            KeyCode::Char('S') => return vec![AppRequest::QueueSeal],
            KeyCode::Char('g') => return vec![AppRequest::GroupCapture],
            KeyCode::Char('G') => return vec![AppRequest::GroupPaste],
            KeyCode::Char('x') => return vec![AppRequest::SessionStop],
            KeyCode::Char('0') => return vec![AppRequest::SessionReset],
            _ => {}
        }
        Vec::new()
    }

    /// Finish, or abandon, a sequence already in progress.
    fn resolve_pending(&mut self, key: KeyEvent) -> Vec<AppRequest> {
        let pending = std::mem::take(&mut self.pending);
        match (pending.as_str(), key.code) {
            ("d", KeyCode::Char('d')) => self.delete_selected(),
            // Anything else abandons the sequence without acting on it, the way
            // vim does. Falling through to the normal handler would make a
            // mistyped `dx` stop the session.
            _ => Vec::new(),
        }
    }

    fn delete_selected(&mut self) -> Vec<AppRequest> {
        match self.tab {
            Tab::History => self
                .selected_clip()
                .map(|clip| vec![AppRequest::Delete(clip.id)])
                .unwrap_or_default(),
            Tab::Bindings => match self.selected_binding().map(|row| (row.target, row.trigger.clone())) {
                // There is no such thing as no leader, only a disarmed one.
                Some((BindingTarget::Leader, _)) => {
                    self.note("the leader cannot be deleted — press e and set enabled to no");
                    Vec::new()
                }
                Some((BindingTarget::Binding(kind), trigger)) => {
                    vec![AppRequest::RemoveBinding { kind, trigger }]
                }
                None => Vec::new(),
            },
            _ => Vec::new(),
        }
    }

    fn edit_key(&mut self, key: KeyEvent) -> Vec<AppRequest> {
        let Some(draft) = self.draft.as_mut() else {
            self.input_mode = InputMode::Normal;
            return Vec::new();
        };

        match key.code {
            KeyCode::Esc => {
                self.draft = None;
                self.input_mode = InputMode::Normal;
            }
            KeyCode::Tab | KeyCode::Down => draft.step(true),
            KeyCode::BackTab | KeyCode::Up => draft.step(false),
            KeyCode::Left | KeyCode::Right | KeyCode::Char(' ')
                if matches!(draft.field, DraftField::Kind | DraftField::Enabled) =>
            {
                draft.toggle();
            }
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
            self.binding_rows.iter().position(|row| {
                !matches!(row.target, BindingTarget::Binding(BindingKind::Leader))
                    && !row.trigger.is_empty()
                    && copycat_protocol::normalize_trigger(&row.trigger)
                        .eq_ignore_ascii_case(&normalized)
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

/// A key event as a config would spell the chord.
fn chord_of(key: KeyEvent) -> String {
    let mut parts = Vec::new();
    if key.modifiers.contains(KeyModifiers::CONTROL) {
        parts.push("ctrl".to_string());
    }
    if key.modifiers.contains(KeyModifiers::ALT) {
        parts.push("alt".to_string());
    }
    // SUPER is Command on macOS. Both it and META only ever arrive from a
    // terminal speaking the Kitty keyboard protocol; a legacy terminal cannot
    // express them, which is why the TUI reports that separately.
    if key.modifiers.intersects(KeyModifiers::SUPER | KeyModifiers::META | KeyModifiers::HYPER) {
        parts.push("super".to_string());
    }
    if key.modifiers.contains(KeyModifiers::SHIFT) {
        parts.push("shift".to_string());
    }
    parts.push(key_name(key.code));
    parts.join("+")
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
            rejected: Vec::new(),
        });
        app
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
        assert_eq!(draft.fields(), &[DraftField::Enabled, DraftField::Trigger]);

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
        assert_eq!(draft.action, "stack.start");
        assert_eq!(draft.kind, BindingKind::Leader);

        // Retype the action.
        app.on_key(key(KeyCode::Tab));
        for _ in 0.."stack.start".len() {
            app.on_key(key(KeyCode::Backspace));
        }
        type_text(&mut app, "queue.capture");
        let requests = app.on_key(key(KeyCode::Enter));

        assert_eq!(
            requests,
            vec![AppRequest::SetBinding {
                kind: BindingKind::Leader,
                trigger: "s".into(),
                action: "queue.capture".into(),
                args: serde_json::json!({"duplicates": "collapse"}),
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
    fn malformed_arguments_keep_the_form_open_with_the_reason() {
        let mut app = bindings_app();
        app.on_key(key(KeyCode::Char('a')));
        assert_eq!(app.draft.as_ref().unwrap().field, DraftField::Trigger);
        type_text(&mut app, "z");
        app.on_key(key(KeyCode::Tab));
        type_text(&mut app, "stack.start");
        app.on_key(key(KeyCode::Tab));
        type_text(&mut app, "{not json");

        let requests = app.on_key(key(KeyCode::Enter));

        assert!(requests.is_empty());
        assert_eq!(app.input_mode, InputMode::Editing, "the typing must not be thrown away");
        let error = app.draft.as_ref().unwrap().error.clone().unwrap();
        assert!(error.contains("valid JSON"), "{error}");
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
