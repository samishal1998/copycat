//! The floating button and the panel it opens into, plus the windows' shared
//! plumbing (macOS only): the always-on-top pop-up windows, placing and moving
//! them natively, and `sync`, which makes the open windows match Settings.
//!
//! One window, two sizes, as in Tauri's `floating.html`: collapsed it is a
//! round button above every app; clicked it grows in place (bottom-right corner
//! fixed) into the panel. GPUI can resize a window but not move one, so the
//! frame is set on the NSWindow itself.

use std::collections::HashMap;
use std::time::{Duration, Instant};

use copycat_core::{ClipId, ClipSummary, SessionMode, SessionState};
use copycat_protocol::{Action, DebugEvent, ResultBody};
use gpui_kit::component::scroll::ScrollableElement as _;
use gpui_kit::component::tag::Tag;
use gpui_kit::component::{ActiveTheme as _, Selectable as _, Sizable as _, h_flex, v_flex};
use gpui_kit::prelude::*;
use gpui_kit::*;
// At the floating button's size (Settings); window sizes are scaled by hand.
use crate::brand::px;
use objc2::MainThreadMarker;
use objc2::rc::Retained;
use objc2_app_kit::{NSEvent, NSEventMask, NSEventModifierFlags, NSScreen, NSView, NSWindow, NSWindowStyleMask};
use objc2_foundation::{NSPoint, NSRect, NSSize};
use raw_window_handle::{HasWindowHandle, RawWindowHandle};

use crate::app::{Daemon, history_mark, injects_paste, request, status_words};
use crate::brand::*;
use crate::mascot::Mascot;
use crate::overlay::Overlay;
use crate::screens::{DragGhost, DragRow, age, now_ms, one_line};
use crate::settings::Settings;
use crate::tray;

/// The window's two sizes at 100%, transparent margin included (`lib.rs`).
const BUTTON: (f64, f64) = (64., 64.);
/// The button as the mascot (Settings → mascot): its cell plus the margin.
const MASCOT: (f64, f64) = (crate::mascot::CELL as f64 + 16., crate::mascot::CELL as f64 + 16.);
const PANEL: (f64, f64) = (340., 500.);
/// The panel out of the mascot: the panel to the cat's left, their margins
/// overlapping, the cat staying where it was.
const PANEL_MASCOT: (f64, f64) = (PANEL.0 + MASCOT.0 - 8., PANEL.1);
const OVERLAY: (f64, f64) = (300., 208.);
/// The leader bubble's room beside the button, added to the window only while
/// it shows: the window's margin takes clicks from the apps under it.
const TIP: f64 = 150.;
/// Clears a bubble whose sequence end was missed (a daemon restart). Longer
/// than the longest per-key leader timeout, 30s, so a slow sequence keeps it.
const TIP_TIMEOUT: Duration = Duration::from_secs(31);
/// How many recent clips the panel lists outside a session.
const HISTORY: usize = 30;
/// A drag let go without Ctrl throws the button to a corner: the pointer's
/// speed is read over its last `FLICK` seconds, and the button coasts on at
/// it for `COAST` seconds to pick the screen. Along an axis it was thrown
/// faster than `FLING` pt/s, it goes to the side it was thrown toward. It
/// lands `INSET` from the corner, as `park_bottom_right` parks it.
const FLICK: f64 = 0.08;
const COAST: f64 = 0.3;
const FLING: f64 = 400.;
const INSET: f64 = 24.;

// ---- native placement ----------------------------------------------------

fn ns_window(window: &Window) -> Option<Retained<NSWindow>> {
    let RawWindowHandle::AppKit(handle) = HasWindowHandle::window_handle(window).ok()?.as_raw() else { return None };
    // SAFETY: GPUI's AppKit handle is its live content view.
    unsafe { handle.ns_view.cast::<NSView>().as_ref() }.window()
}

/// Pull `r` back onto the screen the window is on.
fn fit(ns: &NSWindow, r: &mut NSRect) {
    if let Some(screen) = ns.screen() {
        clamp(r, screen.visibleFrame());
    }
}

/// Pull `r` back inside `v`.
fn clamp(r: &mut NSRect, v: NSRect) {
    r.origin.x = r.origin.x.min(v.origin.x + v.size.width - r.size.width).max(v.origin.x);
    r.origin.y = r.origin.y.min(v.origin.y + v.size.height - r.size.height).max(v.origin.y);
}

/// Which of `frames` holds `p`. No flip needed: `NSEvent::mouseLocation` and
/// `NSScreen::frame` share AppKit's global space, origin at the primary's
/// bottom left and y up, where a screen left of or above it just has a
/// negative x or a y past the primary's height. (GPUI's display bounds cannot
/// place a screen: on macOS their origin is always 0.) Columns run minX to
/// maxX - 1, as in CG; rows minY + 1 to maxY: AppKit puts a pointer on a
/// screen's top row at its maxY. Inclusive both ways, the left column of a
/// screen right of the primary would go to the primary.
fn screen_under(p: NSPoint, frames: &[NSRect]) -> Option<usize> {
    frames.iter().position(|f| {
        (f.origin.x..f.origin.x + f.size.width).contains(&p.x) && p.y > f.origin.y && p.y <= f.origin.y + f.size.height
    })
}

/// The visible frame of the screen holding `p`, if one does.
fn visible_frame_at(p: NSPoint) -> Option<NSRect> {
    let screens = NSScreen::screens(MainThreadMarker::new()?).to_vec();
    let frames: Vec<NSRect> = screens.iter().map(|s| s.frame()).collect();
    Some(screens.get(screen_under(p, &frames)?)?.visibleFrame())
}

/// GPUI opens a window without a title bar as Titled + FullSizeContentView,
/// and AppKit gives every titled window a rounded hairline frame and a shadow
/// it may recompute later (on reorder, resize, appearance change): the faint
/// square around the button. Borderless draws neither; NonactivatingPanel
/// keeps it a panel that never takes focus.
fn frameless(ns: &NSWindow) {
    ns.setStyleMask(NSWindowStyleMask::Borderless | NSWindowStyleMask::NonactivatingPanel);
    ns.setHasShadow(false);
}

/// Bottom right of the window's screen, or at `anchor` (the bottom-right
/// corner it was last left at).
fn park_bottom_right(window: &Window, anchor: Option<(f64, f64)>) {
    let Some(ns) = ns_window(window) else { return };
    // The window is mostly transparent margin; the shadow is drawn by the view.
    frameless(&ns);
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
    // Onto the anchor's screen, not the one the window opened on: a drag or a
    // summon may have left the slot's display. Its own screen if the anchor's
    // is gone. A point just inside the corner: the corner is on the edge.
    match anchor.and_then(|(x, y)| visible_frame_at(NSPoint::new(x - 1., y + 1.))) {
        Some(v) => clamp(&mut r, v),
        None => fit(&ns, &mut r),
    }
    ns.setFrameOrigin(r.origin);
}

fn park_top_right(window: &Window) {
    let Some(ns) = ns_window(window) else { return };
    frameless(&ns);
    let Some(screen) = ns.screen() else { return };
    let (v, f) = (screen.visibleFrame(), ns.frame());
    ns.setFrameOrigin(NSPoint::new(
        v.origin.x + v.size.width - f.size.width - 16.,
        v.origin.y + v.size.height - f.size.height - 16.,
    ));
}

