//! The Shortcuts screen: the leader key, the add/edit form and the list of
//! what is configured. Every control is one daemon action (`bind.leader`,
//! `bind.set`, `bind.remove`, `bind.list`), as in main.js.

use std::collections::BTreeMap;

use copycat_protocol::{
    Action, ArgKind, ArgSpec, Binding, BindingKind, RejectedBinding, ResultBody, action_spec, normalize_trigger,
};
use gpui_kit::component::button::{Button, ButtonGroup, ButtonVariants as _};
use gpui_kit::component::form::{field, v_form};
use gpui_kit::component::input::{Input, InputEvent, InputState, NumberInput};
use gpui_kit::component::kbd::Kbd;
use gpui_kit::component::select::{SearchableVec, Select, SelectEvent, SelectGroup, SelectItem, SelectState};
use gpui_kit::component::switch::Switch;
use gpui_kit::component::tag::Tag;
use gpui_kit::component::{ActiveTheme as _, IndexPath, Selectable as _, Sizable as _, h_flex, v_flex};
use gpui_kit::prelude::*;
use gpui_kit::*;

use crate::app::{AppView, Screen, request};
use crate::brand::*;
use crate::brand::px;
use crate::screens::empty;

/// Every bindable action in plain words, grouped as the picker shows them:
/// group, action, `ui.show` target, label. A test keeps it in step with
/// [`copycat_protocol::BINDABLE_ACTIONS`].
const ACTIONS: &[(&str, &str, Option<&str>, &str)] = &[
    ("Paste", "paste.mode", None, "Paste next from the session"),
    ("Paste", "paste.latest", None, "Paste latest copy"),
    ("Paste", "paste.offset", None, "Paste an older copy"),
    ("Paste", "paste.transform", None, "Paste with formatting changed"),
    ("Paste", "group.paste_last", None, "Paste recent copies together"),
    ("Paste", "paste.favourite", None, "Paste a favourite"),
    ("Sessions", "paste.next", None, "Paste next session item"),
    ("Sessions", "stack.start", None, "Start a stack (newest first)"),
    ("Sessions", "queue.start", None, "Start a queue from recent copies"),
    ("Sessions", "queue.capture", None, "Start collecting a queue"),
    ("Sessions", "queue.seal", None, "Finish collecting the queue"),
    ("Sessions", "group.capture", None, "Start collecting a group"),
    ("Sessions", "group.paste", None, "Paste the group"),
    ("Sessions", "session.stop", None, "End the session"),
    ("Sessions", "session.reset", None, "Restart the session from the top"),
    ("History", "history.toggle_pause", None, "Pause or resume history"),
    ("History", "history.pause", None, "Pause history"),
    ("History", "history.resume", None, "Resume history"),
    ("History", "latest.pin", None, "Pin or unpin latest copy"),
    ("History", "favourite.assign", None, "Save the next copy as a favourite"),
    ("History", "latest.delete", None, "Delete latest copy"),
    ("History", "history.clear", None, "Clear history"),
    ("Copycat", "ui.show", Some("window"), "Open Copycat"),
    ("Copycat", "ui.show", Some("history"), "Open History"),
    ("Copycat", "ui.show", Some("bindings"), "Open Shortcuts"),
    ("Copycat", "ui.show", Some("settings"), "Open Settings"),
    ("Copycat", "ui.show", Some("picker"), "Show or hide the menu-bar list"),
    ("Copycat", "ui.show", Some("floating"), "Show or hide the floating button"),
    ("Copycat", "ui.show", Some("overlay"), "Show or hide the live activity panel"),
    ("Copycat", "ui.show", Some("summon"), "Bring the floating button here"),
    ("Copycat", "ui.show", Some("sleep"), "Put the cat to sleep"),
];

/// An action and, for `ui.show`, its target.
type ActionId = (&'static str, Option<&'static str>);

/// The [`ACTIONS`] entry for a configured action; a `ui.show` target this
/// app doesn't know falls back to opening the window.
fn action_entry(action: &str, target: Option<&str>) -> Option<&'static (&'static str, &'static str, Option<&'static str>, &'static str)> {
    let mut named = ACTIONS.iter().filter(|a| a.1 == action);
    ACTIONS.iter().find(|a| a.1 == action && a.2 == target).or_else(|| named.next())
}

fn action_label(b: &Binding) -> String {
    let target = b.args.get("target").and_then(|t| t.as_str());
    action_entry(&b.action, target).map_or_else(|| b.action.clone(), |a| a.3.to_string())
}

#[derive(Clone)]
struct ActionChoice {
    id: ActionId,
    label: &'static str,
}

impl SelectItem for ActionChoice {
    type Value = ActionId;

    fn title(&self) -> SharedString {
        self.label.into()
    }

    fn value(&self) -> &ActionId {
        &self.id
    }

    /// The daemon's name finds it too.
    fn matches(&self, query: &str) -> bool {
        let query = query.to_lowercase();
        self.label.to_lowercase().contains(&query) || self.id.0.contains(&query)
    }
}

type Actions = SearchableVec<SelectGroup<ActionChoice>>;

fn action_items() -> Actions {
    let mut groups: Vec<SelectGroup<ActionChoice>> = Vec::new();
    for &(group, name, target, label) in ACTIONS {
        if groups.last().is_none_or(|g| g.title.as_ref() != group) {
            groups.push(SelectGroup::new(group));
        }
        if let Some(g) = groups.last_mut() {
            g.items.push(ActionChoice { id: (name, target), label });
        }
    }
    SearchableVec::new(groups)
}

