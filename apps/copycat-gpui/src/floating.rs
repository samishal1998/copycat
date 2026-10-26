//! The floating button and the panel it opens into, plus the windows' shared
//! plumbing (macOS only): the always-on-top pop-up windows, placing and moving
//! them natively, and `sync`, which makes the open windows match Settings.
//!
//! One window, two sizes, as in Tauri's `floating.html`: collapsed it is a
//! round button above every app; clicked it grows in place (bottom-right corner
//! fixed) into the panel. GPUI can resize a window but not move one, so the
//! frame is set on the NSWindow itself.

use std::collections::HashMap;

use copycat_core::{ClipId, ClipSummary, SessionState};
use copycat_protocol::{Action, ResultBody};
use gpui_kit::component::scroll::ScrollableElement as _;
use gpui_kit::component::tag::Tag;
use gpui_kit::component::{ActiveTheme as _, Disableable as _, Sizable as _, h_flex, v_flex};
use gpui_kit::prelude::*;
use gpui_kit::*;
use objc2::rc::Retained;
use objc2_app_kit::{NSEvent, NSView, NSWindow};
use objc2_foundation::{NSPoint, NSRect, NSSize};
use raw_window_handle::{HasWindowHandle, RawWindowHandle};

use crate::app::{Daemon, injects_paste, request};
use crate::brand::*;
use crate::overlay::Overlay;
use crate::screens::{DragGhost, DragRow, one_line};
use crate::settings::Settings;
use crate::tray;

/// The window's two sizes, transparent margin included (`lib.rs`).
const BUTTON: (f64, f64) = (64., 64.);
const PANEL: (f64, f64) = (340., 500.);
const OVERLAY: (f64, f64) = (300., 208.);

// ---- native placement ----------------------------------------------------

fn ns_window(window: &Window) -> Option<Retained<NSWindow>> {
    let RawWindowHandle::AppKit(handle) = HasWindowHandle::window_handle(window).ok()?.as_raw() else { return None };
    // SAFETY: GPUI's AppKit handle is its live content view.
    unsafe { handle.ns_view.cast::<NSView>().as_ref() }.window()
}

/// Pull `r` back onto the screen the window is on.
fn fit(ns: &NSWindow, r: &mut NSRect) {
    if let Some(screen) = ns.screen() {
        let v = screen.visibleFrame();
        r.origin.x = r.origin.x.min(v.origin.x + v.size.width - r.size.width).max(v.origin.x);
        r.origin.y = r.origin.y.min(v.origin.y + v.size.height - r.size.height).max(v.origin.y);
    }
}

/// Bottom right of the window's screen, or at `anchor` (the bottom-right
/// corner it was last left at).
fn park_bottom_right(window: &Window, anchor: Option<(f64, f64)>) {
    let Some(ns) = ns_window(window) else { return };
    // The window is mostly transparent margin; the shadow is drawn by the view.
    ns.setHasShadow(false);
    let mut r = ns.frame();
    let (right, bottom) = match (anchor, ns.screen()) {
        (Some(anchor), _) => anchor,
        (None, Some(screen)) => {
            let v = screen.visibleFrame();
            (v.origin.x + v.size.width - 24., v.origin.y + 24.)
        }
        _ => return,
    };
    r.origin = NSPoint::new(right - r.size.width, bottom);
    fit(&ns, &mut r);
    ns.setFrameOrigin(r.origin);
}

fn park_top_right(window: &Window) {
    let Some(ns) = ns_window(window) else { return };
    ns.setHasShadow(false);
    let Some(screen) = ns.screen() else { return };
    let (v, f) = (screen.visibleFrame(), ns.frame());
    ns.setFrameOrigin(NSPoint::new(
        v.origin.x + v.size.width - f.size.width - 16.,
        v.origin.y + v.size.height - f.size.height - 16.,
    ));
}

/// Resize with the bottom-right corner fixed, kept on screen. Returns the
/// corner, for remembering.
fn resize(window: &Window, (w, h): (f64, f64)) -> Option<(f64, f64)> {
    let ns = ns_window(window)?;
    let f = ns.frame();
    let mut r = NSRect::new(NSPoint::new(f.origin.x + f.size.width - w, f.origin.y), NSSize::new(w, h));
    fit(&ns, &mut r);
    // display: NO: a synchronous draw here runs inside GPUI's borrow and does nothing.
    ns.setFrame_display(r, false);
    Some((r.origin.x + w, r.origin.y))
}

