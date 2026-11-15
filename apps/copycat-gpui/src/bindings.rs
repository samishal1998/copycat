//! The Bindings screen: the leader chord, the add/edit form and the list of
//! what is configured. Every control is one daemon action (`bind.leader`,
//! `bind.set`, `bind.remove`, `bind.list`), as in main.js.

use copycat_protocol::{
    Action, ActionSpec, ArgKind, BINDABLE_ACTIONS, Binding, BindingKind, RejectedBinding, ResultBody, action_spec,
};
use gpui_kit::component::form::{field, v_form};
use gpui_kit::component::kbd::Kbd;
use gpui_kit::component::switch::Switch;
use gpui_kit::component::tag::Tag;
use gpui_kit::component::{ActiveTheme as _, Sizable as _};
use gpui_kit::component::input::{Input, InputState, Textarea, TextareaState};
use gpui_kit::component::select::{Select, SelectEvent, SelectState};
use gpui_kit::component::{IndexPath, h_flex, v_flex};
use gpui_kit::prelude::*;
use gpui_kit::*;

use crate::app::{AppView, request};
use crate::brand::*;
use crate::screens::empty;

const TUI_ACTIONS: [&str; 24] = [
    "confirm", "paste_next", "delete", "pin", "search", "toggle_raw", "add", "edit", "test", "toggle_pause",
    "stack_start", "queue_capture", "queue_seal", "group_capture", "group_paste", "session_stop",
    "session_reset", "next_tab", "prev_tab", "down", "up", "refresh", "help", "quit",
];
const KINDS: [(BindingKind, &str); 3] = [
    (BindingKind::Leader, "leader — keys after the leader"),
    (BindingKind::Hotkey, "hotkey — a system-wide chord"),
    (BindingKind::Tui, "tui — a key in this app"),
];

type Choice = Entity<SelectState<Vec<SharedString>>>;

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
    /// The chord, the per-key timeout and the wait for a longer sequence, as
    /// the fields show them.
    Leader(String, String, String),
    Reset,
}

pub struct BindForm {
    leader: Entity<InputState>,
    /// Seconds the leader waits for each key.
    timeout: Entity<InputState>,
    /// The timeout field as last loaded, so saving the leader only sends a
    /// timeout the user changed.
    timeout_loaded: String,
    /// Seconds a bound sequence that starts a longer one waits for the next
    /// key, and that field as last loaded.
    wait: Entity<InputState>,
    wait_loaded: String,
    armed: bool,
    kind: Choice,
    trigger: Entity<InputState>,
    action: Choice,
    args: Entity<TextareaState>,
    /// The binding being edited, as configured, so a failed edit can put it
    /// back.
    editing: Option<(BindingKind, Binding)>,
    pub data: Option<BindingsData>,
    pub sync: Vec<FormSync>,
}

fn choice(items: Vec<SharedString>, window: &mut Window, cx: &mut Context<AppView>) -> Choice {
    cx.new(|cx| SelectState::new(items, Some(IndexPath::new(0)), window, cx))
}

fn action_names(kind: BindingKind) -> Vec<SharedString> {
    match kind {
        BindingKind::Tui => TUI_ACTIONS.iter().map(|a| (*a).into()).collect(),
        _ => BINDABLE_ACTIONS.iter().map(|a| a.name.into()).collect(),
    }
}

/// The hint under the picker: what the action does and, if it has required
/// arguments, an example of them.
fn hint(spec: &ActionSpec) -> String {
    let needs: serde_json::Map<String, serde_json::Value> = spec
        .args
        .iter()
        .filter(|a| a.required)
        .map(|a| {
            let example = match a.kind {
                ArgKind::Enum(values) => values.first().copied().unwrap_or_default().into(),
                ArgKind::Int => 1.into(),
                ArgKind::Bool => true.into(),
                ArgKind::Text => "…".into(),
            };
            (a.name.to_string(), example)
        })
        .collect();
    if needs.is_empty() {
        spec.summary.to_string()
    } else {
        format!("{} — needs {}", spec.summary, serde_json::Value::Object(needs))
    }
}