/// Resize with the left edge fixed (`keep_left`) or the right one, and the top
/// edge (`keep_top`) or the bottom one, kept on screen.
fn resize(window: &Window, (w, h): (f64, f64), keep_left: bool, keep_top: bool) -> Option<()> {
    let ns = ns_window(window)?;
    let f = ns.frame();
    let x = if keep_left { f.origin.x } else { f.origin.x + f.size.width - w };
    let y = if keep_top { f.origin.y + f.size.height - h } else { f.origin.y };
    let mut r = NSRect::new(NSPoint::new(x, y), NSSize::new(w, h));
    fit(&ns, &mut r);
    // display: NO: a synchronous draw here runs inside GPUI's borrow and does nothing.
    ns.setFrame_display(r, false);
    frameless(&ns);
    Some(())
}

/// Scale a window by `ratio` (the main one, on a UI scale change), its
/// minimum with it, top-left corner fixed and kept on its screen. GPUI's
/// `resize` keeps the bottom-left corner, which can push the title bar off the
/// top, and cannot change the minimum, which would stop a shrink.
pub fn rescale(window: &mut Window, ratio: f64, cx: &mut App) {
    let Some(ns) = ns_window(window) else { return };
    let f = ns.frame();
    let (mut w, mut h) = (f.size.width * ratio, f.size.height * ratio);
    if let Some(v) = ns.screen().map(|s| s.visibleFrame()) {
        (w, h) = (w.min(v.size.width), h.min(v.size.height));
    }
    let min = ns.contentMinSize();
    ns.setContentMinSize(NSSize::new((min.width * ratio).min(w), (min.height * ratio).min(h)));
    let mut r = NSRect::new(NSPoint::new(f.origin.x, f.origin.y + f.size.height - h), NSSize::new(w, h));
    fit(&ns, &mut r);
    ns.setFrame_display(r, false);
    window.defer(cx, |window, cx| window.bounds_changed(cx));
}

/// Whether the window sits on the left half of its screen, and on the top half.
fn halves(window: &Window) -> (bool, bool) {
    let Some(ns) = ns_window(window) else { return (false, false) };
    let (f, Some(screen)) = (ns.frame(), ns.screen()) else { return (false, false) };
    let v = screen.visibleFrame();
    (
        f.origin.x + f.size.width / 2. < v.origin.x + v.size.width / 2.,
        f.origin.y + f.size.height / 2. > v.origin.y + v.size.height / 2.,
    )
}

/// Whether the pointer is over the window.
fn pointer_over(window: &Window) -> bool {
    let (Some(ns), p) = (ns_window(window), NSEvent::mouseLocation()) else { return false };
    let f = ns.frame();
    (f.origin.x..f.origin.x + f.size.width).contains(&p.x) && (f.origin.y..f.origin.y + f.size.height).contains(&p.y)
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
    /// The pointer over the last `FLICK` seconds, for a throw's speed.
    samples: Vec<(Instant, NSPoint)>,
    /// The window's flight to a corner after a throw; a new press stops it.
    pub throw: Option<Task<()>>,
}

impl Drag {
    pub fn begin(&mut self, window: &Window) {
        self.throw = None;
        self.start = ns_window(window).map(|ns| (NSEvent::mouseLocation(), ns.frame().origin));
        self.moved = false;
        self.samples = self.start.map(|(at, _)| (Instant::now(), at)).into_iter().collect();
    }

    pub fn drag(&mut self, window: &Window) {
        let (Some((from, origin)), Some(ns)) = (self.start, ns_window(window)) else { return };
        let now = NSEvent::mouseLocation();
        self.samples.retain(|(t, _)| t.elapsed().as_secs_f64() <= FLICK);
        self.samples.push((Instant::now(), now));
        let (dx, dy) = (now.x - from.x, now.y - from.y);
        if !self.moved && dx.hypot(dy) < 4. {
            return;
        }
        self.moved = true;
        ns.setFrameOrigin(NSPoint::new(origin.x + dx, origin.y + dy));
    }

    /// Whether this press moved the window. Once: an element sharing this
    /// `Drag` hears the same release as one outside it.
    pub fn end(&mut self) -> bool {
        self.start.take().is_some() && self.moved
    }

    /// The pointer's velocity as it let go, in pt/s.
    fn fling(&self) -> NSPoint {
        let now = Instant::now();
        let mut samples: Vec<_> = self.samples.iter().map(|(t, p)| (now.duration_since(*t).as_secs_f64(), *p)).collect();
        samples.push((0., NSEvent::mouseLocation()));
        velocity(&samples)
    }
}

/// Velocity in pt/s from `(age in seconds, point)` samples, oldest first,
/// over the last `FLICK` seconds: none if the pointer rested before letting go.
/// With no sample in that time but the release, from the last one before it:
/// a pointer that outran the element sent none since.
fn velocity(samples: &[(f64, NSPoint)]) -> NSPoint {
    let i = samples.iter().position(|(age, _)| *age <= FLICK).unwrap_or(0).min(samples.len().saturating_sub(2));
    match (samples.get(i), samples.last()) {
        (Some((a0, p0)), Some((a1, p1))) if a0 - a1 >= 0.01 => {
            NSPoint::new((p1.x - p0.x) / (a0 - a1), (p1.y - p0.y) / (a0 - a1))
        }
        _ => NSPoint::new(0., 0.),
    }
}

/// Where a throw lands: the button's box `b` thrown at `v` pt/s goes to a
/// corner of the screen it would coast onto (`own`, if none or if it must
/// `stay`), as the screen's index and the box's new origin.
fn landing(b: NSRect, v: NSPoint, frames: &[NSRect], visible: &[NSRect], own: usize, stay: bool) -> (usize, NSPoint) {
    let (w, h) = (b.size.width, b.size.height);
    let p = NSPoint::new(b.origin.x + w / 2. + v.x * COAST, b.origin.y + h / 2. + v.y * COAST);
    let n = screen_under(p, frames).filter(|_| !stay).unwrap_or(own);
    let s = visible[n];
    // Toward the low side: thrown that way, or coasting to its near half.
    let low = |speed: f64, at: f64, from: f64, len: f64| {
        if speed.abs() > FLING { speed < 0. } else { at < from + len / 2. }
    };
    let x = if low(v.x, p.x, s.origin.x, s.size.width) { s.origin.x + INSET } else { s.origin.x + s.size.width - INSET - w };
    let y = if low(v.y, p.y, s.origin.y, s.size.height) { s.origin.y + INSET } else { s.origin.y + s.size.height - INSET - h };
    (n, NSPoint::new(x, y))
}

/// Ease out with a slight overshoot (back, s = 0.5: under 1% past), so the
/// button settles into its corner. 0 at t = 0, 1 from t = 1 on.
fn ease_out_back(t: f64) -> f64 {
    let u = t.clamp(0., 1.) - 1.;
    1. + u * u * (1.5 * u + 0.5)
}

/// Mouse handlers that make an element move its window.
pub fn draggable<T: InteractiveElement, V: 'static>(
    el: T,
    cx: &Context<V>,
    drag: fn(&mut V) -> &mut Drag,
    moved: fn(&mut V, &mut Window, &mut Context<V>),
) -> T {
    let down = move |v: &mut V, _: &MouseDownEvent, w: &mut Window, _: &mut Context<V>| drag(v).begin(w);
    let up = move |v: &mut V, _: &MouseUpEvent, w: &mut Window, cx: &mut Context<V>| {
        if drag(v).end() {
            moved(v, w, cx);
        }
    };
    // GPUI hands a Ctrl-press and a Ctrl-release over as the right button's.
    el.on_mouse_down(MouseButton::Left, cx.listener(down))
        .on_mouse_down(MouseButton::Right, cx.listener(down))
        .on_mouse_move(cx.listener(move |v, e: &MouseMoveEvent, w, _| {
            if e.dragging() {
                drag(v).drag(w)
            }
        }))
        .on_mouse_up(MouseButton::Left, cx.listener(up))
        .on_mouse_up(MouseButton::Right, cx.listener(up))
        // A hard throw can outrun the element: the release lands outside it.
        .on_mouse_up_out(MouseButton::Left, cx.listener(up))
        .on_mouse_up_out(MouseButton::Right, cx.listener(up))
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
    /// The mascot and size settings the open buttons were built with.
    look: (bool, f32),
}