fn corner(window: &Window) -> Option<(f64, f64)> {
    let f = ns_window(window)?.frame();
    Some((f.origin.x + f.size.width, f.origin.y))
}

/// Click versus drag, as `dragToMove` in `floating.js`: a press that travels
/// 4pt moves the window with the pointer, anything less is a click. The pointer
/// is read in screen coordinates, so moving the window under it cannot skew
/// the next delta.
#[derive(Default)]
pub struct Drag {
    start: Option<(NSPoint, NSPoint)>,
    /// Whether the last press became a drag; read by the click handler.
    pub moved: bool,
}

impl Drag {
    pub fn begin(&mut self, window: &Window) {
        self.start = ns_window(window).map(|ns| (NSEvent::mouseLocation(), ns.frame().origin));
        self.moved = false;
    }

    pub fn drag(&mut self, window: &Window) {
        let (Some((from, origin)), Some(ns)) = (self.start, ns_window(window)) else { return };
        let now = NSEvent::mouseLocation();
        let (dx, dy) = (now.x - from.x, now.y - from.y);
        if !self.moved && dx.hypot(dy) < 4. {
            return;
        }
        self.moved = true;
        ns.setFrameOrigin(NSPoint::new(origin.x + dx, origin.y + dy));
    }

    /// Whether this press moved the window.
    pub fn end(&mut self) -> bool {
        self.start = None;
        self.moved
    }
}

/// Mouse handlers that make an element move its window.
pub fn draggable<T: InteractiveElement, V: 'static>(
    el: T,
    cx: &Context<V>,
    drag: fn(&mut V) -> &mut Drag,
    moved: fn(&mut V, &mut Window, &mut Context<V>),
) -> T {
    el.on_mouse_down(MouseButton::Left, cx.listener(move |v, _, w, _| drag(v).begin(w)))
        .on_mouse_move(cx.listener(move |v, e: &MouseMoveEvent, w, _| {
            if e.dragging() {
                drag(v).drag(w)
            }
        }))
        .on_mouse_up(MouseButton::Left, cx.listener(move |v, _, w, cx| {
            if drag(v).end() {
                moved(v, w, cx);
            }
        }))
}

// ---- the open windows ----------------------------------------------------

#[derive(Default)]
struct Wins {
    /// Floating windows by slot: 0 is the primary display's, 1.. the others.
    floats: Vec<(usize, AnyWindowHandle)>,
    debug: Option<AnyWindowHandle>,
    /// Where each slot's button was last left, as its bottom-right corner, so a
    /// button toggled off and on comes back where it was dragged to.
    anchors: HashMap<usize, (f64, f64)>,
}

impl Global for Wins {}

/// Open the windows Settings asks for, and keep them in step from then on.
pub fn start(cx: &mut App) {
    cx.set_global(Wins::default());
    cx.observe(&Settings::get(cx), |_, cx| sync(cx)).detach();
    sync(cx);
}

/// `gpui_kit::open_window` with a see-through `Root`: the stock one paints the
/// theme background over the whole window, which is a square behind the round
/// button and the panel's corners.
fn open_popup<V: Render>(
    options: WindowOptions,
    cx: &mut App,
    build: impl FnOnce(&mut Window, &mut App) -> Entity<V>,
) -> Result<(AnyWindowHandle, Entity<V>)> {
    let mut built = None;
    let window = cx.open_window(options, |window, cx| {
        let view = build(window, cx);
        built = Some(view.clone());
        cx.new(|cx| gpui_kit::base::Root::new(view, window, cx).bg(transparent_black()))
    })?;
    Ok((window.into(), built.expect("open_window ran its build closure")))
}

fn popup(display: Option<DisplayId>, (w, h): (f64, f64)) -> WindowOptions {
    WindowOptions {
        window_bounds: Some(WindowBounds::Windowed(Bounds::new(
            point(px(0.), px(0.)),
            size(px(w as f32), px(h as f32)),
        ))),
        display_id: display,
        titlebar: None,
        // A non-activating pop-up panel on every Space, fullscreen ones too.
        kind: WindowKind::PopUp,
        window_background: WindowBackgroundAppearance::Transparent,
        // Never takes focus from the app the user is in.
        focus: false,
        // Never key, so GPUI would cap it at 30 fps.
        inactive_frame_interval: None,
        show: true,
        is_movable: false,
        is_resizable: false,
        is_minimizable: false,
        ..Default::default()
    }
}

