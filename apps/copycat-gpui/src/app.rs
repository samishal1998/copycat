//! The daemon's polled state, the main window's state and the round trips
//! that feed it, and the frame around the screens (sidebar, status line).
//!
//! Every socket call blocks, so each one runs on the background executor and
//! lands back on the view through `WeakEntity::update`; nothing here ever
//! talks to the daemon from the UI thread.

use std::time::Duration;

use copycat_core::{ClipId, ClipSummary, CoreError, SessionSummary};
use copycat_protocol::{
    Action, CheckStatus, DoctorReport, ResultBody, StatusReport, call,
};
use gpui_kit::component::input::{InputEvent, InputState};
use gpui_kit::component::sidebar::{Sidebar, SidebarMenu, SidebarMenuItem};
use gpui_kit::component::tag::Tag;
use gpui_kit::component::{ActiveTheme as _, IconName, Theme, alert::Alert, h_flex, v_flex};
use gpui_kit::prelude::*;
use gpui_kit::*;

/// The daemon as every window sees it. One poll feeds the main window and the
/// tray dropdown: each observes this entity instead of asking on its own.
pub struct Daemon {
    pub connected: bool,
    pub status: Option<StatusReport>,
    /// A failed paste from the tray dropdown, waiting for the main window to
    /// show it: the dropdown closes before its paste, so it is gone by then.
    pub error: Option<SharedString>,
    _poll: Task<()>,
}

struct SharedDaemon(Entity<Daemon>);

impl Global for SharedDaemon {}

impl Daemon {
    /// Start the poll. Once, before any window opens: windows come and go,
    /// this outlives them.
    pub fn start(cx: &mut App) {
        let daemon = cx.new(|cx| Daemon {
            connected: false,
            status: None,
            error: None,
            // Polling, not a subscription: the same 600ms loop the Tauri app
            // and the TUI run. An event stream on the protocol is a later
            // optimization.
            _poll: cx.spawn(async move |this: WeakEntity<Daemon>, cx| {
                // Bring the daemon up before the first poll, so a fresh launch
                // connects instead of flashing "offline".
                cx.background_spawn(async { copycat_launcher::ensure_daemon() }).await;
                loop {
                    let reply = cx
                        .background_spawn(async { call(&copycat_launcher::socket_path(), Action::Status) })
                        .await;
                    let polled = this.update(cx, |daemon, cx| {
                        (daemon.connected, daemon.status) = match reply {
                            Ok(ResultBody::Status(status)) => (true, Some(*status)),
                            _ => (false, None),
                        };
                        cx.notify();
                    });
                    if polled.is_err() {
                        break;
                    }
                    cx.background_executor().timer(Duration::from_millis(600)).await;
                }
            }),
        });
        cx.set_global(SharedDaemon(daemon));
    }

    pub fn get(cx: &App) -> Entity<Self> {
        cx.global::<SharedDaemon>().0.clone()
    }
}

/// Changes whenever the history list may have: a new copy, an eviction, a
/// clear, a reconnect. A view refetches its list when this moves, rather than
/// on a timer.
pub fn history_mark(status: Option<&StatusReport>) -> Option<(Option<ClipId>, usize)> {
    status.map(|s| (s.core.latest.as_ref().map(|c| c.id), s.core.hot_items))
}

#[derive(Clone, Copy, PartialEq, Eq)]
pub enum Screen {
    History,
    Session,
    Settings,
}

pub struct AppView {
    daemon: Entity<Daemon>,
    pub screen: Screen,
    pub search: Entity<InputState>,
    /// The daemon's state as of its last poll, copied so the screens can read
    /// it without a context.
    pub connected: bool,
    pub status: Option<StatusReport>,
    /// `history_mark` when the list was last fetched.
    history_mark: Option<(Option<ClipId>, usize)>,
    pub doctor: Option<DoctorReport>,
    pub clips: Vec<ClipSummary>,
    pub truncated: bool,
    /// The active session's items in paste order, while the Session screen is up.
    pub items: Vec<ClipSummary>,
    /// Which session `items` came from: a reply for a session that has since
    /// ended must not be drawn under the next one, nor moved with its ids.
    items_session: Option<u64>,
    /// A `session.items` request is out, so the poll does not stack another.
    items_loading: bool,
    /// Status polls since the last doctor fetch.
    polls: u32,
    toast: Option<SharedString>,
    toast_gen: u64,
    /// The pending debounced search; replacing it cancels the previous one.
    _search: Option<Task<()>>,
    _subs: Vec<Subscription>,
}