impl Global for Wins {}

/// The leader keys typed so far, for the buttons' bubble: None outside a
/// sequence, "" for the leader alone.
struct LeaderKeys(Entity<Option<String>>);

impl Global for LeaderKeys {}

/// What the bubble shows after one debug event, given what it showed before.
fn leader_after(shown: Option<String>, kind: &str, detail: &str) -> Option<String> {
    match kind {
        "leader" if detail.starts_with("leader pressed") => Some(String::new()),
        "leader-typing" => Some(detail.to_string()),
        // Timed out, cancelled, fired, or no binding.
        "leader" | "leader-key" => None,
        _ => shown,
    }
}

/// The daemon's newest debug events, from the tray's poll (`watch_ui`).
pub fn leader_events(events: &[DebugEvent], cx: &mut App) {
    let Some(LeaderKeys(keys)) = cx.try_global::<LeaderKeys>() else { return };
    if !events.iter().any(|e| e.kind.starts_with("leader")) {
        return;
    }
    let keys = keys.clone();
    let shown = events.iter().fold(keys.read(cx).clone(), |shown, e| leader_after(shown, &e.kind, &e.detail));
    keys.update(cx, |keys, cx| {
        *keys = shown;
        cx.notify();
    });
}

/// Open the windows Settings asks for, and keep them in step from then on.
pub fn start(cx: &mut App) {
    cx.set_global(Wins::default());
    let keys = cx.new(|_| None);
    cx.set_global(LeaderKeys(keys));
    cx.observe(&Settings::get(cx), |_, cx| sync(cx)).detach();
    sync(cx);
    watch_clicks(cx);
}

/// A click in another app closes the open panels, unless Settings or a pin
/// keeps them. A global monitor sees only other apps' events, which is what a
/// click away is; it needs no permission for mouse events.
fn watch_clicks(cx: &mut App) {
    let (tx, rx) = async_channel::unbounded();
    let block = block2::RcBlock::new(move |_: std::ptr::NonNull<NSEvent>| {
        let _ = tx.try_send(());
    });
    let mask = NSEventMask::LeftMouseDown | NSEventMask::RightMouseDown | NSEventMask::OtherMouseDown;
    // Kept for the app's life: nothing removes it.
    std::mem::forget(NSEvent::addGlobalMonitorForEventsMatchingMask_handler(mask, &block));
    cx.spawn(async move |cx| {
        while rx.recv().await.is_ok() {
            cx.update(|cx| {
                if !Settings::get(cx).read(cx).close_on_click_away {
                    return;
                }
                for (_, handle) in cx.global::<Wins>().floats.clone() {
                    let _ = handle.update(cx, |_, window, cx| {
                        let view = gpui_kit::base::Root::read(window, cx).view().clone();
                        if let Ok(view) = view.downcast::<Floating>() {
                            view.update(cx, |view, cx| view.auto_close(window, cx));
                        }
                    });
                }
            });
        }
    })
    .detach();
}

/// `gpui_kit::open_window` with a see-through `Root`: the stock one paints the
/// theme background over the whole window, which is a square behind the round
/// button and the panel's corners.
fn open_popup<V: Render>(
    options: WindowOptions,
    look: Look,
    cx: &mut App,
    build: impl FnOnce(&mut Window, &mut App) -> Entity<V>,
) -> Result<(AnyWindowHandle, Entity<V>)> {
    let mut built = None;
    let window = cx.open_window(options, |window, cx| {
        follow_system(window);
        let view = build(window, cx);
        built = Some(view.clone());
        next_window(look);
        cx.new(|cx| gpui_kit::base::Root::new(view, window, cx).bg(transparent_black()))
    })?;
    Ok((window.into(), built.expect("open_window ran its build closure")))
}

