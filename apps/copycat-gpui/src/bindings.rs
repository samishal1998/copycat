//! The Bindings screen: the leader chord, the add/edit form and the list of
//! what is configured. Every control is one daemon action (`bind.leader`,
//! `bind.set`, `bind.remove`, `bind.list`), as in main.js.

use copycat_protocol::{Action, Binding, BindingKind, RejectedBinding, ResultBody};
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

use crate::app::AppView;
use crate::brand::*;
use crate::screens::empty;

/// Daemon actions a key can be bound to, with a hint of what they need.
const ACTIONS: [(&str, &str); 16] = [
    ("paste.mode", "pop / advance / paste the group — whatever the chord means now"),
    ("paste.next", "consume the session's next item"),
    ("paste.latest", "paste the newest clip"),
    ("paste.offset", "paste an older clip — needs {\"offset\":1}"),
    ("stack.start", "begin a stack — optional {\"duplicates\":\"preserve\"}"),
    ("queue.start", "snapshot the newest N — needs {\"last\":5}"),
    ("queue.capture", "collect a queue from now on"),
    ("queue.seal", "seal the queue"),
    ("group.capture", "collect a group — optional {\"delimiter\":\", \"}"),
    ("group.paste", "paste the captured group"),
    ("group.paste_last", "join the newest N — needs {\"last\":3}"),
    ("session.stop", "end the active session"),
    ("session.reset", "return the cursor to the start"),
    ("history.pause", "stop recording copies"),
    ("history.resume", "resume recording copies"),
    ("history.clear", "clear history"),
];
const TUI_ACTIONS: [&str; 24] = [
    "confirm", "paste_next", "delete", "pin", "search", "toggle_raw", "add", "edit", "test", "toggle_pause",
    "stack_start", "queue_capture", "queue_seal", "group_capture", "group_paste", "session_stop",
    "session_reset", "next_tab", "prev_tab", "down", "up", "refresh", "help", "quit",
];
const KINDS: [(BindingKind, &str); 3] = [
    (BindingKind::Leader, "leader — a key after the leader"),
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
    Leader(String),
    Reset,
}

pub struct BindForm {
    leader: Entity<InputState>,
    armed: bool,
    kind: Choice,
    trigger: Entity<InputState>,
    action: Choice,
    args: Entity<TextareaState>,
    /// The binding being edited: its kind and identity (the trigger; for a
    /// TUI key, the action).
    editing: Option<(BindingKind, String)>,
    pub data: Option<BindingsData>,
    pub sync: Vec<FormSync>,
}

fn choice(items: Vec<SharedString>, window: &mut Window, cx: &mut Context<AppView>) -> Choice {
    cx.new(|cx| SelectState::new(items, Some(IndexPath::new(0)), window, cx))
}

fn action_names(kind: BindingKind) -> Vec<SharedString> {
    match kind {
        BindingKind::Tui => TUI_ACTIONS.iter().map(|a| (*a).into()).collect(),
        _ => ACTIONS.iter().map(|(a, _)| (*a).into()).collect(),
    }
}

impl BindForm {
    pub fn new(window: &mut Window, cx: &mut Context<AppView>) -> (Self, Vec<Subscription>) {
        let kind = choice(KINDS.iter().map(|(_, label)| (*label).into()).collect(), window, cx);
        let action = choice(action_names(BindingKind::Leader), window, cx);
        let form = BindForm {
            leader: cx.new(|cx| InputState::new(window, cx).placeholder("ctrl+alt+space")),
            armed: false,
            kind: kind.clone(),
            trigger: cx.new(|cx| InputState::new(window, cx).placeholder("s   ·   ctrl+alt+v   ·   dd")),
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
        self.editing = Some((kind, if kind == BindingKind::Tui { b.action.clone() } else { b.trigger.clone() }));
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

fn has_args(b: &Binding) -> bool {
    b.args.as_object().is_some_and(|o| !o.is_empty())
}

impl AppView {
    /// Apply what a reply left for the form (see [`FormSync`]). Called from render.
    pub fn sync_form(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        for sync in std::mem::take(&mut self.form.sync) {
            match sync {
                FormSync::Leader(trigger) => {
                    self.form.leader.update(cx, |s, cx| s.set_value(trigger.as_str(), window, cx))
                }
                FormSync::Reset => self.form.reset(window, cx),
            }
        }
    }

    pub fn load_bindings(&mut self, cx: &mut Context<Self>) {
        self.send(Action::BindList, cx, |this, body, _| {
            if let ResultBody::Bindings { leader, sequences, hotkeys, tui, rejected } = body {
                this.form.armed = leader.is_some();
                this.form.sync.push(FormSync::Leader(leader.unwrap_or_default()));
                this.form.data = Some(BindingsData { sequences, hotkeys, tui, rejected });
            }
        });
    }

    fn save_leader(&mut self, cx: &mut Context<Self>) {
        let trigger = self.form.leader.read(cx).value().trim().to_string();
        let action = Action::BindLeader {
            trigger: (!trigger.is_empty()).then_some(trigger),
            enabled: Some(self.form.armed),
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
        let set = Action::BindSet { kind, trigger: trigger.clone(), action: action.clone(), args };
        // A renamed trigger removes the old entry first, so an edit can't
        // leave two.
        let identity = if kind == BindingKind::Tui { action } else { trigger };
        match self.form.editing.clone() {
            Some((old_kind, old)) if old_kind != kind || old != identity => {
                let remove = Action::BindRemove { kind: old_kind, trigger: old };
                self.send(remove, cx, move |this, _, cx| this.send(set, cx, saved));
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
        let hint = if form.kind(cx) == BindingKind::Tui {
            "no arguments"
        } else {
            ACTIONS.iter().find(|(name, _)| *name == form.action(cx)).map_or("no arguments", |(_, hint)| *hint)
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
                        field().label("Chord").description("A chord like ctrl+alt+space or cmd+shift+v.").child(
                            h_flex()
                                .gap(px(10.))
                                .child(div().w(px(220.)).flex_none().child(Input::new(&form.leader)))
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
                    let identity = if kind == BindingKind::Tui { b.action.clone() } else { b.trigger.clone() };
                    let (edit, remove) = (b.clone(), (kind, identity));
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
    div().text_size(px(11.5)).line_height(relative(1.5)).text_color(rgb(FAINT)).mt(px(5.)).child(text.into())
}
