//! The daemon's polled state, the main window's state and the round trips
//! that feed it, and the frame around the screens (sidebar, status line).
//!
//! Every socket call blocks, so each one runs on the background executor and
//! lands back on the view through `WeakEntity::update`; nothing here ever
//! talks to the daemon from the UI thread.

use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::Duration;

use copycat_core::{ClipId, ClipSummary, CoreError, CoreStatus, SessionState, SessionSummary};
use copycat_protocol::{
    Action, CheckStatus, DoctorReport, ResultBody, StatusReport, call,
};
use gpui_kit::component::input::{InputEvent, InputState};
use gpui_kit::component::scroll::ScrollableElement as _;
use gpui_kit::component::sidebar::{Sidebar, SidebarMenu, SidebarMenuItem};
use gpui_kit::component::{Icon, Sizable as _, TitleBar, WindowExt as _, h_flex, v_flex};
use gpui_kit::component::notification::Notification;
use gpui_kit::prelude::*;
use gpui_kit::*;

use crate::bindings::BindForm;
use crate::brand::*;
use crate::brand::px;
use crate::settings::Settings;

/// The release this app was built from (CI sets it from the `gui-v*` tag); the
/// crate version is a placeholder.
pub const APP_VERSION: &str = match option_env!("COPYCAT_GUI_VERSION") {
    Some(v) => v,
    None => env!("CARGO_PKG_VERSION"),
};

/// Where the Settings screen's update check stands.
pub enum Update {
    Idle,
    Checking,
    UpToDate,
    Available { tag: String, dmg_url: String },
    Installing,
    Failed(String),
}

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