/// An argument's field label and help, or `None` for one the form leaves as
/// loaded: `ui.show`'s target is part of the action, and `inject: false`
/// would make the key do nothing.
fn arg_label(action: &str, arg: &str) -> Option<(&'static str, Option<&'static str>)> {
    Some(match (action, arg) {
        (_, "duplicates") => ("Repeated copies", None),
        ("paste.offset", "offset") => ("How far back", Some("1 is the copy before the latest")),
        (_, "offset") => ("Which copy", Some("0 is the latest")),
        (_, "transform") => ("Change to", None),
        (_, "last") => ("How many recent copies", None),
        (_, "delimiter") => ("Separator", None),
        (_, "raw") => ("Count repeated copies separately", None),
        (_, "peek") => ("Keep it in the session (don't advance)", None),
        (_, "keep_pinned") => ("Keep pinned copies", None),
        (_, "keys") => ("Favourite's letters", Some("Leave empty to type them after the shortcut, like a or ab")),
        _ => return None,
    })
}

/// A choice of an enum argument in plain words.
fn value_label(value: &str) -> Option<&'static str> {
    Some(match value {
        "collapse" => "Skip repeats",
        "preserve" => "Keep repeats",
        "plain" => "Plain text",
        "trim" => "Trim spaces",
        "upper" => "UPPERCASE",
        "lower" => "lowercase",
        "join_lines" => "One line",
        _ => return None,
    })
}

/// A binding's arguments as words for its row: `3 copies`, `Keep repeats`.
/// Defaults and what the form doesn't show are left out.
fn arg_words(b: &Binding) -> Vec<String> {
    let Some(spec) = action_spec(&b.action) else { return Vec::new() };
    spec.args
        .iter()
        .filter_map(|arg| {
            let (label, _) = arg_label(&b.action, arg.name)?;
            let value = b.args.get(arg.name)?;
            match arg.kind {
                ArgKind::Enum(_) => value.as_str().map(|v| value_label(v).unwrap_or(v).to_string()),
                ArgKind::Int if arg.name == "last" => value.as_u64().map(|n| format!("{n} copies")),
                ArgKind::Int => value.as_u64().filter(|n| *n > 0).map(|n| format!("{n} back")),
                ArgKind::Bool => (value == &serde_json::Value::Bool(true)).then(|| label.to_string()),
                ArgKind::Text => value.as_str().map(|v| format!("separated by “{v}”")),
            }
        })
        .collect()
}

/// A pressed key as the daemon spells a chord (`ctrl+alt+v`), or `None`
/// when it needs ⌃, ⌥ or ⌘ first. F-keys may stand alone. Built by hand, not
/// from `Keystroke::unparse`, which joins with `-` and so breaks on that key.
fn chord(stroke: &Keystroke) -> Option<String> {
    let m = &stroke.modifiers;
    let fkey = stroke.key.strip_prefix('f').is_some_and(|n| n.parse::<u8>().is_ok());
    if !(m.control || m.alt || m.platform || fkey) {
        return None;
    }
    let mods = [(m.control, "ctrl"), (m.alt, "alt"), (m.platform, "cmd"), (m.shift, "shift")];
    let mut parts: Vec<&str> = mods.iter().filter(|(on, _)| *on).map(|(_, name)| *name).collect();
    parts.push(&stroke.key);
    Some(parts.join("+"))
}

/// A daemon chord in any spelling as the keystroke a [`Kbd`] draws, or
/// `None` when it isn't one (a refused `<leader> <sequence>`).
fn keystroke(chord: &str) -> Option<Keystroke> {
    if chord.trim().is_empty() || chord.contains(' ') {
        return None;
    }
    let normalized = normalize_trigger(chord);
    let gpui: Vec<&str> = normalized
        .split('+')
        .map(|token| match token {
            "super" => "cmd",
            "cmdorctrl" => "secondary",
            other => other,
        })
        .collect();
    Keystroke::parse(&gpui.join("-").to_lowercase()).ok()
}

/// The leader sequence's problem, as `check_sequence` in the daemon sees it.
fn sequence_problem(sequence: &str) -> Option<&'static str> {
    sequence.chars().any(|c| !c.is_ascii_graphic()).then_some("Use letters, digits and punctuation, with no spaces.")
}

/// Whether the wait for a longer sequence applies: one sequence starts
/// another (`s` and `ss`), or a favourite's letters are typed after one.
fn uses_wait(sequences: &[Binding]) -> bool {
    let triggers = || sequences.iter().map(|b| b.trigger.as_str());
    let favourite = |b: &Binding| {
        ["paste.favourite", "favourite.assign"].contains(&b.action.as_str())
            && b.args.get("keys").and_then(|k| k.as_str()).is_none_or(str::is_empty)
    };
    sequences.iter().any(favourite) || triggers().any(|a| triggers().any(|b| b.len() > a.len() && b.starts_with(a)))
}

const TIMEOUTS: [(u64, &str); 4] = [(2000, "2 s"), (5000, "5 s"), (10_000, "10 s"), (30_000, "30 s")];
const WAITS: [(u64, &str); 3] = [(250, "Short"), (400, "Normal"), (800, "Long")];
const WAIT_HELP: &str = "Used when one sequence starts another, like s and ss, and to end a favourite's letters.";

/// A delay none of the presets is, from the CLI or the config file. Zero is
/// a daemon too old to say.
fn custom(ms: u64, presets: &[(u64, &str)]) -> Option<String> {
    (ms != 0 && presets.iter().all(|(p, _)| *p != ms)).then(|| format!("Custom: {} s", ms as f64 / 1000.0))
}