fn popup(display: Option<DisplayId>, (w, h): (f64, f64)) -> WindowOptions {
    WindowOptions {
        window_bounds: Some(WindowBounds::Windowed(Bounds::new(
            point(gpui_kit::px(0.), gpui_kit::px(0.)),
            size(gpui_kit::px(w as f32), gpui_kit::px(h as f32)),
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

/// `ui.show summon`: the floating button to the pointer, on whichever screen
/// that is, turned on first if it is off. With a button on every screen the
/// one already on the pointer's screen just comes to the front, and the others
/// stay where they are.
pub fn summon(cx: &mut App) {
    let Some(mtm) = MainThreadMarker::new() else { return };
    let screens = NSScreen::screens(mtm).to_vec();
    let at = NSEvent::mouseLocation();
    let frames: Vec<NSRect> = screens.iter().map(|s| s.frame()).collect();
    // Off every screen (in a gap between them): the primary.
    let Some(screen) = screens.get(screen_under(at, &frames).unwrap_or(0)) else { return };
    let settings = Settings::get(cx);
    if !settings.read(cx).floating {
        settings.update(cx, |s, cx| s.change(cx, |s| s.floating = true));
        // Now rather than when the change's observer runs: there must be a
        // button to move. The observer's own `sync` then finds nothing to do.
        // Only then: a `sync` would drop a button an earlier summon added.
        sync(cx);
    }
    let floats = cx.global::<Wins>().floats.clone();
    let handle = if settings.read(cx).every_screen {
        let on_screen = |handle: &AnyWindowHandle, cx: &mut App| {
            let ns = handle.update(cx, |_, window, _| ns_window(window)).ok().flatten();
            // By frame: AppKit may hand out a new NSScreen for the same display.
            ns.and_then(|ns| ns.screen()).is_some_and(|s| s.frame() == screen.frame())
        };
        if let Some((_, handle)) = floats.iter().find(|(_, handle)| on_screen(handle, cx)) {
            let _ = handle.update(cx, |_, window, _| {
                if let Some(ns) = ns_window(window) {
                    ns.orderFrontRegardless();
                }
            });
            return;
        }
        // A screen without one: its button was dragged off it, or it was
        // plugged in after `sync` last read the displays. A new slot; a
        // settings change drops it again if its screen already had a button.
        let n = floats.iter().map(|(n, _)| n + 1).max().unwrap_or(0);
        let wanted = settings.read(cx).clone();
        let Some(handle) = open_float(n, None, &wanted, cx) else { return };
        cx.global_mut::<Wins>().floats.push((n, handle));
        handle
    } else {
        let Some((_, handle)) = floats.iter().find(|(n, _)| *n == 0) else { return };
        *handle
    };
    let v = screen.visibleFrame();
    let _ = handle.update(cx, |_, window, cx| {
        let view = gpui_kit::base::Root::read(window, cx).view().clone();
        if let Ok(view) = view.downcast::<Floating>() {
            view.update(cx, |view, cx| view.summon(at, v, window, cx));
        }
    });
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

    // Toggling the mascot or resizing rebuilds the buttons, at their anchors,
    // in the new look.
    let look = (settings.mascot, settings.floating_scale);
    let restyled = std::mem::replace(&mut cx.global_mut::<Wins>().look, look) != look;
    let mut floats = std::mem::take(&mut cx.global_mut::<Wins>().floats);
    floats.retain(|(n, handle)| {
        let keep =
            !restyled && slots.iter().any(|(slot, _)| slot == n) && handle.update(cx, |_, _, _| ()).is_ok();
        if !keep {
            let _ = handle.update(cx, |_, window, _| window.remove_window());
        }
        keep
    });
    for (n, display) in slots {
        if floats.iter().any(|(slot, _)| *slot == n) {
            continue;
        }
        if let Some(handle) = open_float(n, display, &settings, cx) {
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
        let (handle, _) = open_popup(popup(primary, OVERLAY), Look::OVERLAY, cx, |_, cx| cx.new(Overlay::new)).ok()?;
        let _ = handle.update(cx, |_, window, _| park_top_right(window));
        Some(handle)
    });
}

/// Open slot `n`'s button, at its anchor or bottom right of its display.
fn open_float(n: usize, display: Option<DisplayId>, settings: &Settings, cx: &mut App) -> Option<AnyWindowHandle> {
    let expanded = n == 0 && std::env::var("COPYCAT_GPUI_FLOATING").as_deref() == Ok("panel");
    let size = match (expanded, settings.mascot) {
        (true, true) => PANEL_MASCOT,
        (true, false) => PANEL,
        (false, true) => MASCOT,
        (false, false) => BUTTON,
    };
    let s = settings.floating_scale as f64;
    let (handle, _) = open_popup(popup(display, (size.0 * s, size.1 * s)), Look::FLOATING, cx, |window, cx| {
        cx.new(|cx| Floating::new(n, expanded, window, cx))
    })
    .ok()?;
    let anchor = cx.global::<Wins>().anchors.get(&n).copied();
    let _ = handle.update(cx, |_, window, _| park_bottom_right(window, anchor));
    Some(handle)
}

// ---- the view ------------------------------------------------------------

struct Floating {
    slot: usize,
    daemon: Entity<Daemon>,
    /// The size setting the window was built with; `sync` rebuilds it on a change.
    scale: f64,
    expanded: bool,
    /// The panel opened to the cat's right (the cat is on the left half of
    /// the screen), so the cat is the window's left end.
    right: bool,
    /// The panel opened downward (the button is on the top half of the
    /// screen), so the button stays put and the window's top edge with it.
    down: bool,
    /// Kept open: Settings' auto-close is off for it until unpinned.
    pinned: bool,
    /// The window, to close the panel from a reply.
    handle: AnyWindowHandle,
    /// Closes the panel once the pointer has been away `close_after` seconds.
    _idle: Option<Task<()>>,
    drag: Drag,
    /// The animated cat drawn as the button, when Settings asks for it.
    mascot: Option<Entity<Mascot>>,
    items: Vec<ClipSummary>,
    /// Which session `items` came from; a reply for an ended one is dropped.
    items_session: Option<u64>,
    items_loading: bool,
    /// Recent history, listed outside a session.
    history: Vec<ClipSummary>,
    /// `history_mark` when `history` was last fetched.
    history_mark: Option<(Option<ClipId>, usize)>,
    msg: Option<SharedString>,
    msg_gen: u64,
    /// The leader bubble's keys, and whether it is on the button's right.
    tip: Option<(String, bool)>,
    _tip_timeout: Option<Task<()>>,
    _sub: Subscription,
    _leader: Subscription,
}

impl Floating {
    fn new(slot: usize, expanded: bool, window: &mut Window, cx: &mut Context<Self>) -> Self {
        let daemon = Daemon::get(cx);
        let keys = cx.global::<LeaderKeys>().0.clone();
        let mut this = Floating {
            slot,
            scale: Settings::get(cx).read(cx).floating_scale as f64,
            expanded,
            right: false,
            down: false,
            pinned: false,
            handle: Window::window_handle(window),
            _idle: None,
            drag: Drag::default(),
            mascot: Settings::get(cx).read(cx).mascot.then(|| cx.new(Mascot::new)),
            items: Vec::new(),
            items_session: None,
            items_loading: false,
            history: Vec::new(),
            history_mark: None,
            msg: None,
            msg_gen: 0,
            tip: None,
            _tip_timeout: None,
            _sub: cx.observe(&daemon, |this, _, cx| this.on_daemon(cx)),
            // Not over the open panel.
            _leader: cx.observe_in(&keys, window, |this, keys, window, cx| {
                let on = Settings::get(cx).read(cx).leader_bubble && !this.expanded;
                let typed = keys.read(cx).clone().filter(|_| on);
                this.show_tip(typed, window, cx);
            }),
            daemon,
        };
        this.on_daemon(cx);
        this
    }

    /// One of the window sizes above at this window's scale.
    fn sized(&self, (w, h): (f64, f64)) -> (f64, f64) {
        (w * self.scale, h * self.scale)
    }

    fn on_daemon(&mut self, cx: &mut Context<Self>) {
        if self.expanded {
            self.load_items(cx);
            // Refetch history only when the daemon says it may have changed,
            // as the dropdown does.
            let mark = history_mark(self.daemon.read(cx).status.as_ref());
            if self.session_id(cx).is_none() && mark.is_some() && mark != self.history_mark {
                self.history_mark = mark;
                self.load_history(cx);
            }
        }
        cx.notify();
    }

    fn load_history(&mut self, cx: &mut Context<Self>) {
        request(Action::HistoryList { limit: HISTORY, raw: false }, cx, |this, reply, _| {
            if let Ok(ResultBody::Clips { clips, .. }) = reply {
                this.history = clips;
            }
        });
    }

    /// Pin or promote a history clip, then refetch the list.
    fn edit_history(&mut self, action: Action, cx: &mut Context<Self>) {
        request(action, cx, |this, reply, cx| match reply {
            Ok(_) => this.load_history(cx),
            Err(error) => this.say(error.message, cx),
        });
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
        if on {
            self.show_tip(None, window, cx);
        }
        // Grow before drawing the panel into the window; shrink after.
        let size = match (on, self.mascot.is_some()) {
            (true, true) => PANEL_MASCOT,
            (true, false) => PANEL,
            (false, true) => MASCOT,
            (false, false) => BUTTON,
        };
        let size = self.sized(size);
        // The panel comes out of the cat on the side with room, and hangs
        // below a button on the top half rather than pushing it down.
        if on {
            let (left, top) = halves(window);
            self.right = self.mascot.is_some() && left;
            self.down = top;
            self._idle = Some(self.idle_close(window, cx));
        }
        self.expanded = on;
        if resize(window, size, self.right, self.down).is_some() {
            self.moved(window, cx);
            // AppKit's resize callback is dropped while this update holds the
            // app, so tell GPUI the bounds changed once the update ends.
            window.defer(cx, |window, cx| window.bounds_changed(cx));
        }
        // Back to the button: the cat greets it with the click sequence.
        if !on && let Some(mascot) = &self.mascot {
            mascot.update(cx, |mascot, cx| mascot.clicked(cx));
        }
        self.on_daemon(cx);
    }

    /// After a drag or resize, remember where the button now is, as its
    /// bottom-right corner.
    fn moved(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(f) = ns_window(window).map(|ns| ns.frame()) else { return };
        // Open downward, the button's bottom is a button's height under the top.
        let (_, h) = self.sized(if self.mascot.is_some() { MASCOT } else { BUTTON });
        let bottom = if self.expanded && self.down { f.origin.y + f.size.height - h } else { f.origin.y };
        cx.global_mut::<Wins>().anchors.insert(self.slot, (f.origin.x + self.button_right(f.size.width), bottom));
    }

    /// Close the panel if Settings lets it close by itself and no pin keeps it.
    fn auto_close(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.expanded && !self.pinned {
            self.set_expanded(false, window, cx);
        }
    }

    /// Close the open panel once the pointer has been away from it for
    /// Settings' `close_after`; ends when the panel closes.
    fn idle_close(&self, window: &mut Window, cx: &mut Context<Self>) -> Task<()> {
        cx.spawn_in(window, async move |this, cx| {
            let mut touched = Instant::now();
            loop {
                cx.background_executor().timer(Duration::from_millis(500)).await;
                let open = this.update_in(cx, |this, window, cx| {
                    let after = Settings::get(cx).read(cx).close_after;
                    if after == 0 || this.pinned || pointer_over(window) {
                        touched = Instant::now();
                    } else if touched.elapsed() >= Duration::from_secs(after) {
                        this.auto_close(window, cx);
                    }
                    this.expanded
                });
                if !matches!(open, Ok(true)) {
                    break;
                }
            }
        })
    }

    /// After a drag: let go holding Ctrl, the button stays where it was
    /// dropped; otherwise it flies to the corner it was thrown toward, its
    /// own screen's with a button on every screen, and is remembered there.
    fn dropped(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let (Some(mtm), Some(ns)) = (MainThreadMarker::new(), ns_window(window)) else { return };
        let screens = NSScreen::screens(mtm).to_vec();
        // An open panel is placed, not thrown: closing it keeps the bottom-right
        // corner, which would leave a thrown button mid-screen.
        if screens.is_empty() || self.expanded || NSEvent::modifierFlags_class().contains(NSEventModifierFlags::Control) {
            self.thrown(0., 0., cx);
            return self.moved(window, cx);
        }
        // The bubble would hold the button off the far corner.
        self.show_tip(None, window, cx);
        let frames: Vec<NSRect> = screens.iter().map(|s| s.frame()).collect();
        let visible: Vec<NSRect> = screens.iter().map(|s| s.visibleFrame()).collect();
        // The button's part of the window, as `summon` places it.
        let f = ns.frame();
        let (w, h) = self.sized(if self.mascot.is_some() { MASCOT } else { BUTTON });
        let left = self.button_right(f.size.width) - w;
        let b = NSRect::new(NSPoint::new(f.origin.x + left, f.origin.y), NSSize::new(w, h));
        // Off every screen (in a gap between them): the primary.
        let own = screen_under(NSPoint::new(b.origin.x + w / 2., b.origin.y + h / 2.), &frames).unwrap_or(0);
        let stay = Settings::get(cx).read(cx).every_screen;
        let (n, to) = landing(b, self.drag.fling(), &frames, &visible, own, stay);
        let mut r = NSRect::new(NSPoint::new(to.x - left, to.y), f.size);
        clamp(&mut r, visible[n]);
        let (from, to) = (f.origin, r.origin);
        self.thrown(to.x - from.x, to.y - from.y, cx);
        let secs = (0.18 + (to.x - from.x).hypot(to.y - from.y) / 4000.).min(0.4);
        let start = Instant::now();
        self.drag.throw = Some(cx.spawn_in(window, async move |this, cx| {
            loop {
                cx.background_executor().timer(Duration::from_millis(16)).await;
                let t = start.elapsed().as_secs_f64() / secs;
                let k = ease_out_back(t);
                let at = if t >= 1. { to } else { NSPoint::new(from.x + (to.x - from.x) * k, from.y + (to.y - from.y) * k) };
                let gone = this.update_in(cx, |this, window, cx| {
                    if let Some(ns) = ns_window(window) {
                        ns.setFrameOrigin(at);
                    }
                    if t >= 1. {
                        this.moved(window, cx);
                    }
                });
                if gone.is_err() || t >= 1. {
                    break;
                }
            }
        }));
    }

    /// Tell the cat it was let go and carried `dx`, `dy`.
    fn thrown(&self, dx: f64, dy: f64, cx: &mut Context<Self>) {
        if let Some(mascot) = &self.mascot {
            mascot.update(cx, |mascot, cx| mascot.thrown(dx, dy, cx));
        }
    }

    /// The button's right edge from the window's left, in a window `width`
    /// wide, whichever side the panel or the bubble is out on.
    fn button_right(&self, width: f64) -> f64 {
        if self.expanded && self.right {
            self.sized(MASCOT).0
        } else if let Some((_, true)) = self.tip {
            width - TIP * self.scale
        } else {
            width
        }
    }

    /// Centre the button on `at`, the window kept inside `v` (`fit` would
    /// clamp to the screen it is leaving), in front, and remembered there as
    /// a drag would.
    fn summon(&mut self, at: NSPoint, v: NSRect, window: &mut Window, cx: &mut Context<Self>) {
        let Some(ns) = ns_window(window) else { return };
        let (w, h) = self.sized(if self.mascot.is_some() { MASCOT } else { BUTTON });
        let mut r = ns.frame();
        r.origin = NSPoint::new(at.x + w / 2. - self.button_right(r.size.width), at.y - h / 2.);
        clamp(&mut r, v);
        ns.setFrameOrigin(r.origin);
        // Above the others without taking focus: the window never becomes key.
        ns.orderFrontRegardless();
        self.moved(window, cx);
    }

    /// Show the leader bubble with `typed`, or hide it. The window grows by
    /// `TIP` toward the screen's middle while it shows, and shrinks back on
    /// the same side.
    fn show_tip(&mut self, typed: Option<String>, window: &mut Window, cx: &mut Context<Self>) {
        let (w, h) = self.sized(if self.mascot.is_some() { MASCOT } else { BUTTON });
        match (typed, self.tip.take()) {
            (None, None) => return,
            (Some(typed), Some((_, right))) => self.tip = Some((typed, right)),
            (typed, was) => {
                let right = was.map_or_else(|| halves(window).0, |(_, right)| right);
                let size = if typed.is_some() { (w + TIP * self.scale, h) } else { (w, h) };
                self.tip = typed.map(|typed| (typed, right));
                if resize(window, size, right, false).is_some() {
                    window.defer(cx, |window, cx| window.bounds_changed(cx));
                }
            }
        }
        self._tip_timeout = self.tip.is_some().then(|| {
            cx.spawn_in(window, async move |this, cx| {
                cx.background_executor().timer(TIP_TIMEOUT).await;
                let _ = this.update_in(cx, |this, window, cx| this.show_tip(None, window, cx));
            })
        });
        cx.notify();
    }

    /// The leader keys typed so far, as a bubble beside the button:
    /// "leader › g p", or "leader ›…" before the first key.
    fn leader_tip(&self, typed: &str, cx: &Context<Self>) -> Div {
        let shadow = vec![BoxShadow {
            color: rgba(pal().shadow).into(),
            offset: point(px(0.), px(4.)),
            blur_radius: px(12.),
            spread_radius: px(-4.),
            inset: false,
        }];
        let keys = typed.chars().map(String::from).collect::<Vec<_>>().join(" ");
        mono(12.)
            .flex()
            .items_center()
            .gap(px(6.))
            .min_w_0()
            .px(px(9.))
            .py(px(5.))
            .rounded(px(8.))
            .border_1()
            .border_color(rgb(pal().line))
            .bg(cx.theme().popover)
            .shadow(shadow)
            .child(div().flex_none().text_color(rgb(pal().faint)).child(if typed.is_empty() { "Leader ›…" } else { "Leader ›" }))
            .when(!typed.is_empty(), |this| this.child(div().min_w_0().truncate().child(keys)))
    }

    fn say(&mut self, text: impl Into<SharedString>, cx: &mut Context<Self>) {
        self.msg = Some(text.into());
        self.msg_gen += 1;
        let generation = self.msg_gen;
        cx.spawn(async move |this, cx| {
            // Long enough to read a mode's how-to.
            cx.background_executor().timer(Duration::from_millis(5000)).await;
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

    /// Send `action`; the reply becomes a line that says what to do next.
    fn act(&mut self, action: Action, cx: &mut Context<Self>) {
        // Not while offline: nothing would be pasted.
        if injects_paste(&action) && self.daemon.read(cx).connected {
            tray::deactivate();
        }
        let sent = action.clone();
        request(action, cx, move |this, reply, cx| match (reply, sent) {
            (Ok(ResultBody::Pasted { preview, .. }), _) => {
                this.say(format!("pasted {}", one_line(&preview, 30)), cx);
                if Settings::get(cx).read(cx).close_on_paste {
                    // Not inside this update: closing needs the window.
                    let (handle, view) = (this.handle, cx.entity());
                    cx.defer(move |cx| {
                        let _ = handle.update(cx, |_, window, cx| view.update(cx, |this, cx| this.auto_close(window, cx)));
                    });
                }
            }
            (Ok(ResultBody::SessionStarted(s)), _) => this.say(started(s.session.mode), cx),
            (Ok(_), Action::SessionReset) => this.say("Back to the first item", cx),
            (Ok(_), Action::SessionStop) => this.say("Stopped. Your copies are still in History.", cx),
            (Ok(_), _) => this.say("done", cx),
            (Err(error), _) => this.say(error.message, cx),
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
        let ring = if session.is_some() { ACTIVE } else if paused { pal().warn } else { pal().line };
        let badge = session.map(|s| {
            let n = if s.state == SessionState::Capturing { s.size } else { s.remaining };
            format!("{}{n}", s.mode.as_str()[..1].to_uppercase())
        });
        let shadow = vec![BoxShadow {
            color: rgba(pal().shadow).into(),
            offset: point(px(0.), px(3.)),
            blur_radius: px(8.),
            spread_radius: px(-3.),
            inset: false,
        }];
        let bubble = match &self.mascot {
            // The cat is the button: no disc, no ring; it dims itself.
            Some(mascot) => div().id("bubble").relative().cursor_pointer().child(mascot.clone()).on_mouse_down(
                MouseButton::Left,
                // Open, the click collapses the panel, which plays it.
                cx.listener(|this, _, _, cx| {
                    if !this.expanded && let Some(mascot) = &this.mascot {
                        mascot.update(cx, |mascot, cx| mascot.clicked(cx));
                    }
                }),
            ),
            None => div()
                .id("bubble")
                .relative()
                .size(px(48.))
                .flex()
                .items_center()
                .justify_center()
                .rounded_full()
                .border_2()
                .border_color(rgb(ring))
                .bg(rgba(pal().surface << 8 | 0xF5))
                .shadow(shadow)
                .cursor_pointer()
                .when(!daemon.connected, |this| this.opacity(0.55))
                .when(ring == pal().line, |this| this.hover(|s| s.border_color(rgb(pal().faint))))
                .child(mark(32.)),
        };
        let bubble = bubble.children(badge.map(|text| {
                Tag::primary().rounded_full().small().absolute().top(px(-5.)).right(px(-7.)).child(text)
            }));
        let bubble = draggable(bubble, cx, |this| &mut this.drag, Self::dropped)
            // Picked up: the cat dangles while dragged.
            .on_mouse_move(cx.listener(|this, e: &MouseMoveEvent, _, cx| {
                if e.dragging() && this.drag.moved && let Some(mascot) = &this.mascot {
                    mascot.update(cx, |mascot, cx| mascot.hold(cx));
                }
            }))
            .on_click(cx.listener(|this, _, window, cx| {
                if !this.drag.moved {
                    this.set_expanded(!this.expanded, window, cx);
                }
            }));
        div().flex_none().p(px(8.)).child(bubble)
    }

    fn panel(&self, cx: &mut Context<Self>) -> Div {
        let daemon = self.daemon.read(cx);
        let connected = daemon.connected;
        let core = daemon.status.as_ref().map(|s| &s.core);
        let session = core.and_then(|c| c.session.as_ref());
        let (mode, size) = status_words(connected, core);
        let cursor = session.map_or(0, |s| s.cursor);

        let header = h_flex()
            .items_center()
            .gap(px(9.))
            .pl(px(13.))
            .pr(px(10.))
            .py(px(10.))
            .border_b_1()
            .border_color(rgb(pal().line))
            .cursor_grab()
            .child(mono(12.5).font_weight(FontWeight::BOLD).child(spaced(&mode, 0.75)))
            .child(mono(11.).text_color(rgb(pal().faint)).whitespace_nowrap().child(size))
            .child(div().flex_1())
            .child(dot(connected, false))
            .when(Settings::get(cx).read(cx).auto_closes(), |this| {
                let pinned = self.pinned;
                this.child(
                    icon_button("keep-open", if pinned { IconName::PinOff } else { IconName::Pin })
                        .selected(pinned)
                        .tooltip(if pinned { "Let it close by itself" } else { "Keep open" })
                        .on_click(cx.listener(|this, _, _, cx| {
                            this.pinned = !this.pinned;
                            cx.notify();
                        })),
                )
            })
            .child(
                icon_button("collapse", IconName::Minus)
                    .tooltip("Collapse")
                    .on_click(cx.listener(|this, _, window, cx| this.set_expanded(false, window, cx))),
            );
        let header = draggable(header.id("grab"), cx, |this| &mut this.drag, Self::dropped);

        let body = v_flex().id("items").flex_1().min_h_0().overflow_y_scrollbar().px(px(12.)).py(px(10.)).child(
            match session {
                None if !connected || self.history.is_empty() => div()
                    .m(px(2.))
                    .text_size(px(12.5))
                    .line_height(px(19.))
                    .text_color(rgb(pal().muted))
                    .child(if connected {
                        "Want to paste several things in a row? Pick how below."
                    } else {
                        "Copycat isn't running. Open Copycat to start it."
                    })
                    .into_any_element(),
                None => {
                    let now = now_ms();
                    v_flex()
                        .gap(px(5.))
                        .children(self.history.iter().enumerate().map(|(ix, clip)| self.history_row(ix, clip, now, cx)))
                        .into_any_element()
                }
                Some(s) => {
                    let items = if self.items_session == Some(s.id) { &self.items[..] } else { &[] };
                    v_flex()
                        .gap(px(5.))
                        .children(items.iter().enumerate().map(|(ix, clip)| self.row(ix, clip, cursor, cx)))
                        .into_any_element()
                }
            },
        );

        // Only what works now, in one row: the modes when none runs; in one,
        // paste (the one call that pastes next, pastes a group or seals a
        // queue), start over, stop. Offline, nothing: the body says why.
        let act = |id: &'static str, label: &'static str, kind: Kind, action: Action| {
            button(id, label, kind, true).flex_1().on_click(cx.listener(move |this, _, _, cx| this.act(action.clone(), cx)))
        };
        let actions = h_flex()
            .gap(px(6.))
            .px(px(12.))
            .py(px(10.))
            .border_t_1()
            .border_color(rgb(pal().line))
            .map(|row| match session {
                None => row.children(starts().into_iter().map(|(id, label, tip, action)| act(id, label, Kind::Plain, action).tooltip(tip))),
                Some(s) => {
                    let capturing = s.state == SessionState::Capturing;
                    let paste = match (s.mode, capturing) {
                        (SessionMode::Group, _) => "Paste all",
                        (SessionMode::Queue, true) => "Start pasting",
                        _ => "Paste next",
                    };
                    row.child(act("paste-mode", paste, Kind::Primary, Action::PasteMode { inject: true }))
                        .when(!capturing && cursor > 0, |row| {
                            row.child(act("session-reset", "Start over", Kind::Plain, Action::SessionReset).tooltip("Go back to the first item."))
                        })
                        .child(
                            act("session-stop", "Stop", Kind::Danger, Action::SessionStop)
                                .tooltip("End this session. Your copies stay in History."),
                        )
                }
            });

        let footer = h_flex()
            .items_center()
            .gap(px(8.))
            .px(px(12.))
            .pb(px(10.))
            .min_h(px(30.))
            // Wraps: a mode's how-to runs two lines.
            .child(mono(11.).flex_1().min_w_0().line_clamp(2).text_color(rgb(pal().muted)).children(self.msg.clone()))
            .child(ghost("open-main", "Open Copycat").xsmall().on_click(|_, _, cx| tray::show_main(cx)));

        // Faded out inside the window's 8pt margin: one cut off at the window's
        // edge draws its rectangle.
        let shadow = vec![BoxShadow {
            color: rgba(pal().shadow).into(),
            offset: point(px(0.), px(2.)),
            blur_radius: px(6.),
            spread_radius: px(-2.),
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
                .when(connected, |this| this.child(actions))
                .child(footer),
        )
    }

    /// One `.sl-row` of the list, as the Session screen draws it.
    fn row(&self, ix: usize, clip: &ClipSummary, cursor: usize, cx: &Context<Self>) -> AnyElement {
        let (done, next) = (ix < cursor, ix == cursor);
        let tag = (done || next).then(|| {
            let tag = if next { Tag::primary().outline() } else { Tag::secondary() };
            tag.small().child(if next { "NEXT" } else { "PASTED" })
        });
        let (clip_id, preview) = (clip.id, SharedString::from(one_line(&clip.preview, 160)));
        let row = mono(12.)
            .id(("sl-row", ix))
            .flex()
            .items_center()
            .gap(px(8.))
            .pl(px(10.))
            .pr(px(8.))
            .py(px(6.))
            .bg(rgb(pal().raised))
            .when(done, |this| this.opacity(0.5))
            .when(!done, |this| {
                let ghost = preview.clone();
                this.cursor_grab()
                    .on_drag(DragRow { ix, id: clip_id }, move |_, _, _, cx| {
                        let text = ghost.clone();
                        cx.new(|_| DragGhost(text))
                    })
                    .drag_over::<DragRow>(move |style, from, _, _| {
                        if from.ix == ix { style } else { style.shadow(drop_marker(if from.ix > ix { -2. } else { 2. })) }
                    })
                    .on_drop(cx.listener(move |this, from: &DragRow, _, cx| this.move_item(from.ix, from.id, ix, cx)))
            })
            .child(div().w(px(12.)).flex_none().text_size(px(12.)).text_color(rgb(pal().faint)).child(if done { "" } else { "⠿" }))
            .child(div().w(px(20.)).flex_none().text_right().text_size(px(10.5)).text_color(rgb(pal().faint)).child((ix + 1).to_string()))
            .child(div().flex_1().min_w_0().truncate().child(preview.clone()))
            .children(tag);
        if next {
            accent_edge(row.bg(rgb(pal().raised)), rgba(ACTIVE_LINE), 8.).into_any_element()
        } else {
            row.rounded(px(8.)).border_1().border_color(rgb(pal().line)).into_any_element()
        }
    }

    /// One recent clip outside a session, in the session rows' look. A click
    /// pastes it; dropping it on a row above makes it the latest (history
    /// only reorders by moving a clip to the top).
    fn history_row(&self, ix: usize, clip: &ClipSummary, now: i64, cx: &Context<Self>) -> AnyElement {
        let (id, pinned) = (clip.id, clip.pinned);
        let preview = SharedString::from(one_line(&clip.preview, 160));
        let row = mono(12.)
            .id(("h-row", ix))
            .flex()
            .items_center()
            .gap(px(8.))
            .pl(px(10.))
            .pr(px(8.))
            .py(px(6.))
            .bg(rgb(pal().raised))
            .cursor_pointer()
            .hover(|style| style.bg(rgb(pal().raised_2)))
            .on_click(cx.listener(move |this, _, _, cx| this.act(Action::PasteId { id }, cx)))
            .when(ix > 0, |this| {
                let ghost = preview.clone();
                this.on_drag(DragRow { ix, id }, move |_, _, _, cx| {
                    let text = ghost.clone();
                    cx.new(|_| DragGhost(text))
                })
            })
            .drag_over::<DragRow>(move |style, from, _, _| {
                if from.ix > ix { style.shadow(drop_marker(-2.)) } else { style }
            })
            .on_drop(cx.listener(move |this, from: &DragRow, _, cx| {
                if from.ix > ix {
                    this.edit_history(Action::HistoryPromote { id: from.id }, cx);
                }
            }))
            .child(div().w(px(12.)).flex_none().text_size(px(12.)).text_color(rgb(pal().faint)).child(if ix > 0 { "⠿" } else { "" }))
            .child(div().w(px(20.)).flex_none().text_right().text_size(px(10.5)).text_color(rgb(pal().faint)).child((ix + 1).to_string()))
            .child(div().flex_1().min_w_0().truncate().child(preview))
            // A plain ⌘V pastes the newest.
            .when(ix == 0, |this| this.child(Tag::primary().outline().small().child("LATEST")))
            .child(div().flex_none().text_size(px(10.5)).text_color(rgb(pal().faint)).child(age(now, clip.captured_at)))
            .child(
                h_flex()
                    .gap(px(2.))
                    // The row's click pastes, so these stop it.
                    .when(ix > 0, |this| {
                        this.child(icon_button(("promote", ix), IconName::ArrowUp).xsmall().tooltip("Make latest").on_click(
                            cx.listener(move |this, _, _, cx| {
                                cx.stop_propagation();
                                this.edit_history(Action::HistoryPromote { id }, cx);
                            }),
                        ))
                    })
                    .child(
                        icon_button(("pin", ix), if pinned { IconName::StarFill } else { IconName::Star })
                            .xsmall()
                            .tooltip(if pinned { "Unpin" } else { "Pin" })
                            .text_color(if pinned { cx.theme().warning } else { cx.theme().muted_foreground })
                            .on_click(cx.listener(move |this, _, _, cx| {
                                cx.stop_propagation();
                                this.edit_history(Action::HistoryPin { id, pinned: !pinned }, cx);
                            })),
                    ),
            );
        if ix == 0 {
            accent_edge(row, rgba(ACTIVE_LINE), 8.).into_any_element()
        } else {
            row.rounded(px(8.)).border_1().border_color(rgb(pal().line)).into_any_element()
        }
    }
}

/// The orange line a dragged row would land on: above the row (`side` < 0)
/// or below it.
fn drop_marker(side: f32) -> Vec<BoxShadow> {
    vec![BoxShadow {
        color: rgb(ACTIVE).into(),
        offset: point(px(0.), px(side)),
        blur_radius: px(0.),
        spread_radius: px(0.),
        inset: false,
    }]
}

/// The three modes' start buttons: id, label, what it does, its action.
pub fn starts() -> [(&'static str, &'static str, &'static str, Action); 3] {
    [
        (
            "stack-start",
            "Stack",
            "Paste your recent copies one at a time, newest first. Each ⌘V pastes the next one.",
            Action::StackStart { duplicates: None },
        ),
        (
            "queue-capture",
            "Queue",
            "Copy a few things, then paste them back in the order you copied them.",
            Action::QueueCapture { duplicates: None },
        ),
        (
            "group-capture",
            "Group",
            "Copy a few things, then paste them all at once as one block.",
            Action::GroupCapture { delimiter: None, duplicates: None },
        ),
    ]
}

/// What to do once `mode` has started.
fn started(mode: SessionMode) -> &'static str {
    match mode {
        SessionMode::Stack => "Stack on. ⌘V pastes your copies, newest first.",
        SessionMode::Queue => "Queue on. Copy a few things, then ⌘V pastes them in order.",
        SessionMode::Group => "Group on. Copy a few things, then ⌘V pastes them all at once.",
    }
}

impl Render for Floating {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        themed(true, window, cx);
        match (self.expanded, self.mascot.is_some()) {
            (false, _) => match &self.tip {
                None => self.bubble(cx),
                // The bubble on the side the window grew, against the button.
                Some((typed, right)) => {
                    let tip = h_flex()
                        .w(px(TIP as f32))
                        .flex_none()
                        .when(*right, |this| this.justify_start())
                        .when(!*right, |this| this.justify_end())
                        .child(self.leader_tip(typed, cx));
                    let row = h_flex().size_full().items_center();
                    if *right { row.child(self.bubble(cx)).child(tip) } else { row.child(tip).child(self.bubble(cx)) }
                }
            },
            (true, false) => self.panel(cx),
            // The cat stays; the panel slides out of it, to the side with room.
            (true, true) => {
                let right = self.right;
                let panel = div()
                    .flex_1()
                    .min_w_0()
                    .h_full()
                    .when(right, |this| this.ml(px(-8.)))
                    .when(!right, |this| this.mr(px(-8.)))
                    .child(self.panel(cx))
                    .with_animation(
                        "panel-out",
                        Animation::new(Duration::from_millis(220)).with_easing(ease_out_quint()),
                        move |panel, t| {
                            let away = (1. - t) * 40.;
                            panel.opacity(t).relative().left(px(if right { -away } else { away }))
                        },
                    );
                let row = h_flex().size_full().map(|row| if self.down { row.items_start() } else { row.items_end() });
                if right { row.child(self.bubble(cx)).child(panel) } else { row.child(panel).child(self.bubble(cx)) }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::{INSET, clamp, ease_out_back, landing, leader_after, screen_under, velocity};
    use objc2_foundation::{NSPoint, NSRect, NSSize};

    #[test]
    fn summon_finds_the_pointer_s_screen_and_keeps_the_button_on_it() {
        let rect = |x, y, w, h| NSRect::new(NSPoint::new(x, y), NSSize::new(w, h));
        // AppKit space: the primary at the origin, a taller screen above and
        // to the left of it (negative x, y past the primary's height).
        let primary = rect(0., 0., 1440., 900.);
        let above_left = rect(-1920., 900., 1920., 1200.);
        let right = rect(1440., 0., 1920., 1080.);
        let frames = [primary, above_left, right];
        assert_eq!(screen_under(NSPoint::new(700., 450.), &frames), Some(0));
        assert_eq!(screen_under(NSPoint::new(-10., 2000.), &frames), Some(1));
        // The upper screen's top-left pixel, and the primary's top-left one.
        assert_eq!(screen_under(NSPoint::new(-1920., 2100.), &frames), Some(1));
        assert_eq!(screen_under(NSPoint::new(0., 900.), &frames), Some(0));
        // The right screen's left column, not the primary's right edge.
        assert_eq!(screen_under(NSPoint::new(1440., 500.), &frames), Some(2));
        // In the gap above the primary: none.
        assert_eq!(screen_under(NSPoint::new(100., 1500.), &frames), None);

        // A 64pt button centred on a pointer in the upper screen's top-left
        // corner, kept inside its visible frame (menu bar off the top).
        let visible = rect(-1920., 900., 1920., 1175.);
        let mut r = rect(-1920. - 32., 2100. - 32., 64., 64.);
        clamp(&mut r, visible);
        assert_eq!((r.origin.x, r.origin.y), (-1920., 2075. - 64.));
        // Well inside, it stays centred on the pointer.
        let mut r = rect(-1000. - 32., 1500. - 32., 64., 64.);
        clamp(&mut r, visible);
        assert_eq!((r.origin.x, r.origin.y), (-1032., 1468.));
    }

    #[test]
    fn a_throw_lands_in_the_corner_it_was_thrown_toward() {
        let rect = |x, y, w, h| NSRect::new(NSPoint::new(x, y), NSSize::new(w, h));
        let pt = |x, y| NSPoint::new(x, y);
        // The primary with a menu bar, a second screen to its right.
        let frames = [rect(0., 0., 1440., 900.), rect(1440., 0., 1920., 1080.)];
        let visible = [rect(0., 0., 1440., 875.), rect(1440., 0., 1920., 1080.)];
        let b = |x, y| rect(x, y, 64., 64.);

        // 1000 pt/s over the last 62.5 ms; a pointer that rested has none.
        let moving = [(0.5, pt(-500., 0.)), (0.0625, pt(0., 0.)), (0.03125, pt(30., 0.)), (0., pt(62.5, 0.))];
        assert_eq!((velocity(&moving).x, velocity(&moving).y), (1000., 0.));
        let rested = [(0.3, pt(0., 0.)), (0.2, pt(90., 0.)), (0., pt(90., 0.))];
        assert_eq!((velocity(&rested).x, velocity(&rested).y), (0., 0.));
        // One that outran the button: from its last sample to the release.
        let escaped = [(0.5, pt(0., 0.)), (0.15, pt(100., 0.)), (0., pt(400., 0.))];
        assert_eq!((velocity(&escaped).x, velocity(&escaped).y), (2000., 0.));

        // Thrown hard up and left from the middle: top left.
        let (n, to) = landing(b(688., 418.), pt(-2000., 1500.), &frames, &visible, 0, false);
        assert_eq!((n, to.x, to.y), (0, INSET, 875. - INSET - 64.));
        // Dropped slowly near the bottom right: bottom right.
        let (n, to) = landing(b(1300., 60.), pt(20., -10.), &frames, &visible, 0, false);
        assert_eq!((n, to.x, to.y), (0, 1440. - INSET - 64., INSET));
        // Thrown right onto the second screen: its far bottom corner, unless
        // the button must stay on its own screen.
        let (n, to) = landing(b(1300., 400.), pt(3000., 0.), &frames, &visible, 0, false);
        assert_eq!((n, to.x, to.y), (1, 3360. - INSET - 64., INSET));
        let (n, to) = landing(b(1300., 400.), pt(3000., 0.), &frames, &visible, 0, true);
        assert_eq!((n, to.x, to.y), (0, 1440. - INSET - 64., INSET));

        assert_eq!((ease_out_back(0.), ease_out_back(1.), ease_out_back(2.)), (0., 1., 1.));
        assert!(ease_out_back(0.9) > 1.);
    }

    #[test]
    fn the_leader_bubble_follows_the_debug_events() {
        let fold = |events: &[(&str, &str)]| {
            events.iter().fold(None, |shown, (kind, detail)| leader_after(shown, kind, detail))
        };
        assert_eq!(fold(&[("leader", "leader pressed — waiting for the sequence key")]), Some(String::new()));
        assert_eq!(fold(&[("leader", "leader pressed — …"), ("leader-typing", "g"), ("hotkey", "x fired")]), Some("g".into()));
        assert_eq!(fold(&[("leader", "leader pressed — …"), ("leader-typing", "g"), ("leader-key", "gp → paste")]), None);
        assert_eq!(fold(&[("leader", "leader pressed — …"), ("leader", "timed out — no sequence key")]), None);
        assert_eq!(fold(&[("leader", "leader pressed — …"), ("leader", "cancelled")]), None);
    }
}