/// A seconds field in milliseconds: `None` when empty or as `loaded` (keep
/// the daemon's), an error when outside `range`.
fn seconds_ms(text: &str, loaded: &str, range: std::ops::RangeInclusive<f64>) -> Result<Option<u64>, ()> {
    let text = text.trim();
    if text.is_empty() || text == loaded.trim() {
        return Ok(None);
    }
    match text.parse::<f64>() {
        Ok(secs) if range.contains(&secs) => Ok(Some((secs * 1000.0).round() as u64)),
        _ => Err(()),
    }
}

impl BindForm {
    pub fn new(window: &mut Window, cx: &mut Context<AppView>) -> (Self, Vec<Subscription>) {
        let kind = choice(KINDS.iter().map(|(_, label)| (*label).into()).collect(), window, cx);
        let action = choice(action_names(BindingKind::Leader), window, cx);
        let form = BindForm {
            leader: cx.new(|cx| InputState::new(window, cx).placeholder("ctrl+alt+space")),
            timeout: cx.new(|cx| InputState::new(window, cx).placeholder("5")),
            timeout_loaded: String::new(),
            wait: cx.new(|cx| InputState::new(window, cx).placeholder("0.4")),
            wait_loaded: String::new(),
            armed: false,
            kind: kind.clone(),
            trigger: cx.new(|cx| InputState::new(window, cx).placeholder("gp   ·   ctrl+alt+v   ·   dd")),
            action: action.clone(),
            args: cx.new(|cx| TextareaState::new(window, cx).rows(2).placeholder("{\"duplicates\":\"preserve\"}")),
            editing: None,
            data: None,
            sync: Vec::new(),
        };
        let subs = vec![
            // A new kind has its own actions.
            cx.subscribe_in(&kind, window, |this: &mut AppView, _, _: &SelectEvent<Vec<SharedString>>, window, cx| {
                this.form.populate_actions(window, cx);
                cx.notify();
            }),
            // The argument hint follows the action.
            cx.subscribe(&action, |_, _, _: &SelectEvent<Vec<SharedString>>, cx| cx.notify()),
        ];
        (form, subs)
    }

    fn kind(&self, cx: &App) -> BindingKind {
        let row = self.kind.read(cx).selected_index(cx).map_or(0, |ix| ix.row);
        KINDS[row.min(KINDS.len() - 1)].0
    }

    fn action(&self, cx: &App) -> String {
        self.action.read(cx).selected_value().map(|a| a.to_string()).unwrap_or_default()
    }

    /// The action list for the chosen kind, keeping the choice when the new
    /// list has it and falling back to the first entry when not.
    fn populate_actions(&mut self, window: &mut Window, cx: &mut Context<AppView>) {
        let items = action_names(self.kind(cx));
        self.action.update(cx, |state, cx| {
            state.set_items(items, window, cx);
            if state.selected_index(cx).is_none() {
                state.set_selected_index(Some(IndexPath::new(0)), window, cx);
            }
        });
    }

    fn reset(&mut self, window: &mut Window, cx: &mut Context<AppView>) {
        self.editing = None;
        self.trigger.update(cx, |s, cx| s.set_value("", window, cx));
        self.args.update(cx, |s, cx| s.set_value("", window, cx));
    }

    /// Put a configured binding into the form.
    fn fill(&mut self, kind: BindingKind, b: &Binding, window: &mut Window, cx: &mut Context<AppView>) {
        self.editing = Some((kind, b.clone()));
        let row = KINDS.iter().position(|(k, _)| *k == kind).unwrap_or(0);
        self.kind.update(cx, |s, cx| s.set_selected_index(Some(IndexPath::new(row)), window, cx));
        self.populate_actions(window, cx);
        let action: SharedString = b.action.clone().into();
        self.action.update(cx, |s, cx| s.set_selected_value(&action, window, cx));
        self.trigger.update(cx, |s, cx| s.set_value(b.trigger.as_str(), window, cx));
        let args = if has_args(b) { b.args.to_string() } else { String::new() };
        self.args.update(cx, |s, cx| s.set_value(args.as_str(), window, cx));
    }
}