/// What a key press is being recorded for.
#[derive(Clone, Copy, PartialEq)]
enum Record {
    Leader,
    Hotkey,
}

/// A `bind.list` reply.
#[derive(Default)]
pub struct BindingsData {
    pub sequences: Vec<Binding>,
    pub hotkeys: Vec<Binding>,
    pub tui: Vec<Binding>,
    pub rejected: Vec<RejectedBinding>,
}

impl BindingsData {
    /// The rail badge's number.
    pub fn count(&self) -> usize {
        self.sequences.len() + self.hotkeys.len()
    }
}

/// An edit to a text field that waits for the next render: the reply that asks
/// for it has no window, and setting a field's text needs one.
pub enum FormSync {
    Reset,
}

pub struct BindForm {
    /// The leader chord as last known. The daemon stops saying it once the
    /// leader is off, so it is kept to show and to turn back on.
    leader: Option<String>,
    armed: bool,
    timeout_ms: u64,
    wait_ms: u64,
    /// The press being recorded, and the interceptor reading it.
    recording: Option<(Record, Subscription)>,
    /// The last press had no ⌃, ⌥ or ⌘.
    bare: bool,
    kind: BindingKind,
    /// The recorded shortcut, as the daemon spells it.
    chord: String,
    /// The letters typed after the leader.
    keys: Entity<InputState>,
    action: Entity<SelectState<Actions>>,
    /// Enum and bool arguments as picked; a key left out is the default.
    arg_values: BTreeMap<&'static str, serde_json::Value>,
    /// No action has more than one whole-number or one text argument.
    number: Entity<InputState>,
    text: Entity<InputState>,
    /// The binding being edited, as configured, so a failed edit can put it
    /// back.
    editing: Option<(BindingKind, Binding)>,
    pub data: Option<BindingsData>,
    pub sync: Vec<FormSync>,
}

impl BindForm {
    pub fn new(window: &mut Window, cx: &mut Context<AppView>) -> (Self, Vec<Subscription>) {
        let action =
            cx.new(|cx| SelectState::new(action_items(), Some(IndexPath::default()), window, cx).searchable(true));
        let keys = cx.new(|cx| InputState::new(window, cx).placeholder("gp"));
        let form = BindForm {
            leader: None,
            armed: false,
            timeout_ms: 0,
            wait_ms: 0,
            recording: None,
            bare: false,
            kind: BindingKind::Hotkey,
            chord: String::new(),
            keys: keys.clone(),
            action: action.clone(),
            arg_values: BTreeMap::new(),
            number: cx.new(|cx| InputState::new(window, cx).step(1.).min(0.)),
            text: cx.new(|cx| InputState::new(window, cx).placeholder("new line")),
            editing: None,
            data: None,
            sync: Vec::new(),
        };
        let subs = vec![
            // The argument fields follow the action.
            cx.subscribe(&action, |_, _, _: &SelectEvent<Actions>, cx| cx.notify()),
            // The key caps beside the letters follow them.
            cx.subscribe(&keys, |_, _, _: &InputEvent, cx| cx.notify()),
        ];
        (form, subs)
    }

    fn action(&self, cx: &App) -> Option<ActionId> {
        self.action.read(cx).selected_value().copied()
    }

    /// An enum's pick, or a required one's first value.
    fn enum_value(&self, arg: &ArgSpec) -> Option<&'static str> {
        let ArgKind::Enum(values) = arg.kind else { return None };
        let picked = self.arg_values.get(arg.name).and_then(|v| v.as_str());
        values.iter().copied().find(|v| Some(*v) == picked).or(values.first().copied().filter(|_| arg.required))
    }

    /// The arguments to send: the fields, over whatever the form doesn't
    /// show from the binding being edited (a hand-edited key, `inject`).
    fn args(&self, (name, target): ActionId, cx: &App) -> Result<serde_json::Value, &'static str> {
        let mut args = match &self.editing {
            Some((_, b)) if b.action == name => b.args.as_object().cloned().unwrap_or_default(),
            _ => serde_json::Map::new(),
        };
        for arg in action_spec(name).map_or(&[][..], |spec| spec.args) {
            if arg_label(name, arg.name).is_none() {
                continue;
            }
            args.remove(arg.name);
            let value = match arg.kind {
                ArgKind::Enum(_) => self.enum_value(arg).map(serde_json::Value::from),
                ArgKind::Bool => self.arg_values.get(arg.name).cloned(),
                ArgKind::Int => {
                    let text = self.number.read(cx).value().trim().to_string();
                    let missing = if arg.name == "last" { "Enter how many copies" } else { "Enter how far back" };
                    match text.parse::<u64>() {
                        _ if text.is_empty() && !arg.required => None,
                        Ok(n) if n > 0 || arg.name != "last" => Some(n.into()),
                        _ => return Err(missing),
                    }
                }
                ArgKind::Text => Some(self.text.read(cx).value().to_string()).filter(|t| !t.is_empty()).map(Into::into),
            };
            if let Some(value) = value {
                args.insert(arg.name.into(), value);
            }
        }
        if let Some(target) = target {
            args.insert("target".into(), target.into());
        }
        Ok(if args.is_empty() { serde_json::Value::Null } else { args.into() })
    }

    fn reset(&mut self, window: &mut Window, cx: &mut Context<AppView>) {
        self.editing = None;
        self.recording = None;
        self.chord.clear();
        self.arg_values.clear();
        for input in [&self.keys, &self.number, &self.text] {
            input.update(cx, |s, cx| s.set_value("", window, cx));
        }
    }

    /// Put a configured binding into the form.
    fn fill(&mut self, kind: BindingKind, b: &Binding, window: &mut Window, cx: &mut Context<AppView>) {
        self.reset(window, cx);
        self.editing = Some((kind, b.clone()));
        self.kind = kind;
        if kind == BindingKind::Hotkey {
            self.chord = b.trigger.clone();
        } else {
            self.keys.update(cx, |s, cx| s.set_value(b.trigger.as_str(), window, cx));
        }
        let target = b.args.get("target").and_then(|t| t.as_str());
        if let Some(&(_, name, target, _)) = action_entry(&b.action, target) {
            self.action.update(cx, |s, cx| s.set_selected_value(&(name, target), window, cx));
        }
        for arg in action_spec(&b.action).map_or(&[][..], |spec| spec.args) {
            let Some(value) = b.args.get(arg.name) else { continue };
            match arg.kind {
                ArgKind::Enum(_) | ArgKind::Bool => {
                    self.arg_values.insert(arg.name, value.clone());
                }
                ArgKind::Int => self.number.update(cx, |s, cx| s.set_value(value.to_string(), window, cx)),
                ArgKind::Text => {
                    let text = value.as_str().unwrap_or_default().to_string();
                    self.text.update(cx, |s, cx| s.set_value(text, window, cx));
                }
            }
        }
    }
}