impl AppView {
    pub fn new(window: &mut Window, cx: &mut Context<Self>) -> Self {
        let search = cx.new(|cx| InputState::new(window, cx).placeholder("Search history"));
        let daemon = Daemon::get(cx);
        let _subs = vec![
            cx.observe(&daemon, |view, _, cx| view.on_daemon(cx)),
            cx.subscribe(&search, |this: &mut Self, _, event: &InputEvent, cx| {
                if matches!(event, InputEvent::Change) {
                    this.search_soon(cx);
                }
            }),
            cx.observe_window_appearance(window, |_, window, cx| {
                Theme::sync_system_appearance(Some(window), cx);
            }),
        ];

        let mut view = AppView {
            daemon,
            screen: Screen::History,
            search,
            connected: false,
            status: None,
            history_mark: None,
            doctor: None,
            clips: Vec::new(),
            truncated: false,
            items: Vec::new(),
            items_session: None,
            items_loading: false,
            polls: 0,
            toast: None,
            toast_gen: 0,
            _search: None,
            _subs,
        };
        // A window reopened from the tray starts from the last poll, not blank.
        view.on_daemon(cx);
        view
    }

    pub fn session(&self) -> Option<&SessionSummary> {
        self.status.as_ref().and_then(|s| s.core.session.as_ref())
    }

    pub fn paused(&self) -> bool {
        self.status.as_ref().is_some_and(|s| s.core.paused)
    }

    fn on_daemon(&mut self, cx: &mut Context<Self>) {
        let was_connected = self.connected;
        let had_session = self.session().is_some();
        if let Some(error) = self.daemon.update(cx, |daemon, _| daemon.error.take()) {
            self.toast(error, cx);
        }
        let daemon = self.daemon.read(cx);
        self.connected = daemon.connected;
        self.status = daemon.status.clone();
        if !self.connected {
            // So the list is fetched again on reconnect.
            self.history_mark = None;
            cx.notify();
            return;
        }
        let mark = history_mark(self.status.as_ref());
        if mark != self.history_mark {
            self.history_mark = mark;
            self.load_history(cx);
        }
        // Diagnostics are cheap but not free: fetch them when the daemon comes
        // up, when a session starts (that is when the keyboard tap starts, and
        // can be refused), and every ~10s — not on every poll.
        let session_started = !had_session && self.session().is_some();
        self.polls += 1;
        if !was_connected || session_started || self.polls >= 16 {
            self.polls = 0;
            self.load_doctor(cx);
        }
        // Items only while they are on screen.
        if self.screen == Screen::Session {
            self.load_items(cx);
        }
        cx.notify();
    }

    /// Send one action off the UI thread; `then` gets the reply, and an error
    /// becomes the status line (the daemon's message is written for a person).
    pub fn send(
        &self,
        action: Action,
        cx: &mut Context<Self>,
        then: impl FnOnce(&mut Self, ResultBody, &mut Context<Self>) + 'static,
    ) {
        let pastes = injects_paste(&action) && cfg!(target_os = "macos");
        // Clicking the window made Copycat the active app, so the paste chord
        // the daemon injects would land here. Deactivating hands focus back to
        // the app the user was in, as the Tauri app and the dropdown do. Not
        // `cx.hide()`: a hidden app would open the tray dropdown invisibly
        // afterwards, now that it keeps running in the menu bar. Not while
        // offline either: nothing would be pasted.
        #[cfg(target_os = "macos")]
        if pastes && self.connected {
            crate::tray::deactivate();
        }
        request(action, cx, move |view, reply, cx| match reply {
            Ok(body) => then(view, body, cx),
            Err(error) => {
                // The paste failed, so bring the window back to show why.
                if pastes {
                    cx.activate(true);
                }
                view.toast(error.message, cx)
            }
        });
    }

