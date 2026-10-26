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
use gpui_kit::component::scroll::ScrollableElement as _;
use gpui_kit::component::alert::Alert;
use gpui_kit::component::sidebar::{Sidebar, SidebarMenu, SidebarMenuItem};
use gpui_kit::component::{Icon, Sizable as _, TitleBar, WindowExt as _, h_flex, v_flex};
use gpui_kit::component::notification::Notification;
use gpui_kit::prelude::*;
use gpui_kit::*;

use crate::bindings::BindForm;
use crate::brand::*;
use crate::settings::Settings;

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
    Bindings,
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
    /// The Bindings screen's form and the list it edits.
    pub form: BindForm,
    /// `config.show`'s TOML, for the Settings screen.
    pub config_toml: Option<String>,
    pub settings: Entity<Settings>,
    /// A system `copycatd` exists; if not, the app runs its bundled one and
    /// says so in a banner.
    installed: bool,
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
    window: AnyWindowHandle,
    /// The pending debounced search; replacing it cancels the previous one.
    _search: Option<Task<()>>,
    _subs: Vec<Subscription>,
}

impl AppView {
    pub fn new(window: &mut Window, cx: &mut Context<Self>) -> Self {
        let search = cx.new(|cx| InputState::new(window, cx).placeholder("Search history…"));
        let daemon = Daemon::get(cx);
        let (form, form_subs) = BindForm::new(window, cx);
        let settings = Settings::get(cx);
        let mut _subs = vec![
            cx.observe(&daemon, |view, _, cx| view.on_daemon(cx)),
            cx.observe(&settings, |_, _, cx| cx.notify()),
            cx.subscribe(&search, |this: &mut Self, _, event: &InputEvent, cx| {
                if matches!(event, InputEvent::Change) {
                    this.search_soon(cx);
                }
            }),
        ];
        _subs.extend(form_subs);

        let mut view = AppView {
            daemon,
            // Screenshot support: CI starts the app on each screen in turn.
            screen: match std::env::var("COPYCAT_GPUI_SCREEN").as_deref() {
                Ok("session") => Screen::Session,
                Ok("bindings") => Screen::Bindings,
                Ok("settings") => Screen::Settings,
                _ => Screen::History,
            },
            search,
            connected: false,
            status: None,
            history_mark: None,
            doctor: None,
            form,
            config_toml: None,
            settings,
            installed: copycat_launcher::find_system_copycatd().is_some(),
            clips: Vec::new(),
            truncated: false,
            items: Vec::new(),
            items_session: None,
            items_loading: false,
            polls: 0,
            window: window.window_handle(),
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
        // Re-checked, so installing the CLI hides the banner without a restart.
        if !self.installed {
            self.installed = copycat_launcher::find_system_copycatd().is_some();
        }
        let was_connected = self.connected;
        let had_session = self.session().is_some();
        if let Some(error) = self.daemon.update(cx, |daemon, _| daemon.error.take()) {
            self.toast_error(error, cx);
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
        if !was_connected {
            // The rail's Bindings count and the Settings TOML need a first fetch.
            self.load_bindings(cx);
            self.load_config(cx);
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
                view.toast_error(error.message, cx)
            }
        });
    }

    /// A notification in the window's stock toast stack. Deferred: callers
    /// have a view context, not the window, and Root may be mid-update.
    pub fn toast(&mut self, text: impl Into<SharedString>, cx: &mut Context<Self>) {
        self.notify_with(Notification::info(text), cx);
    }

    pub fn toast_error(&mut self, text: impl Into<SharedString>, cx: &mut Context<Self>) {
        self.notify_with(Notification::error(text), cx);
    }