/// What a binding is removed by: the trigger, or for a TUI key the action.
fn identity(kind: BindingKind, trigger: &str, action: &str) -> String {
    (if kind == BindingKind::Tui { action } else { trigger }).to_string()
}

impl AppView {
    /// Apply what a reply left for the form (see [`FormSync`]). Called from render.
    pub fn sync_form(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        for sync in std::mem::take(&mut self.form.sync) {
            match sync {
                FormSync::Reset => self.form.reset(window, cx),
            }
        }
    }

    pub fn load_bindings(&mut self, cx: &mut Context<Self>) {
        self.send(Action::BindList, cx, |this, body, _| this.apply_bindings(body));
    }

    fn apply_bindings(&mut self, body: ResultBody) {
        if let ResultBody::Bindings { leader, leader_timeout_ms, leader_wait_ms, sequences, hotkeys, tui, rejected } = body {
            self.form.armed = leader.is_some();
            if leader.is_some() {
                self.form.leader = leader;
            }
            self.form.timeout_ms = leader_timeout_ms;
            self.form.wait_ms = leader_wait_ms;
            self.form.data = Some(BindingsData { sequences, hotkeys, tui, rejected });
        }
    }

    /// One leader change, saved at once; the reply is the new `bind.list`.
    fn set_leader(&mut self, trigger: Option<String>, enabled: Option<bool>, timeout_ms: Option<u64>, wait_ms: Option<u64>, cx: &mut Context<Self>) {
        self.send(Action::BindLeader { trigger, enabled, timeout_ms, wait_ms }, cx, |this, body, cx| {
            this.toast("Leader key updated", cx);
            this.apply_bindings(body);
        });
    }

    /// Record the next press. An interceptor, not a key listener: it runs
    /// before the app's own bindings, so ⌘W or Esc are recorded, not obeyed.
    /// Pressing the leader's current chord fires the leader instead: the
    /// daemon's tap sees it first. ponytail: switch the leader off while
    /// recording if re-recording the same chord ever matters.
    fn record(&mut self, target: Record, window: &mut Window, cx: &mut Context<Self>) {
        window.blur(cx);
        let view = cx.weak_entity();
        let interceptor = cx.intercept_keystrokes(move |event, _, cx| {
            let stroke = &event.keystroke;
            if matches!(stroke.key.as_str(), "shift" | "control" | "alt" | "platform" | "function") {
                return;
            }
            if view.update(cx, |this, cx| this.recorded(stroke, cx)).unwrap_or(false) {
                cx.stop_propagation();
            }
        });
        self.form.recording = Some((target, interceptor));
        self.form.bare = false;
        cx.notify();
    }

    /// Whether the press was taken. Leaving the screen stops recording, so
    /// typing elsewhere isn't swallowed.
    fn recorded(&mut self, stroke: &Keystroke, cx: &mut Context<Self>) -> bool {
        let Some((target, _)) = &self.form.recording else { return false };
        let target = *target;
        cx.notify();
        if self.screen != Screen::Bindings {
            self.form.recording = None;
            return false;
        }
        if stroke.key == "escape" && !stroke.modifiers.modified() {
            self.form.recording = None;
            return true;
        }
        let Some(chord) = chord(stroke) else {
            self.form.bare = true;
            return true;
        };
        self.form.recording = None;
        if let Some(used) = self.used_for(&chord, target) {
            self.toast(format!("{} is already used for {used}", shown(&chord)), cx);
        }
        match target {
            Record::Hotkey => self.form.chord = chord,
            Record::Leader => self.set_leader(Some(chord), None, None, None, cx),
        }
        true
    }

    /// What else a chord already does, if anything.
    fn used_for(&self, chord: &str, target: Record) -> Option<String> {
        let stroke = keystroke(chord)?;
        let same = |other: &str| keystroke(other).as_ref() == Some(&stroke);
        if target == Record::Hotkey && self.form.leader.as_deref().is_some_and(same) {
            return Some("the leader key".into());
        }
        let editing = self.form.editing.as_ref().map(|(_, b)| b.trigger.as_str());
        let hotkeys = self.form.data.as_ref().map_or(&[][..], |d| &d.hotkeys);
        hotkeys.iter().find(|b| same(&b.trigger) && Some(b.trigger.as_str()) != editing).map(action_label)
    }