    pub fn toast(&mut self, text: impl Into<SharedString>, cx: &mut Context<Self>) {
        self.toast = Some(text.into());
        self.toast_gen += 1;
        let generation = self.toast_gen;
        cx.spawn(async move |this, cx| {
            cx.background_executor().timer(Duration::from_millis(2400)).await;
            let _ = this.update(cx, |view, cx| {
                if view.toast_gen == generation {
                    view.toast = None;
                    cx.notify();
                }
            });
        })
        .detach();
        cx.notify();
    }

    /// Search 200ms after the last keystroke, so typing is not a request per key.
    fn search_soon(&mut self, cx: &mut Context<Self>) {
        self._search = Some(cx.spawn(async move |this, cx| {
            cx.background_executor().timer(Duration::from_millis(200)).await;
            let _ = this.update(cx, |view, cx| view.load_history(cx));
        }));
    }

    pub fn load_history(&mut self, cx: &mut Context<Self>) {
        let query = self.search.read(cx).value().trim().to_string();
        let action = if query.is_empty() {
            Action::HistoryList { limit: 100, raw: false }
        } else {
            Action::HistorySearch { query, limit: 100 }
        };
        self.send(action, cx, |view, body, _| {
            if let ResultBody::Clips { clips, truncated } = body {
                view.clips = clips;
                view.truncated = truncated;
            }
        });
    }

    pub fn load_items(&mut self, cx: &mut Context<Self>) {
        let Some(session) = self.session().map(|s| s.id) else {
            self.items.clear();
            self.items_session = None;
            return;
        };
        if self.items_loading {
            return;
        }
        self.items_loading = true;
        // Not through `send`: its callback is skipped on an error, and the
        // flag must clear either way or the items never load again. A failure
        // here is the poll's business, not a toast's.
        request(Action::SessionItems, cx, move |view, reply, _| {
            view.items_loading = false;
            // Dropped if the session changed while the request was out.
            if let Ok(ResultBody::Clips { clips, .. }) = reply
                && view.session().map(|s| s.id) == Some(session)
            {
                view.items = clips;
                view.items_session = Some(session);
            }
        });
    }

    /// The items, if they belong to session `id`; none if they are another's.
    pub fn items_for(&self, id: u64) -> &[ClipSummary] {
        if self.items_session == Some(id) { &self.items } else { &[] }
    }

    pub fn load_doctor(&mut self, cx: &mut Context<Self>) {
        self.send(Action::Doctor, cx, |view, body, _| {
            if let ResultBody::Doctor(doctor) = body {
                view.doctor = Some(*doctor);
            }
        });
    }

    pub fn show(&mut self, screen: Screen, cx: &mut Context<Self>) {
        self.screen = screen;
        match screen {
            Screen::History => self.load_history(cx),
            Screen::Session => self.load_items(cx),
            Screen::Settings => self.load_doctor(cx),
        }
        cx.notify();
    }

    /// The blocking permission case: the daemon's keyboard tap was refused.
    /// Same rule as `permissionBlocked()` in the Tauri app.
    pub fn permission_blocked(&self) -> bool {
        const CHECKS: [&str; 4] =
            ["macos-permissions", "paste-injection", "paste-interception", "global-hotkeys"];
        self.doctor.as_ref().is_some_and(|doctor| {
            doctor.checks.iter().any(|c| {
                let detail = c.detail.to_lowercase();
                c.status == CheckStatus::Unavailable
                    && CHECKS.contains(&c.name.as_str())
                    && ["accessibility", "input monitoring", "event tap"]
                        .iter()
                        .any(|word| detail.contains(word))
            })
        })
    }

    fn mode_chip(&self) -> Tag {
        let Some(status) = &self.status else {
            return Tag::secondary().child("—");
        };
        match &status.core.session {
            Some(s) if s.state == copycat_core::SessionState::Capturing => {
                Tag::info().child(format!("{} · capturing", s.mode.as_str().to_uppercase()))
            }
            Some(s) => Tag::info().child(format!(
                "{} · {}/{}",
                s.mode.as_str().to_uppercase(),
                s.cursor,
                s.size
            )),
            None if status.core.paused => Tag::warning().child("PAUSED"),
            None => Tag::secondary().child("NORMAL"),
        }
    }