    fn notify_with(&mut self, note: Notification, cx: &mut Context<Self>) {
        let handle = self.window;
        cx.defer(move |cx| {
            let _ = handle.update(cx, |_, window, cx| window.push_notification(note, cx));
        });
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
            Screen::Bindings => self.load_bindings(cx),
            Screen::Settings => {
                self.load_config(cx);
                self.load_doctor(cx)
            }
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

    /// The rail foot's mode line: the mode and where it stands, and the size
    /// beside it. Same text as `renderRailState` in the Tauri app.
    fn rail_state(&self) -> (String, u32, String) {
        let Some(status) = &self.status else {
            return ("—".into(), TEXT, String::new());
        };
        let plural = |n: usize, word: &str| format!("{n} {word}{}", if n == 1 { "" } else { "s" });
        match &status.core.session {
            Some(s) => {
                let mode = s.mode.as_str().to_uppercase();
                let at = if s.state == copycat_core::SessionState::Capturing {
                    format!("{mode} · capturing")
                } else {
                    format!("{mode} · {}/{}", s.cursor, s.size)
                };
                (at, ACTIVE, plural(s.size, "item"))
            }
            None if status.core.paused => ("PAUSED".into(), WARN, plural(status.core.hot_items, "clip")),
            None => ("NORMAL".into(), TEXT, plural(status.core.hot_items, "clip")),
        }
    }

    /// Any unavailable capability, as the Settings badge shows it.
    fn diagnostic_failing(&self) -> bool {
        self.doctor.as_ref().is_some_and(|d| d.checks.iter().any(|c| c.status == CheckStatus::Unavailable))
    }

    /// The left rail of main.html: a stock Sidebar (brand, nav) and the live foot.
    fn rail(&self, cx: &Context<Self>) -> impl IntoElement {
        let core = self.status.as_ref().map(|s| &s.core);
        let nav = |label: &'static str, icon: IconName, screen: Screen, badge: Option<usize>| {
            SidebarMenuItem::new(label)
                .icon(Icon::from(icon))
                .active(self.screen == screen)
                .on_click(cx.listener(move |this, _, _, cx| this.show(screen, cx)))
                .suffix(move |_, _| div().children(badge.map(|n| mono(10.).text_color(rgb(FAINT)).child(n.to_string()))))
        };
        let failing = self.diagnostic_failing();

        let (mode, mode_color, size) = self.rail_state();
        let line = || mono(10.5).flex().items_center().gap(px(7.)).text_color(rgb(FAINT)).mt(px(6.));
        let foot = v_flex()
            .w_full()
            .pt(px(12.))
            .border_t_1()
            .border_color(rgb(LINE_SOFT))
            .child(
                h_flex()
                    .gap(px(8.))
                    .pb(px(10.))
                    .child(mono(11.).font_weight(FontWeight::BOLD).text_color(rgb(mode_color)).child(spaced(&mode, 0.44)))
                    .child(mono(10.5).text_color(rgb(FAINT)).child(size)),
            )
            .child(
                line()
                    .mt_0()
                    .text_color(rgb(MUTED))
                    .child(dot(self.connected, false))
                    .child(if self.connected { "connected" } else { "daemon offline" }),
            )
            .when_some(self.status.as_ref(), |this, status| {
                this.child(line().child(format!("key: {}", status.key_storage)))
            })
            .when(self.permission_blocked(), |this| {
                this.child(
                    button("perm-alert", "Permission needed — open Settings", Kind::Danger, true)
                        .icon(IconName::TriangleAlert)
                        .my(px(8.))
                        .on_click(cx.listener(|this, _, _, cx| this.show(Screen::Settings, cx))),
                )
            })
            .when_some(self.status.as_ref(), |this, status| {
                this.child(line().mt_0().text_color(rgb(MUTED)).child(format!("copycat v{}", status.daemon_version)))
            });

        Sidebar::<SidebarMenu>::new("rail")
            .w(px(184.))
            .collapsible(false)
            .header(h_flex().gap(px(9.)).child(mark(22.)).child(mono(14.).font_weight(FontWeight::BOLD).child("copycat")))
            .child(SidebarMenu::new().children([
                nav("History", IconName::Clock, Screen::History, core.map(|c| c.hot_items)),
                nav("Session", IconName::ListOrdered, Screen::Session, core.and_then(|c| c.session.as_ref()).map(|s| s.remaining)),
                nav("Bindings", IconName::Keyboard, Screen::Bindings, self.form.data.as_ref().map(|d| d.count())),
                nav("Settings", IconName::Settings, Screen::Settings, None).suffix(move |_, _| {
                    div().children(failing.then(|| Icon::from(IconName::TriangleAlert).small().text_color(rgb(BAD))))
                }),
            ]))
            .footer(foot)
    }
}

const INSTALL_CMD: &str = "curl -fsSL https://raw.githubusercontent.com/samishal1998/copycat/main/install.sh | sh";

impl AppView {
    /// `#install-banner`: shown while the app runs its bundled daemon because
    /// no command-line install exists.
    fn install_banner(&self, cx: &Context<Self>) -> impl IntoElement {
        h_flex()
            .gap(px(12.))
            .mb(px(20.))
            .child(
                div().flex_1().min_w_0().child(
                    Alert::info(
                        "install-banner",
                        "Install the Copycat command-line tools so your terminal and this app share one daemon.",
                    )
                    .title("Running the built-in daemon."),
                ),
            )
            .child(button("install-copy", "Copy install command", Kind::Plain, true).on_click(cx.listener(
                |this, _, _, cx| {
                    cx.write_to_clipboard(ClipboardItem::new_string(INSTALL_CMD.into()));
                    this.toast("install command copied — paste it in a terminal", cx);
                },
            )))
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
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        self.sync_form(window, cx);
        let screen = match self.screen {
            Screen::History => self.history_screen(cx).into_any_element(),
            Screen::Session => self.session_screen(cx).into_any_element(),
            Screen::Bindings => self.bindings_screen(cx).into_any_element(),
            Screen::Settings => self.settings_screen(cx).into_any_element(),
        };
        v_flex()
            .size_full()
            .bg(rgb(SURFACE))
            .text_color(rgb(TEXT))
            .font_family(UI)
            .text_size(px(13.5))
            .line_height(relative(1.5))
            .child(TitleBar::new())
            .child(
                h_flex()
                    .flex_1()
                    .min_h_0()
                    .items_start()
                    .child(self.rail(cx))
                    .child(
                        div().relative().flex_1().min_w_0().h_full().child(
                            v_flex().size_full().overflow_y_scrollbar().child(
                                div()
                                    .pt(px(26.))
                                    .px(px(30.))
                                    .pb(px(40.))
                                    .children((self.connected && !self.installed).then(|| self.install_banner(cx)))
                                    .child(screen),
                            ),
                        ),
                    ),
            )
    }
}