    fn save_binding(&mut self, cx: &mut Context<Self>) {
        let kind = self.form.kind;
        let trigger = match kind {
            BindingKind::Hotkey => self.form.chord.clone(),
            _ => self.form.keys.read(cx).value().trim().to_string(),
        };
        if trigger.is_empty() {
            let missing = if kind == BindingKind::Hotkey { "Record a shortcut first" } else { "Type the letters to press after the leader" };
            return self.toast(missing, cx);
        }
        if let Some(problem) = (kind == BindingKind::Leader).then(|| sequence_problem(&trigger)).flatten() {
            return self.toast(problem, cx);
        }
        let Some(id) = self.form.action(cx) else { return self.toast("Choose what it does", cx) };
        let args = match self.form.args(id, cx) {
            Ok(args) => args,
            Err(missing) => return self.toast(missing, cx),
        };
        let action = id.0.to_string();
        fn saved(this: &mut AppView, _: ResultBody, cx: &mut Context<AppView>) {
            this.toast("Shortcut saved", cx);
            this.form.sync.push(FormSync::Reset);
            this.load_bindings(cx);
        }
        let new_identity = identity(kind, &trigger, &action);
        let set = Action::BindSet { kind, trigger, action, args };
        // A renamed trigger removes the old entry first, so an edit can't
        // leave two. If the new one is then refused, the old goes back.
        match self.form.editing.clone() {
            Some((old_kind, old)) if old_kind != kind || identity(old_kind, &old.trigger, &old.action) != new_identity => {
                let remove = Action::BindRemove { kind: old_kind, trigger: identity(old_kind, &old.trigger, &old.action) };
                let restore = Action::BindSet { kind: old_kind, trigger: old.trigger, action: old.action, args: old.args };
                self.send(remove, cx, move |_, _, cx| {
                    request(set, cx, move |this, reply, cx| match reply {
                        Ok(body) => saved(this, body, cx),
                        Err(error) => {
                            this.toast_error(error.message, cx);
                            request(restore, cx, |this, reply, cx| {
                                if let Err(error) = reply {
                                    this.toast_error(error.message, cx);
                                }
                                this.load_bindings(cx);
                            });
                        }
                    })
                });
            }
            _ => self.send(set, cx, saved),
        }
    }

    pub fn reload_config(&mut self, cx: &mut Context<Self>) {
        self.send(Action::BindReload, cx, |this, _, cx| {
            this.toast("Settings reloaded", cx);
            this.load_config(cx);
            this.load_bindings(cx);
        });
    }

    /// The config file's path, for Settings → Advanced to open.
    pub fn load_config(&mut self, cx: &mut Context<Self>) {
        self.send(Action::ConfigShow, cx, |this, body, _| {
            if let ResultBody::Config { path, .. } = body {
                this.config_path = Some(path);
            }
        });
    }

    /// A shortcut's key caps and its Change button, or while recording, what
    /// to do.
    fn recorder(&self, target: Record, chord: Option<&str>, cx: &Context<Self>) -> Div {
        let id = if target == Record::Leader { "rec-leader" } else { "rec-hotkey" };
        if self.form.recording.as_ref().is_some_and(|(t, _)| *t == target) {
            let (prompt, color) = if self.form.bare {
                ("Add ⌃, ⌥ or ⌘ to the key", cx.theme().danger)
            } else {
                ("Press the new shortcut… (Esc to cancel)", cx.theme().foreground)
            };
            return h_flex().gap(px(10.)).child(div().text_color(color).child(prompt)).child(
                ghost(id, "Cancel").small().on_click(cx.listener(|this, _, _, cx| {
                    this.form.recording = None;
                    cx.notify();
                })),
            );
        }
        h_flex()
            .gap(px(10.))
            .child(match chord {
                Some(chord) => chord_chip(chord),
                // The daemon only leaves the leader's chord out while it is off.
                None => div()
                    .text_color(cx.theme().muted_foreground)
                    .child(if target == Record::Leader { "Off" } else { "Not set" })
                    .into_any_element(),
            })
            .child(ghost(id, if chord.is_some() { "Change" } else { "Record" }).small().on_click(cx.listener(
                move |this, _, window, cx| this.record(target, window, cx),
            )))
    }