/// Make the open windows match the settings: one button per wanted display
/// (new ones are parked bottom right), the debug overlay on or off.
// ponytail: displays are read when settings change or at launch; plugging one
// in later needs a re-toggle, as in the Tauri app.
fn sync(cx: &mut App) {
    let settings = Settings::get(cx).read(cx).clone();
    let primary = cx.primary_display().map(|d| d.id());
    let mut slots = Vec::new();
    if settings.floating {
        slots.push((0, primary));
        if settings.every_screen {
            let others = cx.displays().into_iter().map(|d| d.id()).filter(|id| Some(*id) != primary);
            slots.extend(others.enumerate().map(|(n, id)| (n + 1, Some(id))));
        }
    }

    let mut floats = std::mem::take(&mut cx.global_mut::<Wins>().floats);
    floats.retain(|(n, handle)| {
        let keep = slots.iter().any(|(slot, _)| slot == n) && handle.update(cx, |_, _, _| ()).is_ok();
        if !keep {
            let _ = handle.update(cx, |_, window, _| window.remove_window());
        }
        keep
    });
    for (n, display) in slots {
        if floats.iter().any(|(slot, _)| *slot == n) {
            continue;
        }
        let expanded = n == 0 && std::env::var("COPYCAT_GPUI_FLOATING").as_deref() == Ok("panel");
        let opened = open_popup(popup(display, if expanded { PANEL } else { BUTTON }), cx, |window, cx| {
            cx.new(|cx| Floating::new(n, expanded, window, cx))
        });
        if let Ok((handle, _)) = opened {
            let anchor = cx.global::<Wins>().anchors.get(&n).copied();
            let _ = handle.update(cx, |_, window, _| park_bottom_right(window, anchor));
            floats.push((n, handle));
        }
    }
    cx.global_mut::<Wins>().floats = floats;

    let wanted = settings.debug || std::env::var_os("COPYCAT_GPUI_DEBUG").is_some();
    let open = cx.global_mut::<Wins>().debug.take();
    let open = open.filter(|handle| {
        let alive = handle.update(cx, |_, _, _| ()).is_ok();
        if alive && !wanted {
            let _ = handle.update(cx, |_, window, _| window.remove_window());
        }
        alive && wanted
    });
    cx.global_mut::<Wins>().debug = open.or_else(|| {
        if !wanted {
            return None;
        }
        let (handle, _) = open_popup(popup(primary, OVERLAY), cx, |_, cx| cx.new(Overlay::new)).ok()?;
        let _ = handle.update(cx, |_, window, _| park_top_right(window));
        Some(handle)
    });
}

// ---- the view ------------------------------------------------------------

struct Floating {
    slot: usize,
    daemon: Entity<Daemon>,
    expanded: bool,
    drag: Drag,
    items: Vec<ClipSummary>,
    /// Which session `items` came from; a reply for an ended one is dropped.
    items_session: Option<u64>,
    items_loading: bool,
    msg: Option<SharedString>,
    msg_gen: u64,
    _sub: Subscription,
}

impl Floating {
    fn new(slot: usize, expanded: bool, _: &mut Window, cx: &mut Context<Self>) -> Self {
        let daemon = Daemon::get(cx);
        let mut this = Floating {
            slot,
            expanded,
            drag: Drag::default(),
            items: Vec::new(),
            items_session: None,
            items_loading: false,
            msg: None,
            msg_gen: 0,
            _sub: cx.observe(&daemon, |this, _, cx| this.on_daemon(cx)),
            daemon,
        };
        this.on_daemon(cx);
        this
    }

    fn on_daemon(&mut self, cx: &mut Context<Self>) {
        if self.expanded {
            self.load_items(cx);
        }
        cx.notify();
    }

    fn session_id(&self, cx: &App) -> Option<u64> {
        self.daemon.read(cx).status.as_ref()?.core.session.as_ref().map(|s| s.id)
    }

    fn load_items(&mut self, cx: &mut Context<Self>) {
        let Some(session) = self.session_id(cx) else {
            self.items.clear();
            self.items_session = None;
            return;
        };
        if self.items_loading {
            return;
        }
        self.items_loading = true;
        request(Action::SessionItems, cx, move |this, reply, cx| {
            this.items_loading = false;
            if let Ok(ResultBody::Clips { clips, .. }) = reply
                && this.session_id(cx) == Some(session)
            {
                this.items = clips;
                this.items_session = Some(session);
            }
        });
    }