/// Grow or shrink the window by `ratio`, within its screen; a fullscreen one
/// fills its screen whatever the scale.
fn rescale(window: &mut Window, ratio: f32, cx: &mut App) {
    if window.is_fullscreen() {
        return;
    }
    #[cfg(target_os = "macos")]
    crate::floating::rescale(window, ratio as f64, cx);
    #[cfg(not(target_os = "macos"))]
    {
        let now = window.viewport_size();
        let size = size(now.width * ratio, now.height * ratio);
        let room = window.display(cx).map(|d| d.visible_bounds().size);
        window.resize(room.map_or(size, |room| size.min(&room)));
    }
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
    /// Where the daemon's config file is, for Settings → Advanced.
    pub config_path: Option<String>,
    /// Settings → Advanced is open.
    pub advanced: bool,
    /// History's "Clear all" was clicked once; the task disarms it.
    pub confirm_clear: Option<Task<()>>,
    pub settings: Entity<Settings>,
    pub update: Update,
    /// A system `copycatd` exists; if not, the app runs its bundled one and
    /// Settings → Advanced offers the command-line tools.
    pub installed: bool,
    pub clips: Vec<ClipSummary>,
    pub truncated: bool,
    /// Counts history requests: only the newest one's reply is shown.
    history_gen: u64,
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
        let mut scale = settings.read(cx).ui_scale;
        let mut _subs = vec![
            cx.observe(&daemon, |view, _, cx| view.on_daemon(cx)),
            // The open window grows or shrinks with the UI scale.
            cx.observe_in(&settings, window, move |_, settings, window, cx| {
                let now = settings.read(cx).ui_scale;
                if now != scale {
                    rescale(window, now / std::mem::replace(&mut scale, now), cx);
                }
                cx.notify();
            }),
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
            config_path: None,
            advanced: false,
            confirm_clear: None,
            settings,
            update: Update::Idle,
            installed: copycat_launcher::find_system_copycatd().is_some(),
            clips: Vec::new(),
            truncated: false,
            history_gen: 0,
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
        // Not in screenshot runs, which must look the same with or without a release out.
        if std::env::var_os("COPYCAT_GPUI_SCREEN").is_none() {
            view.check_update(false, cx);
        }
        view
    }

    /// Look for a newer `gui-v*` release with a .dmg. Only a `manual` check
    /// reports a failure; the one at startup may well run offline.
    pub fn check_update(&mut self, manual: bool, cx: &mut Context<Self>) {
        self.update = Update::Checking;
        cx.notify();
        cx.spawn(async move |this, cx| {
            let found = cx
                .background_spawn(async {
                    let releases = copycat_launcher::fetch_releases()?;
                    let release = copycat_launcher::newer_release(&releases, "gui-v", APP_VERSION);
                    Ok::<_, String>(release.and_then(|r| {
                        let dmg = r.assets.iter().find(|a| a.ends_with(".dmg"))?;
                        Some((r.tag.clone(), dmg.clone()))
                    }))
                })
                .await;
            let _ = this.update(cx, |view, cx| {
                view.update = match found {
                    Ok(Some((tag, dmg_url))) => Update::Available { tag, dmg_url },
                    Ok(None) => Update::UpToDate,
                    Err(error) if manual => Update::Failed(error),
                    Err(_) => Update::Idle,
                };
                cx.notify();
            });
        })
        .detach();
    }

    /// Replace the running Copycat.app with the release's and relaunch it. The
    /// relaunched app's `ensure_daemon` swaps in the newer bundled daemon.
    pub fn install_update(&mut self, cx: &mut Context<Self>) {
        let (Update::Available { dmg_url, .. }, Some(app)) = (&self.update, app_bundle()) else {
            return;
        };
        let url = dmg_url.clone();
        self.update = Update::Installing;
        cx.notify();
        let bundle = app.clone();
        cx.spawn(async move |this, cx| {
            let installed = cx.background_spawn(async move { install_app(&url, &bundle) }).await;
            // Wait for this process to exit, so `open` starts the new app
            // rather than focusing the old one.
            let relaunch = installed.and_then(|()| {
                Command::new("sh")
                    .args(["-c", r#"while kill -0 "$1" 2>/dev/null; do sleep 0.2; done; open "$0""#])
                    .arg(&app)
                    .arg(std::process::id().to_string())
                    .stdin(Stdio::null())
                    .stdout(Stdio::null())
                    .stderr(Stdio::null())
                    .spawn()
                    .map_err(|e| format!("installed, but could not relaunch ({e}): reopen Copycat"))
            });
            // Quit even if the window closed meanwhile: the bundle under this
            // process is gone either way.
            match relaunch {
                Ok(_) => cx.update(|cx| cx.quit()),
                Err(error) => {
                    let _ = this.update(cx, |view, cx| {
                        view.update = Update::Failed(error);
                        cx.notify();
                    });
                }
            }
        })
        .detach();
    }

    pub fn session(&self) -> Option<&SessionSummary> {
        self.status.as_ref().and_then(|s| s.core.session.as_ref())
    }

    pub fn paused(&self) -> bool {
        self.status.as_ref().is_some_and(|s| s.core.paused)
    }

    fn on_daemon(&mut self, cx: &mut Context<Self>) {
        // Re-checked, so installing the CLI hides its offer without a restart.
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
            // The rail's Shortcuts count and the config file's path need a first fetch.
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
        self.history_gen += 1;
        let generation = self.history_gen;
        self.send(action, cx, move |view, body, _| {
            if let ResultBody::Clips { clips, truncated } = body
                && view.history_gen == generation
            {
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
        self.confirm_clear = None;
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

    /// The rail foot's mode line, and the count beside it, in the floating
    /// panel's words. None with no session: History's badge counts the clips.
    fn rail_state(&self) -> Option<(String, u32, String)> {
        let core = &self.status.as_ref()?.core;
        let color = match &core.session {
            Some(_) => ACTIVE,
            None if core.paused => pal().warn,
            None => return None,
        };
        let (mode, count) = status_words(true, Some(core));
        Some((mode, color, count))
    }

    /// Restart the daemon off the UI thread, then look again at what it can do.
    pub fn restart_daemon(&mut self, toast: &'static str, cx: &mut Context<Self>) {
        self.toast(toast, cx);
        cx.spawn(async move |this, cx| {
            cx.background_spawn(async { copycat_launcher::restart_daemon() }).await;
            let _ = this.update(cx, |view, cx| view.load_doctor(cx));
        })
        .detach();
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
                .suffix(move |_, _| div().children(badge.map(|n| mono(10.).text_color(rgb(pal().faint)).child(n.to_string()))))
        };
        let failing = self.diagnostic_failing();

        let foot = v_flex()
            .w_full()
            .pt(px(12.))
            .border_t_1()
            .border_color(rgb(pal().line_soft))
            .children(self.rail_state().map(|(mode, mode_color, count)| {
                h_flex()
                    .gap(px(8.))
                    .pb(px(10.))
                    .child(mono(11.).font_weight(FontWeight::BOLD).text_color(rgb(mode_color)).child(spaced(&mode, 0.44)))
                    .child(mono(10.5).text_color(rgb(pal().faint)).child(count))
            }))
            .child(
                mono(10.5)
                    .flex()
                    .items_center()
                    .gap(px(7.))
                    .text_color(rgb(pal().muted))
                    .child(dot(self.connected, false))
                    .child(if self.connected { "Ready" } else { "Not running" })
                    .when(!self.connected, |this| {
                        this.child(button("rail-restart", "Restart", Kind::Ghost, true).on_click(cx.listener(
                            |this, _, _, cx| this.restart_daemon("Restarting…", cx),
                        )))
                    }),
            )
            .when(self.permission_blocked(), |this| {
                this.child(
                    button("perm-alert", "Needs permission", Kind::Danger, true)
                        .icon(IconName::TriangleAlert)
                        .my(px(8.))
                        .on_click(cx.listener(|this, _, _, cx| this.show(Screen::Settings, cx))),
                )
            })
            .when(matches!(self.update, Update::Available { .. }), |this| {
                this.child(
                    button("update-available", "Update available", Kind::Ghost, true)
                        .mt(px(6.))
                        .on_click(cx.listener(|this, _, _, cx| this.show(Screen::Settings, cx))),
                )
            });

        Sidebar::<SidebarMenu>::new("rail")
            .w(px(184.))
            .collapsible(false)
            .header(h_flex().gap(px(9.)).child(mark(22.)).child(mono(14.).font_weight(FontWeight::BOLD).child("copycat")))
            .child(SidebarMenu::new().children([
                nav("History", IconName::Clock, Screen::History, core.map(|c| c.hot_items)),
                nav("Session", IconName::ListOrdered, Screen::Session, core.and_then(|c| c.session.as_ref()).map(|s| s.remaining)),
                nav("Shortcuts", IconName::Keyboard, Screen::Bindings, self.form.data.as_ref().map(|d| d.count())),
                nav("Settings", IconName::Settings, Screen::Settings, None).suffix(move |_, _| {
                    div().children(failing.then(|| Icon::from(IconName::TriangleAlert).small().text_color(rgb(pal().bad))))
                }),
            ]))
            .footer(foot)
    }
}

/// The Copycat.app this process runs from (`…/Copycat.app/Contents/MacOS/copycat-gpui`):
/// only a bundle can update itself in place. None on other platforms.
pub fn app_bundle() -> Option<PathBuf> {
    let exe = std::env::current_exe().ok()?;
    let app = exe.ancestors().nth(3)?;
    (cfg!(target_os = "macos") && app.extension().is_some_and(|e| e == "app")).then(|| app.to_path_buf())
}

/// Download the .dmg at `$1` and swap its app in for the bundle at `$2`. The
/// new copy is staged beside the old one, so the swap is two renames, and the
/// old one comes back if the second fails.
const INSTALL_SCRIPT: &str = r#"set -e
url=$1 app=$2
tmp=$(mktemp -d)
trap 'hdiutil detach "$tmp/mnt" -quiet -force 2>/dev/null || :; rm -rf "$tmp"' EXIT
mkdir "$tmp/mnt"
curl -fsSL --max-time 600 -o "$tmp/Copycat.dmg" "$url"
hdiutil attach -nobrowse -noautoopen -quiet -mountpoint "$tmp/mnt" "$tmp/Copycat.dmg"
new=
for a in "$tmp"/mnt/*.app; do [ -d "$a" ] && new=$a && break; done
[ -n "$new" ] || { echo "no app in the disk image" >&2; exit 1; }
rm -rf "$app.new"
ditto "$new" "$app.new"
hdiutil detach "$tmp/mnt" -quiet || : # a busy volume is the trap's to force
xattr -dr com.apple.quarantine "$app.new" 2>/dev/null || true
rm -rf "$app.old"
mv "$app" "$app.old"
mv "$app.new" "$app" || { mv "$app.old" "$app"; exit 1; }
rm -rf "$app.old"
"#;

/// Install the app, then the command-line tools if they are installed, so the
/// two keep sharing one daemon version. The latter's failure is not the app's.
fn install_app(url: &str, app: &Path) -> Result<(), String> {
    let out = Command::new("sh")
        .args(["-c", INSTALL_SCRIPT, "sh", url])
        .arg(app)
        .stdin(Stdio::null())
        .output()
        .map_err(|e| format!("could not run sh: {e}"))?;
    if !out.status.success() {
        let stderr = String::from_utf8_lossy(&out.stderr);
        let why = stderr.lines().rev().find(|l| !l.trim().is_empty()).unwrap_or("no details");
        return Err(format!("update failed: {}", why.trim()));
    }
    let cli = copycat_launcher::find_system_copycatd().and_then(|d| Some(d.parent()?.join("copycat")));
    if let Some(cli) = cli.filter(|c| c.is_file()) {
        let _ = Command::new(cli).arg("update").stdin(Stdio::null()).output();
    }
    Ok(())
}

const INSTALL_CMD: &str ="curl -fsSL https://raw.githubusercontent.com/samishal1998/copycat/main/install.sh | sh";

impl AppView {
    /// Put the command-line tools' install command on the clipboard.
    pub fn copy_install(&mut self, cx: &mut Context<Self>) {
        cx.write_to_clipboard(ClipboardItem::new_string(INSTALL_CMD.into()));
        self.toast("Copied. Paste it into Terminal and press Return.", cx);
    }
}

/// The state in plain words, for the rail, the floating panel, the dropdown
/// and the overlay: the mode, then what its count means ("READY", "10 saved";
/// "STACK", "9 left").
pub fn status_words(connected: bool, core: Option<&CoreStatus>) -> (String, String) {
    if !connected {
        return ("NOT RUNNING".into(), String::new());
    }
    match core.and_then(|c| c.session.as_ref()) {
        Some(s) if s.state == SessionState::Capturing => (s.mode.as_str().to_uppercase(), format!("{} copied", s.size)),
        Some(s) => (s.mode.as_str().to_uppercase(), format!("{} left", s.remaining)),
        None if core.is_some_and(|c| c.paused) => ("PAUSED".into(), "not saving copies".into()),
        None => ("READY".into(), core.map(|c| format!("{} saved", c.hot_items)).unwrap_or_default()),
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
        themed(false, window, cx);
        self.sync_form(window, cx);
        let screen = match self.screen {
            Screen::History => self.history_screen(cx).into_any_element(),
            Screen::Session => self.session_screen(cx).into_any_element(),
            Screen::Bindings => self.bindings_screen(cx).into_any_element(),
            Screen::Settings => self.settings_screen(cx).into_any_element(),
        };
        v_flex()
            .size_full()
            .bg(rgb(pal().surface))
            .text_color(rgb(pal().text))
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
                                    .child(screen),
                            ),
                        ),
                    ),
            )
    }
}

#[cfg(test)]
mod tests {
    use super::status_words;
    use copycat_core::{CoreStatus, DuplicatePolicy, SessionMode, SessionState, SessionSummary};

    #[test]
    fn the_status_says_what_the_count_means() {
        let session = |state| SessionSummary {
            id: 1,
            mode: SessionMode::Stack,
            state,
            duplicate_policy: DuplicatePolicy::Collapse,
            size: 10,
            cursor: 1,
            remaining: 9,
            next: None,
            delimiter: None,
            created_at: 0,
        };
        let mut core = CoreStatus {
            paused: false,
            hot_items: 10,
            hot_capacity: 100,
            duplicate_policy: DuplicatePolicy::Collapse,
            latest: None,
            session: None,
        };
        let words = |core: &CoreStatus| {
            let (mode, count) = status_words(true, Some(core));
            format!("{mode} · {count}")
        };
        assert_eq!(status_words(false, Some(&core)).0, "NOT RUNNING");
        assert_eq!(words(&core), "READY · 10 saved");
        core.paused = true;
        assert_eq!(words(&core), "PAUSED · not saving copies");
        core.session = Some(session(SessionState::Ready));
        assert_eq!(words(&core), "STACK · 9 left");
        core.session = Some(session(SessionState::Capturing));
        assert_eq!(words(&core), "STACK · 10 copied");
    }
}