    pub fn bindings_screen(&self, cx: &Context<Self>) -> impl IntoElement {
        let form = &self.form;
        let editing = form.editing.is_some();
        let leader = form.leader.as_deref();
        let waits = form.data.as_ref().is_some_and(|d| uses_wait(&d.sequences));
        let keys = form.keys.read(cx).value().to_string();
        let selected = form.action(cx);

        let leader_section = section("Leader key").child(
            v_form()
                .columns(2)
                .child(
                    field()
                        .label("Leader key")
                        .description("Press the leader key, then type a few letters to run a shortcut.")
                        .child(
                            h_flex().gap(px(14.)).child(self.recorder(Record::Leader, leader, cx)).child(
                                Switch::new("leader-on").label("On").checked(form.armed).on_click(cx.listener(
                                    |this, on: &bool, _, cx| {
                                        this.form.armed = *on;
                                        this.set_leader(None, Some(*on), None, None, cx);
                                    },
                                )),
                            ),
                        ),
                )
                .child(
                    field()
                        .label("Time to type each letter")
                        .when_some(custom(form.timeout_ms, &TIMEOUTS), |field, custom| field.description(custom))
                        .child(segments("leader-timeout", &TIMEOUTS, Some(form.timeout_ms), cx, |this, ms, cx| {
                            this.form.timeout_ms = ms;
                            this.set_leader(None, None, Some(ms), None, cx);
                        })),
                )
                .child(
                    field()
                        .label("Pause before a short sequence runs")
                        .description(match custom(form.wait_ms, &WAITS) {
                            Some(custom) => format!("{custom}. {WAIT_HELP}"),
                            None => WAIT_HELP.into(),
                        })
                        .visible(waits)
                        .child(segments("leader-wait", &WAITS, Some(form.wait_ms), cx, |this, ms, cx| {
                            this.form.wait_ms = ms;
                            this.set_leader(None, None, None, Some(ms), cx);
                        })),
                ),
        );

        let trigger = if form.kind == BindingKind::Hotkey {
            self.recorder(Record::Hotkey, (!form.chord.is_empty()).then_some(form.chord.as_str()), cx)
        } else {
            h_flex()
                .gap(px(10.))
                .child(match leader {
                    Some(chord) => chord_chip(chord),
                    None => div().child("The leader key").into_any_element(),
                })
                .child(div().text_color(cx.theme().muted_foreground).child("then type"))
                .child(div().w(px(90.)).flex_none().child(Input::new(&form.keys).small()))
                .children(keys.chars().filter(char::is_ascii_graphic).map(letter_chip))
        };
        let trigger_help = (form.kind == BindingKind::Leader).then(|| match sequence_problem(&keys) {
            Some(problem) => div().text_color(cx.theme().danger).child(problem),
            None => help("Letters and digits, typed one after another. Upper and lower case are different keys."),
        });

        let arg_fields = selected.and_then(|(name, _)| action_spec(name)).map_or(&[][..], |spec| spec.args).iter().filter_map(
            |arg| {
                let (name, _) = selected?;
                let (label, hint) = arg_label(name, arg.name)?;
                let control = match arg.kind {
                    ArgKind::Enum(values) => {
                        let options: Vec<(Option<&'static str>, &'static str)> = (!arg.required)
                            .then_some((None, "Default"))
                            .into_iter()
                            .chain(values.iter().map(|v| (Some(*v), value_label(v).unwrap_or(v))))
                            .collect();
                        let arg_name = arg.name;
                        segments(arg.name, &options, Some(form.enum_value(arg)), cx, move |this, value, _| {
                            match value {
                                Some(value) => this.form.arg_values.insert(arg_name, value.into()),
                                None => this.form.arg_values.remove(arg_name),
                            };
                        })
                        .into_any_element()
                    }
                    ArgKind::Bool => {
                        let arg_name = arg.name;
                        let on = form.arg_values.get(arg.name) == Some(&serde_json::Value::Bool(true));
                        Switch::new(arg.name)
                            .label(label)
                            .checked(on)
                            .on_click(cx.listener(move |this, on: &bool, _, cx| {
                                match on {
                                    true => this.form.arg_values.insert(arg_name, true.into()),
                                    false => this.form.arg_values.remove(arg_name),
                                };
                                cx.notify();
                            }))
                            .into_any_element()
                    }
                    ArgKind::Int => div().w(px(140.)).child(NumberInput::new(&form.number).small()).into_any_element(),
                    ArgKind::Text => div().w(px(140.)).child(Input::new(&form.text).small()).into_any_element(),
                };
                Some(
                    field()
                        .col_span(if matches!(arg.kind, ArgKind::Enum(_)) { 2 } else { 1 })
                        // A switch carries its own label.
                        .when(arg.kind != ArgKind::Bool, |field| field.label(label))
                        .when_some(hint, |field, hint| field.description(hint))
                        .child(control),
                )
            },
        );

        v_flex()
            .gap(px(16.))
            .child(heading("Shortcuts", "Keyboard shortcuts that work anywhere on your Mac."))
            .child(leader_section)
            .child(
                section(if editing { "Edit shortcut" } else { "New shortcut" })
                    .child(
                        v_form()
                            .columns(2)
                            .child(
                                field().label("When I press").col_span(2).child(
                                    v_flex()
                                        .gap(px(10.))
                                        .child(segments(
                                            "b-kind",
                                            &[(BindingKind::Hotkey, "A shortcut, anywhere"), (BindingKind::Leader, "The leader key, then letters")],
                                            Some(form.kind),
                                            cx,
                                            |this, kind, _| {
                                                this.form.kind = kind;
                                                this.form.recording = None;
                                            },
                                        ))
                                        .child(trigger)
                                        .children(trigger_help),
                                ),
                            )
                            .child(field().label("Do this").col_span(2).child(Select::new(&form.action)))
                            .children(arg_fields),
                    )
                    .child(
                        h_flex()
                            .gap(px(8.))
                            .mt(px(12.))
                            .child(
                                primary("b-save", if editing { "Save" } else { "Add" })
                                    .icon(if editing { IconName::Save } else { IconName::Plus })
                                    .on_click(cx.listener(|this, _, _, cx| this.save_binding(cx))),
                            )
                            .when(editing, |this| {
                                this.child(
                                    ghost("b-cancel", "Cancel edit").icon(IconName::Undo2).on_click(cx.listener(
                                        |this, _, _, cx| {
                                            this.form.sync.push(FormSync::Reset);
                                            this.form.editing = None;
                                            cx.notify();
                                        },
                                    )),
                                )
                            }),
                    ),
            )
            .child(section("Your shortcuts").child(self.configured(cx)))
    }