    fn set_expanded(&mut self, on: bool, window: &mut Window, cx: &mut Context<Self>) {
        if on == self.expanded {
            return;
        }
        // Grow before drawing the panel into the window; shrink after.
        if let Some(corner) = resize(window, if on { PANEL } else { BUTTON }) {
            cx.global_mut::<Wins>().anchors.insert(self.slot, corner);
            // AppKit's resize callback is dropped while this update holds the
            // app, so tell GPUI the bounds changed once the update ends.
            window.defer(cx, |window, cx| window.bounds_changed(cx));
        }
        self.expanded = on;
        self.on_daemon(cx);
    }

    /// After a drag, remember where the button now is.
    fn moved(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if let Some(corner) = corner(window) {
            cx.global_mut::<Wins>().anchors.insert(self.slot, corner);
        }
    }

    fn say(&mut self, text: impl Into<SharedString>, cx: &mut Context<Self>) {
        self.msg = Some(text.into());
        self.msg_gen += 1;
        let generation = self.msg_gen;
        cx.spawn(async move |this, cx| {
            cx.background_executor().timer(std::time::Duration::from_millis(3200)).await;
            let _ = this.update(cx, |this, cx| {
                if this.msg_gen == generation {
                    this.msg = None;
                    cx.notify();
                }
            });
        })
        .detach();
        cx.notify();
    }

    fn act(&mut self, action: Action, label: &'static str, cx: &mut Context<Self>) {
        // Not while offline: nothing would be pasted.
        if injects_paste(&action) && self.daemon.read(cx).connected {
            tray::deactivate();
        }
        request(action, cx, move |this, reply, cx| match reply {
            Ok(ResultBody::Pasted { preview, .. }) => this.say(format!("pasted {}", one_line(&preview, 30)), cx),
            Ok(ResultBody::SessionStarted(_)) => this.say(format!("{} started", label.to_lowercase()), cx),
            Ok(_) => this.say("done", cx),
            Err(error) => this.say(error.message, cx),
        });
    }

    fn move_item(&mut self, from: usize, id: ClipId, to: usize, cx: &mut Context<Self>) {
        let cursor = self.daemon.read(cx).status.as_ref().and_then(|s| s.core.session.as_ref()).map_or(0, |s| s.cursor);
        let to = to.min(self.items.len().saturating_sub(1)).max(cursor);
        if from != to {
            request(Action::SessionMove { from, to, id }, cx, |this, _, cx| {
                this.items_loading = false;
                this.load_items(cx);
            });
        }
    }

    fn bubble(&self, cx: &mut Context<Self>) -> Div {
        let daemon = self.daemon.read(cx);
        let core = daemon.status.as_ref().map(|s| &s.core);
        let session = core.and_then(|c| c.session.as_ref());
        let paused = core.is_some_and(|c| c.paused);
        // A live mode rings the button; the badge's letter names which.
        let ring = if session.is_some() { ACTIVE } else if paused { GOLD } else { LINE };
        let badge = session.map(|s| {
            let n = if s.state == SessionState::Capturing { s.size } else { s.remaining };
            format!("{}{n}", s.mode.as_str()[..1].to_uppercase())
        });
        let shadow = vec![BoxShadow {
            color: rgba(0x000000D9).into(),
            offset: point(px(0.), px(8.)),
            blur_radius: px(22.),
            spread_radius: px(-8.),
            inset: false,
        }];
        let bubble = div()
            .id("bubble")
            .relative()
            .size(px(48.))
            .flex()
            .items_center()
            .justify_center()
            .rounded_full()
            .border_2()
            .border_color(rgb(ring))
            .bg(rgba(0x1C1F23F5))
            .shadow(shadow)
            .cursor_pointer()
            .when(!daemon.connected, |this| this.opacity(0.55))
            .when(ring == LINE, |this| this.hover(|s| s.border_color(rgb(FAINT))))
            .child(mark(32.))
            .children(badge.map(|text| {
                Tag::primary().rounded_full().small().absolute().top(px(-5.)).right(px(-7.)).child(text)
            }));
        let bubble = draggable(bubble, cx, |this| &mut this.drag, Self::moved)
            .on_click(cx.listener(|this, _, window, cx| {
                if !this.drag.moved {
                    this.set_expanded(true, window, cx);
                }
            }));
        div().size_full().p(px(8.)).child(bubble)
    }