/// What a binding is removed by: the trigger, or for a TUI key the action.
fn identity(kind: BindingKind, trigger: &str, action: &str) -> String {
    (if kind == BindingKind::Tui { action } else { trigger }).to_string()
}

fn has_args(b: &Binding) -> bool {
    b.args.as_object().is_some_and(|o| !o.is_empty())
}

impl AppView {
    /// Apply what a reply left for the form (see [`FormSync`]). Called from render.
    pub fn sync_form(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        for sync in std::mem::take(&mut self.form.sync) {
            match sync {
                FormSync::Leader(trigger, timeout, wait) => {
                    self.form.leader.update(cx, |s, cx| s.set_value(trigger.as_str(), window, cx));
                    self.form.timeout.update(cx, |s, cx| s.set_value(timeout.as_str(), window, cx));
                    self.form.wait.update(cx, |s, cx| s.set_value(wait.as_str(), window, cx));
                }
                FormSync::Reset => self.form.reset(window, cx),
            }
        }
    }

    pub fn load_bindings(&mut self, cx: &mut Context<Self>) {
        self.send(Action::BindList, cx, |this, body, _| {
            if let ResultBody::Bindings { leader, leader_timeout_ms, leader_wait_ms, sequences, hotkeys, tui, rejected } = body {
                this.form.armed = leader.is_some();
                // Zero is a daemon too old to say: leave the field empty.
                let seconds = |ms: u64| if ms == 0 { String::new() } else { (ms as f64 / 1000.0).to_string() };
                let (timeout, wait) = (seconds(leader_timeout_ms), seconds(leader_wait_ms));
                this.form.timeout_loaded = timeout.clone();
                this.form.wait_loaded = wait.clone();
                this.form.sync.push(FormSync::Leader(leader.unwrap_or_default(), timeout, wait));
                this.form.data = Some(BindingsData { sequences, hotkeys, tui, rejected });
            }
        });
    }

    fn save_leader(&mut self, cx: &mut Context<Self>) {
        let trigger = self.form.leader.read(cx).value().trim().to_string();
        let Ok(timeout_ms) = seconds_ms(&self.form.timeout.read(cx).value(), &self.form.timeout_loaded, 0.5..=30.0)
        else {
            return self.toast("the timeout must be 0.5 to 30 seconds", cx);
        };
        let Ok(wait_ms) = seconds_ms(&self.form.wait.read(cx).value(), &self.form.wait_loaded, 0.1..=5.0) else {
            return self.toast("the wait must be 0.1 to 5 seconds", cx);
        };
        let action = Action::BindLeader {
            trigger: (!trigger.is_empty()).then_some(trigger),
            enabled: Some(self.form.armed),
            timeout_ms,
            wait_ms,
        };
        self.send(action, cx, |this, _, cx| {
            this.toast("leader saved", cx);
            this.load_bindings(cx);
        });
    }