    /// The `#bindings-list`: groups of rows with Edit / Remove, then what the
    /// platform refused.
    fn configured(&self, cx: &Context<Self>) -> impl IntoElement {
        let Some(data) = &self.form.data else {
            return v_flex().child(empty("Loading…"));
        };
        let leader = self.form.leader.as_deref();
        let group = |title: &'static str, items: &[Binding], kind: BindingKind, tag: &'static str| {
            (!items.is_empty()).then(|| {
                v_flex().gap(px(6.)).child(subtitle(title, cx)).children(items.iter().enumerate().map(|(ix, b)| {
                    let (edit, remove) = (b.clone(), (kind, identity(kind, &b.trigger, &b.action)));
                    let keys = match kind {
                        BindingKind::Hotkey => chord_chip(&b.trigger),
                        BindingKind::Leader => h_flex()
                            .gap(px(3.))
                            .child(leader.map_or_else(|| bold("Leader"), chord_chip))
                            .children(b.trigger.chars().map(letter_chip))
                            .into_any_element(),
                        BindingKind::Tui => bold(&b.trigger),
                    };
                    let (what, words) = match kind {
                        BindingKind::Tui => (b.action.replace('_', " "), Vec::new()),
                        _ => (action_label(b), arg_words(b)),
                    };
                    row(
                        h_flex()
                            .gap(px(8.))
                            .child(keys)
                            .child(div().child(what))
                            .children(words.into_iter().map(|w| div().text_color(cx.theme().muted_foreground).child(format!("· {w}")))),
                        h_flex()
                            .gap(px(6.))
                            .when(kind != BindingKind::Tui, |this| {
                                this.child(icon_button((tag, ix), IconName::Pencil).tooltip("Edit").on_click(
                                    cx.listener(move |this, _, window, cx| {
                                        this.form.fill(kind, &edit, window, cx);
                                        cx.notify();
                                    }),
                                ))
                            })
                            .child(
                                icon_button((tag, ix + 1000), IconName::Trash)
                                    .text_color(cx.theme().danger)
                                    .tooltip("Remove")
                                    .on_click(cx.listener(move |this, _, _, cx| {
                                        let (kind, trigger) = remove.clone();
                                        this.send(Action::BindRemove { kind, trigger }, cx, |this, _, cx| {
                                            this.load_bindings(cx)
                                        });
                                    })),
                            ),
                        cx,
                    )
                }))
            })
        };
        let rejected = (!data.rejected.is_empty()).then(|| {
            v_flex().gap(px(6.)).child(subtitle("Not working", cx)).children(data.rejected.iter().map(|r| {
                row(
                    h_flex()
                        .gap(px(8.))
                        .child(chord_chip(&r.trigger))
                        .child(div().min_w_0().truncate().text_color(cx.theme().muted_foreground).child(r.reason.clone())),
                    Tag::warning().outline().small().child("Not working"),
                    cx,
                )
            }))
        });
        let groups = [
            group("Leader key + letters", &data.sequences, BindingKind::Leader, "leader"),
            group("Shortcuts", &data.hotkeys, BindingKind::Hotkey, "hotkey"),
            group("Terminal app keys", &data.tui, BindingKind::Tui, "tui"),
        ];
        let none = groups.iter().all(Option::is_none) && rejected.is_none();
        v_flex()
            .gap(px(12.))
            .children(groups.into_iter().flatten())
            .children(rejected)
            .when(none, |this| this.child(empty("No shortcuts yet.")))
    }
}