    fn panel(&self, cx: &mut Context<Self>) -> Div {
        let daemon = self.daemon.read(cx);
        let connected = daemon.connected;
        let core = daemon.status.as_ref().map(|s| &s.core);
        let session = core.and_then(|c| c.session.as_ref());
        let paused = core.is_some_and(|c| c.paused);
        let (mode, size) = match (connected, session) {
            (false, _) => ("OFFLINE".to_string(), String::new()),
            (_, Some(s)) => (
                s.mode.as_str().to_uppercase(),
                if s.state == SessionState::Capturing {
                    format!("capturing · {}", s.size)
                } else {
                    format!("{}/{} · {} left", s.cursor, s.size, s.remaining)
                },
            ),
            (_, None) => (
                if paused { "PAUSED" } else { "NORMAL" }.to_string(),
                core.map(|c| format!("{} clips", c.hot_items)).unwrap_or_default(),
            ),
        };
        let cursor = session.map_or(0, |s| s.cursor);

        let header = h_flex()
            .items_center()
            .gap(px(9.))
            .pl(px(13.))
            .pr(px(10.))
            .py(px(10.))
            .border_b_1()
            .border_color(rgb(LINE))
            .cursor_grab()
            .child(mono(12.5).font_weight(FontWeight::BOLD).child(spaced(&mode, 0.75)))
            .child(mono(11.).text_color(rgb(FAINT)).whitespace_nowrap().child(size))
            .child(div().flex_1())
            .child(dot(connected, false))
            .child(
                icon_button("collapse", IconName::Minus)
                    .tooltip("Collapse")
                    .on_click(cx.listener(|this, _, window, cx| this.set_expanded(false, window, cx))),
            );
        let header = draggable(header.id("grab"), cx, |this| &mut this.drag, Self::moved);

        let body = v_flex().id("items").flex_1().min_h_0().overflow_y_scrollbar().px(px(12.)).py(px(10.)).child(
            match session {
                None => div()
                    .m(px(2.))
                    .text_size(px(12.5))
                    .line_height(px(19.))
                    .text_color(rgb(MUTED))
                    .child("No mode active. Start one below — then your own ⌘V pastes the next item.")
                    .into_any_element(),
                Some(s) => {
                    let items = if self.items_session == Some(s.id) { &self.items[..] } else { &[] };
                    v_flex()
                        .gap(px(5.))
                        .children(items.iter().enumerate().map(|(ix, clip)| self.row(ix, clip, cursor, cx)))
                        .into_any_element()
                }
            },
        );

        // Only what the current mode can do, as `render` in floating.js.
        let mode_is = |name: &str| session.is_some_and(|s| s.mode.as_str() == name);
        let capturing_queue =
            session.is_some_and(|s| s.mode.as_str() == "queue" && s.state == SessionState::Capturing);
        let act = |id: &'static str, label: &'static str, kind: Kind, off: bool, action: Action| {
            button(id, label, kind, true)
                .flex_1()
                .disabled(!connected || off)
                .on_click(cx.listener(move |this, _, _, cx| this.act(action.clone(), label, cx)))
        };
        let actions = v_flex()
            .gap(px(6.))
            .px(px(12.))
            .py(px(10.))
            .border_t_1()
            .border_color(rgb(LINE))
            .child(
                h_flex()
                    .gap(px(6.))
                    .child(act("paste-mode", "Paste next", Kind::Primary, session.is_none(), Action::PasteMode { inject: true }))
                    .child(act("group-paste", "Paste group", Kind::Plain, !mode_is("group"), Action::GroupPaste)),
            )
            .child(
                h_flex()
                    .gap(px(6.))
                    .child(act("stack-start", "Stack", Kind::Plain, false, Action::StackStart { duplicates: None }))
                    .child(act("queue-capture", "Queue", Kind::Plain, false, Action::QueueCapture { duplicates: None }))
                    .child(act(
                        "group-capture",
                        "Group",
                        Kind::Plain,
                        false,
                        Action::GroupCapture { delimiter: None, duplicates: None },
                    ))
                    .child(act("queue-seal", "Seal", Kind::Plain, !capturing_queue, Action::QueueSeal)),
            )
            .child(
                h_flex()
                    .gap(px(6.))
                    .child(act("session-reset", "Reset", Kind::Plain, session.is_none(), Action::SessionReset))
                    .child(act("session-stop", "Stop", Kind::Danger, session.is_none(), Action::SessionStop)),
            );

        let footer = h_flex()
            .items_center()
            .gap(px(8.))
            .px(px(12.))
            .pb(px(10.))
            .min_h(px(30.))
            .child(mono(11.).flex_1().min_w_0().truncate().text_color(rgb(MUTED)).children(self.msg.clone()))
            .child(ghost("open-main", "Open Copycat").xsmall().on_click(|_, _, cx| tray::show_main(cx)));

        let shadow = vec![BoxShadow {
            color: rgba(0x000000D9).into(),
            offset: point(px(0.), px(18.)),
            blur_radius: px(48.),
            spread_radius: px(-18.),
            inset: false,
        }];
        div().size_full().p(px(8.)).child(
            v_flex()
                .size_full()
                .overflow_hidden()
                .rounded(px(14.))
                .border_1()
                .border_color(cx.theme().border)
                .bg(cx.theme().popover)
                .shadow(shadow)
                .child(header)
                .child(body)
                .child(actions)
                .child(footer),
        )
    }