    fn save_binding(&mut self, cx: &mut Context<Self>) {
        let kind = self.form.kind(cx);
        let trigger = self.form.trigger.read(cx).value().trim().to_string();
        let action = self.form.action(cx);
        if trigger.is_empty() {
            return self.toast("a trigger is required", cx);
        }
        let mut args = serde_json::Value::Null;
        let raw = self.form.args.read(cx).value().trim().to_string();
        if !raw.is_empty() && kind != BindingKind::Tui {
            match serde_json::from_str(&raw) {
                Ok(value) => args = value,
                Err(error) => return self.toast(format!("arguments are not valid JSON: {error}"), cx),
            }
        }
        fn saved(this: &mut AppView, _: ResultBody, cx: &mut Context<AppView>) {
            this.toast("binding saved", cx);
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
            this.toast("config reloaded", cx);
            this.load_config(cx);
            this.load_bindings(cx);
        });
    }

    pub fn load_config(&mut self, cx: &mut Context<Self>) {
        self.send(Action::ConfigShow, cx, |this, body, _| {
            if let ResultBody::Config { toml, .. } = body {
                this.config_toml = Some(toml);
            }
        });
    }

    pub fn bindings_screen(&self, cx: &Context<Self>) -> impl IntoElement {
        let form = &self.form;
        let editing = form.editing.is_some();
        let hint = match action_spec(&form.action(cx)) {
            Some(spec) if form.kind(cx) != BindingKind::Tui => hint(spec),
            _ => "no arguments".to_string(),
        };

        v_flex()
            .gap(px(16.))
            .child(heading(
                "Bindings",
                "The leader chord, global hotkeys, and the keys inside this app. Changes save to your config.",
            ))
            .child(
                section("Leader").child(
                    v_form().child(
                        field()
                            .label("Chord")
                            .description(
                                "A chord like ctrl+alt+space or cmd+shift+v, then the seconds allowed between keys after the leader. Esc cancels.",
                            )
                            .child(
                                h_flex()
                                    .gap(px(10.))
                                    .child(div().w(px(220.)).flex_none().child(Input::new(&form.leader)))
                                    .child(div().w(px(56.)).flex_none().child(Input::new(&form.timeout)))
                                    .child(div().text_color(cx.theme().muted_foreground).child("s"))
                                    .child(Switch::new("leader-armed").label("armed").checked(form.armed).on_click(
                                        cx.listener(|this, on: &bool, _, cx| {
                                            this.form.armed = *on;
                                            cx.notify();
                                        }),
                                    ))
                                    .child(
                                        primary("leader-save", "Save")
                                            .icon(IconName::Save)
                                            .small()
                                            .on_click(cx.listener(|this, _, _, cx| this.save_leader(cx))),
                                    ),
                            ),
                    )
                    .child(
                        field()
                            .label("Wait for longer sequence")
                            .description(
                                "When one sequence starts another (s and ss), the shorter fires after this many seconds with no next key.",
                            )
                            .child(
                                h_flex()
                                    .gap(px(10.))
                                    .child(div().w(px(56.)).flex_none().child(Input::new(&form.wait)))
                                    .child(div().text_color(cx.theme().muted_foreground).child("s")),
                            ),
                    ),
                ),
            )
            .child(
                section(if editing { "Edit binding" } else { "Add a binding" })
                    .child(
                        v_form()
                            .columns(2)
                            .child(field().label("Kind").child(Select::new(&form.kind)))
                            .child(field().label("Trigger").child(Input::new(&form.trigger)))
                            .child(field().label("Action").col_span(2).child(Select::new(&form.action)))
                            .child(
                                field()
                                    .label("Arguments (JSON, optional)")
                                    .description(hint)
                                    .col_span(2)
                                    .child(Textarea::new(&form.args)),
                            ),
                    )
                    .child(
                        h_flex()
                            .gap(px(8.))
                            .mt(px(12.))
                            .child(
                                primary("b-save", if editing { "Save binding" } else { "Add binding" })
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
            .child(section("Configured").child(self.configured(cx)))
    }

    /// The `#bindings-list`: groups of rows with Edit / Remove, then what the
    /// platform refused.
    fn configured(&self, cx: &Context<Self>) -> impl IntoElement {
        let Some(data) = &self.form.data else {
            return v_flex().child(empty("Loading…"));
        };
        let group = |title: &'static str, items: &[Binding], kind: BindingKind, tag: &'static str| {
            (!items.is_empty()).then(|| {
                v_flex().gap(px(6.)).child(subtitle(title, cx)).children(items.iter().enumerate().map(|(ix, b)| {
                    let (edit, remove) = (b.clone(), (kind, identity(kind, &b.trigger, &b.action)));
                    row(
                        mono(12.5).min_w_0().truncate().child(
                            h_flex()
                                .gap(px(6.))
                                .child(trigger_chip(&b.trigger))
                                .child(div().text_color(cx.theme().muted_foreground).child(format!("→ {}", b.action)))
                                .when(has_args(b), |this| {
                                    this.child(div().text_color(cx.theme().muted_foreground).child(b.args.to_string()))
                                }),
                        ),
                        h_flex()
                            .gap(px(6.))
                            .child(
                                icon_button((tag, ix), IconName::Pencil).tooltip("Edit").on_click(cx.listener(
                                    move |this, _, window, cx| {
                                        this.form.fill(kind, &edit, window, cx);
                                        cx.notify();
                                    },
                                )),
                            )
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
            v_flex().gap(px(6.)).child(subtitle("Not active", cx)).children(data.rejected.iter().map(|r| {
                row(
                    mono(12.5).min_w_0().truncate().child(
                        h_flex()
                            .gap(px(6.))
                            .child(trigger_chip(&r.trigger))
                            .child(div().text_color(cx.theme().muted_foreground).child(r.reason.clone())),
                    ),
                    Tag::warning().outline().small().child("REJECTED"),
                    cx,
                )
            }))
        });
        let groups = [
            group("Leader sequences", &data.sequences, BindingKind::Leader, "leader"),
            group("Hotkeys", &data.hotkeys, BindingKind::Hotkey, "hotkey"),
            group("App keys (changed)", &data.tui, BindingKind::Tui, "tui"),
        ];
        let none = groups.iter().all(Option::is_none) && rejected.is_none();
        v_flex()
            .gap(px(12.))
            .children(groups.into_iter().flatten())
            .children(rejected)
            .when(none, |this| this.child(empty("No bindings configured.")))
    }
}

fn subtitle(text: &'static str, cx: &App) -> Div {
    div().text_color(cx.theme().muted_foreground).child(text)
}

/// A chord as a [`Kbd`] when it parses as one keystroke (`ctrl+alt+v`), else
/// (`dd`, a multi-key sequence) as plain bold text.
fn trigger_chip(trigger: &str) -> AnyElement {
    match Keystroke::parse(trigger) {
        Ok(stroke) => Kbd::new(stroke).into_any_element(),
        Err(_) => div().font_weight(FontWeight::BOLD).child(trigger.to_string()).into_any_element(),
    }
}

/// A mono line on the left, tools on the right.
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
        .child(div().flex_1().min_w_0().child(content))
        .child(tools)
}

/// `.field-help`.
pub fn help(text: impl Into<SharedString>) -> Div {
    div().text_size(px(11.5)).line_height(relative(1.5)).text_color(rgb(pal().faint)).mt(px(5.)).child(text.into())
}

#[cfg(test)]
mod tests {
    use super::{hint, seconds_ms};
    use copycat_protocol::action_spec;

    #[test]
    fn the_hint_shows_required_arguments_and_the_timeout_stays_in_range() {
        let hint_for = |name| hint(action_spec(name).unwrap());
        assert!(hint_for("paste.offset").ends_with(r#"needs {"offset":1}"#), "{}", hint_for("paste.offset"));
        assert!(hint_for("ui.show").ends_with(r#"needs {"target":"window"}"#));
        assert!(hint_for("paste.transform").ends_with(r#"needs {"transform":"plain"}"#));
        assert!(!hint_for("history.pause").contains("needs"));

        let timeout_ms = |text, loaded| seconds_ms(text, loaded, 0.5..=30.0);
        assert_eq!(timeout_ms(" 5 ", ""), Ok(Some(5000)));
        assert_eq!(timeout_ms("2.5", "5"), Ok(Some(2500)));
        assert_eq!(timeout_ms("", "5"), Ok(None));
        assert!(timeout_ms("0.2", "").is_err() && timeout_ms("31", "").is_err() && timeout_ms("five", "").is_err());
        // An unchanged field is not sent, so it is not validated either.
        assert_eq!(timeout_ms("0.2", "0.2"), Ok(None));
        assert_eq!(seconds_ms("0.25", "0.4", 0.1..=5.0), Ok(Some(250)));
        assert!(seconds_ms("0.05", "", 0.1..=5.0).is_err());
    }
}