/// One of a few, as Settings draws it. Each button has its own click: the
/// group's callback misses keyboard presses.
fn segments<T: Copy + PartialEq + 'static>(
    id: &'static str,
    options: &[(T, &'static str)],
    now: Option<T>,
    cx: &Context<AppView>,
    pick: impl Fn(&mut AppView, T, &mut Context<AppView>) + Copy + 'static,
) -> ButtonGroup {
    ButtonGroup::new(id).outline().small().children(options.iter().enumerate().map(|(ix, &(value, label))| {
        let on = now == Some(value);
        Button::new((id, ix)).label(label).selected(on).when(on, |b| b.primary()).on_click(cx.listener(
            move |this, _, _, cx| {
                pick(this, value, cx);
                cx.notify();
            },
        ))
    }))
}

fn subtitle(text: &'static str, cx: &App) -> Div {
    div().text_color(cx.theme().muted_foreground).child(text)
}

fn bold(text: &str) -> AnyElement {
    div().font_weight(FontWeight::BOLD).child(text.to_string()).into_any_element()
}

/// A chord in any daemon spelling as key caps (⌃⌥V), else as plain text.
fn chord_chip(chord: &str) -> AnyElement {
    keystroke(chord).map_or_else(|| bold(chord), |stroke| Kbd::new(stroke).into_any_element())
}

/// A chord as a toast spells it: ⌃⌥V.
fn shown(chord: &str) -> String {
    keystroke(chord).map_or_else(|| chord.to_string(), |stroke| Kbd::format(&stroke))
}

/// One key of a leader sequence. A capital shows ⇧, since case matters.
fn letter_chip(c: char) -> AnyElement {
    let mut stroke = Keystroke { key: c.to_ascii_lowercase().to_string(), ..Default::default() };
    stroke.modifiers.shift = c.is_ascii_uppercase();
    Kbd::new(stroke).into_any_element()
}

/// Key caps and words on the left, tools on the right.
fn row(content: impl IntoElement, tools: impl IntoElement, cx: &App) -> Div {
    h_flex()
        .justify_between()
        .gap(px(12.))
        .px(px(12.))
        .py(px(7.))
        .rounded(cx.theme().radius)
        .border_1()
        .border_color(cx.theme().border)
        .bg(cx.theme().muted)
        .child(div().flex_1().min_w_0().overflow_hidden().child(content))
        .child(tools)
}

/// `.field-help`.
pub fn help(text: impl Into<SharedString>) -> Div {
    div().text_size(px(11.5)).line_height(relative(1.5)).text_color(rgb(pal().faint)).mt(px(5.)).child(text.into())
}

#[cfg(test)]
mod tests {
    use super::{
        ACTIONS, ArgKind, ArgSpec, Binding, Keystroke, Modifiers, TIMEOUTS, action_entry, action_label, action_spec,
        arg_label, arg_words, chord, custom, keystroke, sequence_problem, uses_wait, value_label,
    };
    use copycat_protocol::BINDABLE_ACTIONS;

    fn binding(trigger: &str, action: &str, args: serde_json::Value) -> Binding {
        Binding { trigger: trigger.into(), action: action.into(), args }
    }

    #[test]
    fn every_action_target_argument_and_choice_has_plain_words() {
        for spec in BINDABLE_ACTIONS {
            let targets: Vec<Option<&str>> = match spec.args.iter().find(|a| a.name == "target") {
                Some(ArgSpec { kind: ArgKind::Enum(values), .. }) => values.iter().map(|v| Some(*v)).collect(),
                _ => vec![None],
            };
            for target in targets {
                assert!(ACTIONS.iter().any(|a| (a.1, a.2) == (spec.name, target)), "{} {target:?} has no label", spec.name);
            }
            for arg in spec.args {
                let label = arg_label(spec.name, arg.name);
                assert_eq!(label.is_none(), ["target", "inject"].contains(&arg.name), "{}.{}", spec.name, arg.name);
                if let (Some(_), ArgKind::Enum(values)) = (label, arg.kind) {
                    assert!(values.iter().all(|v| value_label(v).is_some()), "{}.{}", spec.name, arg.name);
                }
            }
        }
        // Every entry names a real action.
        assert!(ACTIONS.iter().all(|a| action_spec(a.1).is_some()));
        // A ui.show target this app doesn't know opens the window.
        assert_eq!(action_entry("ui.show", Some("dock")).map(|a| a.3), Some("Open Copycat"));
    }

    #[test]
    fn a_press_becomes_a_daemon_chord_and_a_chord_becomes_key_caps() {
        let press = |key: &str, f: fn(&mut Modifiers)| {
            let mut stroke = Keystroke { key: key.into(), ..Default::default() };
            f(&mut stroke.modifiers);
            chord(&stroke)
        };
        assert_eq!(press("v", |m| (m.control, m.alt) = (true, true)).as_deref(), Some("ctrl+alt+v"));
        assert_eq!(press("-", |m| (m.platform, m.shift) = (true, true)).as_deref(), Some("cmd+shift+-"));
        assert_eq!(press("f5", |_| ()).as_deref(), Some("f5"));
        assert_eq!(press("v", |m| m.shift = true), None);
        assert_eq!(press("f", |_| ()), None);

        let caps = |chord| keystroke(chord).map(|k| (k.modifiers.control, k.modifiers.alt, k.modifiers.platform, k.key));
        assert_eq!(caps("ctrl+alt+v"), Some((true, true, false, "v".into())));
        assert_eq!(caps("cmd+shift+-"), Some((false, false, true, "-".into())));
        assert_eq!(caps("Super+K"), Some((false, false, true, "k".into())));
        assert_eq!(caps("ctrl+alt+space dd"), None);
    }

    #[test]
    fn rows_say_their_arguments_and_the_wait_shows_only_for_prefixes() {
        let offset = binding("ctrl+alt+v", "paste.offset", serde_json::json!({"offset": 2, "raw": false}));
        assert_eq!(arg_words(&offset), ["2 back"]);
        let queue = binding("q", "queue.start", serde_json::json!({"last": 3, "duplicates": "preserve"}));
        assert_eq!(arg_words(&queue), ["3 copies", "Keep repeats"]);
        let mode = binding("p", "paste.mode", serde_json::json!({"inject": false}));
        assert!(arg_words(&mode).is_empty());
        assert_eq!(action_label(&binding("z", "ui.show", serde_json::json!({"target": "sleep"}))), "Put the cat to sleep");

        let seqs = |triggers: &[&str]| triggers.iter().map(|t| binding(t, "queue.seal", serde_json::Value::Null)).collect::<Vec<_>>();
        assert!(uses_wait(&seqs(&["s", "ss"])));
        assert!(!uses_wait(&seqs(&["s", "gp", "dd"])));
        assert!(uses_wait(&[binding("f", "paste.favourite", serde_json::json!({}))]));
        assert!(!uses_wait(&[binding("f", "paste.favourite", serde_json::json!({"keys": "a"}))]));

        assert_eq!(custom(7000, &TIMEOUTS).as_deref(), Some("Custom: 7 s"));
        assert_eq!(custom(5000, &TIMEOUTS), None);
        assert_eq!(custom(0, &TIMEOUTS), None);
        assert_eq!(sequence_problem("g p"), Some("Use letters, digits and punctuation, with no spaces."));
        assert_eq!(sequence_problem("gP!"), None);
    }
}