    /// One `.sl-row` of the list, as the Session screen draws it.
    fn row(&self, ix: usize, clip: &ClipSummary, cursor: usize, cx: &Context<Self>) -> AnyElement {
        let last = self.items.len() - 1;
        let (done, next) = (ix < cursor, ix == cursor);
        let mover = |id: &'static str, icon: IconName, to: usize, disabled: bool| {
            let clip_id = clip.id;
            icon_button((id, ix), icon)
                .xsmall()
                .disabled(disabled)
                .on_click(cx.listener(move |this, _, _, cx| this.move_item(ix, clip_id, to, cx)))
        };
        let tag = (done || next).then(|| {
            let tag = if next { Tag::primary().outline() } else { Tag::secondary() };
            tag.small().child(if next { "NEXT" } else { "PASTED" })
        });
        let (clip_id, preview) = (clip.id, SharedString::from(one_line(&clip.preview, 160)));
        let marker = |side: f32| {
            vec![BoxShadow {
                color: rgb(ACTIVE).into(),
                offset: point(px(0.), px(side)),
                blur_radius: px(0.),
                spread_radius: px(0.),
                inset: false,
            }]
        };
        let row = mono(12.)
            .id(("sl-row", ix))
            .flex()
            .items_center()
            .gap(px(8.))
            .pl(px(10.))
            .pr(px(8.))
            .py(px(6.))
            .bg(rgb(RAISED))
            .when(done, |this| this.opacity(0.5))
            .when(!done, |this| {
                let ghost = preview.clone();
                this.cursor_grab()
                    .on_drag(DragRow { ix, id: clip_id }, move |_, _, _, cx| {
                        let text = ghost.clone();
                        cx.new(|_| DragGhost(text))
                    })
                    .drag_over::<DragRow>(move |style, from, _, _| {
                        if from.ix == ix { style } else { style.shadow(marker(if from.ix > ix { -2. } else { 2. })) }
                    })
                    .on_drop(cx.listener(move |this, from: &DragRow, _, cx| this.move_item(from.ix, from.id, ix, cx)))
            })
            .child(div().w(px(12.)).flex_none().text_size(px(12.)).text_color(rgb(FAINT)).child(if done { "" } else { "⠿" }))
            .child(div().w(px(20.)).flex_none().text_right().text_size(px(10.5)).text_color(rgb(FAINT)).child((ix + 1).to_string()))
            .child(div().flex_1().min_w_0().truncate().child(preview.clone()))
            .children(tag)
            .when(!done, |this| {
                this.child(
                    h_flex()
                        .gap(px(2.))
                        .child(mover("up", IconName::ChevronUp, ix.saturating_sub(1).max(cursor), ix == cursor))
                        .child(mover("down", IconName::ChevronDown, (ix + 1).min(last), ix == last)),
                )
            });
        if next {
            accent_edge(row.bg(rgb(RAISED)), rgba(ACTIVE_LINE), 8.).into_any_element()
        } else {
            row.rounded(px(8.)).border_1().border_color(rgb(LINE)).into_any_element()
        }
    }
}

impl Render for Floating {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        if self.expanded { self.panel(cx) } else { self.bubble(cx) }
    }
}