    fn sidebar(&self, cx: &Context<Self>) -> impl IntoElement {
        let theme = cx.theme();
        let (success, danger, muted) = (theme.success, theme.danger, theme.muted_foreground);
        let item = |label: &'static str, icon: IconName, screen: Screen| {
            SidebarMenuItem::new(label)
                .icon(icon)
                .active(self.screen == screen)
                .on_click(cx.listener(move |this, _, _, cx| this.show(screen, cx)))
        };
        let menu = SidebarMenu::new().children([
            item("History", IconName::Inbox, Screen::History),
            item("Session", IconName::LayoutDashboard, Screen::Session),
            item("Settings", IconName::Settings, Screen::Settings),
        ]);

        let footer = v_flex()
            .w_full()
            .gap_2()
            .when(self.permission_blocked(), |this| {
                this.child(
                    div()
                        .id("perm-alert")
                        .cursor_pointer()
                        .on_click(cx.listener(|this, _, _, cx| this.show(Screen::Settings, cx)))
                        .child(Alert::warning("perm", "Permission needed — open Settings")),
                )
            })
            .child(h_flex().child(self.mode_chip()))
            .child(
                h_flex()
                    .gap_2()
                    .text_sm()
                    .child(
                        div()
                            .size(px(8.))
                            .rounded_full()
                            .bg(if self.connected { success } else { danger }),
                    )
                    .child(if self.connected { "connected" } else { "daemon offline" }),
            )
            .when_some(self.status.as_ref(), |this, status| {
                this.child(
                    div()
                        .text_xs()
                        .text_color(muted)
                        .child(format!("copycat v{}", status.daemon_version)),
                )
            });

        Sidebar::new("rail")
            .collapsible(false)
            .w(px(210.))
            .header(div().px_2().font_weight(FontWeight::SEMIBOLD).child("Copycat"))
            .child(menu)
            .footer(footer)
    }
}

/// Send one action off the UI thread and hand the reply to `done` on the view,
/// error or not. A paste first waits for focus to settle in the app the caller
/// handed it back to.
pub fn request<V: 'static>(
    action: Action,
    cx: &mut Context<V>,
    done: impl FnOnce(&mut V, Result<ResultBody, CoreError>, &mut Context<V>) + 'static,
) {
    let socket = copycat_launcher::socket_path();
    let pastes = injects_paste(&action) && cfg!(target_os = "macos");
    cx.spawn(async move |this, cx| {
        let reply = cx
            .background_spawn(async move {
                if pastes {
                    // ponytail: fixed settle delay for the other app to take
                    // focus, as in the Tauri app.
                    std::thread::sleep(Duration::from_millis(150));
                }
                call(&socket, action)
            })
            .await;
        let _ = this.update(cx, |view, cx| {
            done(view, reply, cx);
            cx.notify();
        });
    })
    .detach();
}

/// Whether an action ends with the daemon pressing the paste chord for the user.
pub fn injects_paste(action: &Action) -> bool {
    matches!(
        action,
        Action::PasteLatest { .. }
            | Action::PasteOffset { .. }
            | Action::PasteId { .. }
            | Action::PasteNext { peek: false }
            | Action::PasteMode { inject: true }
            | Action::GroupPaste
            | Action::GroupPasteLast { .. }
    )
}

impl Render for AppView {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let screen = match self.screen {
            Screen::History => self.history_screen(cx).into_any_element(),
            Screen::Session => self.session_screen(cx).into_any_element(),
            Screen::Settings => self.settings_screen(cx).into_any_element(),
        };
        let theme = cx.theme();
        let (background, foreground, border) = (theme.background, theme.foreground, theme.border);
        h_flex()
            .size_full()
            .items_start()
            .bg(background)
            .text_color(foreground)
            .child(self.sidebar(cx))
            .child(
                v_flex()
                    .flex_1()
                    .min_w_0()
                    .h_full()
                    .child(v_flex().flex_1().min_h_0().p_4().gap_3().child(screen))
                    .when_some(self.toast.clone(), |this, toast| {
                        this.child(
                            div()
                                .px_4()
                                .py_2()
                                .border_t_1()
                                .border_color(border)
                                .text_sm()
                                .child(toast),
                        )
                    }),
            )
    }
}
