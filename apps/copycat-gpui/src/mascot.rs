//! The interactive floating button (Settings → "Interactive mascot"): the
//! Copycat cat, animated from the `copycat-v3` atlases, in place of the round
//! button.
//!
//! The atlases are a 36 × 4 grid: one row per look (stack, queue, group, and
//! "leader", the plain cat), and per row four phases of nine frames each:
//! idle, pressed, transition, selected. Downscaled from 192px to 144px cells,
//! 2x the 72pt the cat is drawn at (at 100%), and stored as lossy WebP (q85,
//! lossless alpha): about a fifth of lossless PNG, with no visible difference
//! at size.
//!
//! What it shows:
//! - no mode (normal, paused, offline): the plain cat idling. Idling is a rest
//!   of a few random seconds, then the idle loop once or twice; paused idles
//!   at half speed and is dimmed, offline is dimmed more.
//! - a stack / queue / group session: that row idling (the cat with its prop).
//! - a click, and the panel collapsing back to the button: the manifest's
//!   click sequence, pressed → transition → selected, selected held ~1.4s.
//! - a mode starting / changing / ending, a paste (cursor moves) or a copy
//!   (history changes): transition → selected, then idle.
//! - the leader key: the plain cat's selected frames, which wear a crown.
//!   Since the crown means "leader", the plain cat's other reactions stop on
//!   transition's last frame (a happy cat) instead of reaching selected.
//! - dragged: the cat dangling by its scruff; thrown, it flies the way it
//!   goes to its corner and lands, then idles.
//! - nothing for Settings → "Sleep after", or "Sleep now": it kneads, falls
//!   asleep and sleeps until something above happens. There is no wake-up
//!   drawn: it cuts back.
//!
//! The last three are from the scenes atlases (`copycat-scenarios-final`),
//! 9 × 7 cells of the same size: held, fling right / left / up / down, knead
//! and sleep. Their cat is drawn smaller, so it was scaled up to match
//! (`docs/guides/tools/mascot-scenes.py`).

use std::hash::BuildHasher;
use std::sync::{Arc, LazyLock};
use std::time::{Duration, Instant};

use copycat_core::SessionMode;
use gpui_kit::*;

use crate::app::{Daemon, history_mark};
use crate::settings::{Nap, Settings};
// At the floating button's size (Settings).
use crate::brand::px;

/// The cat's size on screen, in points, at 100%.
pub const CELL: f32 = 72.;

const STACK: usize = 0;
const QUEUE: usize = 1;
const GROUP: usize = 2;
const PLAIN: usize = 3;

#[derive(Clone, Copy)]
enum Phase {
    Idle,
    Pressed,
    Transition,
    Selected,
}
use Phase::*;

/// Frames per phase.
const FRAMES: u64 = 9;

// The scenes atlases' rows, and their frames' ms.
const HELD: usize = 0;
const FLING_RIGHT: usize = 1;
const FLING_LEFT: usize = 2;
const FLING_UP: usize = 3;
const FLING_DOWN: usize = 4;
const KNEAD: usize = 5;
const SLEEP: usize = 6;
const HELD_MS: u64 = 210;
const FLING_MS: u64 = 100;
/// A fling's first frames crouch to jump; let go, it is already in the air.
const FLING_FROM: u64 = 3;
const KNEAD_MS: u64 = 190;
const SLEEP_MS: u64 = 410;

/// Knead loops before it sleeps.
const KNEADS: u64 = 3;

impl Phase {
    /// The manifest's `frameDurationMs`, per frame; idle slowed down.
    fn frame_ms(self) -> u64 {
        match self {
            Idle => 190,
            Pressed => 60,
            Transition => 65,
            Selected => 120,
        }
    }
}

/// A reaction: phases and how long each lasts. A looping phase (idle,
/// selected) cycles its three frames; the others play once and hold the last.
type Script = &'static [(Phase, u64)];
const CLICK: Script = &[(Pressed, 540), (Transition, 585), (Selected, 1440)];
const CHEER: Script = &[(Transition, 585), (Selected, 1440)];
const PLAIN_CLICK: Script = &[(Pressed, 540), (Transition, 1200)];
const PLAIN_CHEER: Script = &[(Transition, 1200)];
const CROWN: Script = &[(Selected, 1440)];

/// A number in `0..n`, different each call.
fn roll(n: u64) -> u64 {
    std::collections::hash_map::RandomState::new().hash_one(0) % n
}

/// One stretch of idling: a rest on the first frame, then the idle loop
/// `loops` times.
#[derive(Clone, Copy)]
struct Idling {
    start: Instant,
    rest: u64,
    loops: u64,
}

impl Idling {
    fn new() -> Self {
        Idling { start: Instant::now(), rest: 1500 + roll(4500), loops: 1 + roll(2) }
    }
}

/// The idle column at `t` ms into a stretch, frames `ms` apart, and the ms
/// until the next frame; None once the stretch is over.
fn idle_at(t: u64, rest: u64, loops: u64, ms: u64) -> Option<(usize, u64)> {
    if t < rest {
        return Some((0, rest - t));
    }
    let t = t - rest;
    (t < loops * FRAMES * ms).then(|| ((t / ms % FRAMES) as usize, ms - t % ms))
}

/// The dozing cell `t` ms after dozing off, as (row, column), and the ms
/// until the next frame: knead's two frames in, its loop (the other seven)
/// `KNEADS` times, then sleep's four frames in and its loop for good.
fn dozing(t: u64) -> (usize, usize, u64) {
    let kneading = (2 + 7 * KNEADS) * KNEAD_MS;
    let (row, ms, enter, t) = if t < kneading { (KNEAD, KNEAD_MS, 2, t) } else { (SLEEP, SLEEP_MS, 4, t - kneading) };
    let n = t / ms;
    let col = if n < enter { n } else { enter + (n - enter) % (FRAMES - enter) };
    (row, col as usize, ms - t % ms)
}

/// The atlas: the core one or the scenes one, in the light or dark look.
fn atlas(dark: bool, scenes: bool) -> Arc<Image> {
    fn load(bytes: &'static [u8]) -> Arc<Image> {
        Arc::new(Image::from_bytes(ImageFormat::Webp, bytes.to_vec()))
    }
    static SHEETS: LazyLock<[Arc<Image>; 4]> = LazyLock::new(|| {
        [
            load(include_bytes!("../assets/mascot/light.webp")),
            load(include_bytes!("../assets/mascot/dark.webp")),
            load(include_bytes!("../assets/mascot/scenes-light.webp")),
            load(include_bytes!("../assets/mascot/scenes-dark.webp")),
        ]
    });
    SHEETS[dark as usize + 2 * scenes as usize].clone()
}

/// What a daemon change is compared on: the session and where it stands, and
/// the history mark.
type Seen = (Option<(u64, SessionMode, usize, usize)>, Option<(Option<copycat_core::ClipId>, usize)>);

pub struct Mascot {
    daemon: Entity<Daemon>,
    seen: Option<Seen>,
    /// The reaction playing: its row, script and start. None is idling.
    playing: Option<(usize, Script, Instant)>,
    /// The idle stretch under way.
    idling: Idling,
    /// Held (looping, while dragged) or a fling (once), over everything
    /// else: its scenes row and start.
    scene: Option<(usize, Instant)>,
    /// When something last happened, to doze off after "Sleep after".
    active: Instant,
    /// When "Sleep now" put it to sleep; cleared when something happens.
    napped: Option<Instant>,
    _sub: Subscription,
    _nap: Subscription,
    _tick: Task<()>,
}

impl Mascot {
    pub fn new(cx: &mut Context<Self>) -> Self {
        let daemon = Daemon::get(cx);
        Mascot {
            seen: None,
            playing: None,
            idling: Idling::new(),
            scene: None,
            active: Instant::now(),
            napped: None,
            _sub: cx.observe(&daemon, |this, _, cx| this.on_daemon(cx)),
            _nap: cx.observe_global::<Nap>(|this, cx| {
                this.wake(cx);
                this.playing = None;
                this.napped = Some(Instant::now());
            }),
            daemon,
            _tick: Self::tick(cx),
        }
    }

    /// One frame at a time, until the window (and so this entity) goes.
    fn tick(cx: &mut Context<Self>) -> Task<()> {
        cx.spawn(async move |this, cx| {
            loop {
                let Ok(wait) = this.update(cx, |this, cx| {
                    cx.notify();
                    this.frame(cx).3
                }) else {
                    return;
                };
                cx.background_executor().timer(Duration::from_millis(wait)).await;
            }
        })
    }

    /// The row the cat idles in: the session's mode, else the plain cat.
    fn base(&self, cx: &App) -> usize {
        let status = self.daemon.read(cx).status.as_ref();
        match status.and_then(|s| s.core.session.as_ref()).map(|s| s.mode) {
            Some(SessionMode::Stack) => STACK,
            Some(SessionMode::Queue) => QUEUE,
            Some(SessionMode::Group) => GROUP,
            None => PLAIN,
        }
    }

    /// Start a reaction.
    fn play(&mut self, row: usize, script: Script, cx: &mut Context<Self>) {
        self.playing = Some((row, script, Instant::now()));
        self.wake(cx);
    }

    /// Something happened: awake, and a fresh tick, as the old one may be
    /// asleep for an idle rest of several seconds.
    fn wake(&mut self, cx: &mut Context<Self>) {
        self.active = Instant::now();
        self.napped = None;
        self._tick = Self::tick(cx);
    }

    /// Picked up by a drag: it dangles until let go.
    pub fn hold(&mut self, cx: &mut Context<Self>) {
        if self.scene.is_none_or(|(row, _)| row != HELD) {
            self.scene = Some((HELD, Instant::now()));
            self.wake(cx);
        }
    }

    /// Let go, then carried `dx`, `dy` points (y up): it flies the way it
    /// goes, or, barely carried, just lands.
    pub fn thrown(&mut self, dx: f64, dy: f64, cx: &mut Context<Self>) {
        let row = match (dx.abs() >= dy.abs(), dx > 0., dy > 0.) {
            _ if dx.hypot(dy) < 8. => None,
            (true, true, _) => Some(FLING_RIGHT),
            (true, false, _) => Some(FLING_LEFT),
            (false, _, true) => Some(FLING_UP),
            (false, _, false) => Some(FLING_DOWN),
        };
        self.scene = row.map(|row| (row, Instant::now()));
        self.playing = None;
        self.wake(cx);
    }

    /// Clicked, or shown again after the panel.
    pub fn clicked(&mut self, cx: &mut Context<Self>) {
        let row = self.base(cx);
        self.play(row, if row == PLAIN { PLAIN_CLICK } else { CLICK }, cx);
    }

    fn on_daemon(&mut self, cx: &mut Context<Self>) {
        // Offline is nothing to react to; coming back compares with before.
        let Some(status) = self.daemon.read(cx).status.as_ref() else { return };
        let session = status.core.session.as_ref().map(|s| (s.id, s.mode, s.cursor, s.remaining));
        let seen = (session, history_mark(Some(status)));
        if self.seen.as_ref().is_some_and(|old| *old != seen) {
            let row = self.base(cx);
            self.play(row, if row == PLAIN { PLAIN_CHEER } else { CHEER }, cx);
        }
        self.seen = Some(seen);
    }

    /// The leader key was pressed: the cat is crowned.
    pub fn led(&mut self, cx: &mut Context<Self>) {
        self.play(PLAIN, CROWN, cx);
    }

    /// The cell to draw now, as (from the scenes atlas, row, column), and
    /// the milliseconds until the next frame.
    fn frame(&mut self, cx: &App) -> (bool, usize, usize, u64) {
        if let Some((row, start)) = self.scene {
            let t = start.elapsed().as_millis() as u64;
            if row == HELD {
                // Eight frames, not nine.
                return (true, HELD, (t / HELD_MS % 8) as usize, HELD_MS - t % HELD_MS);
            }
            if t < (FRAMES - FLING_FROM) * FLING_MS {
                return (true, row, (FLING_FROM + t / FLING_MS) as usize, FLING_MS - t % FLING_MS);
            }
            self.scene = None;
            self.idling = Idling::new();
        }
        if let Some((row, script, start)) = self.playing {
            let mut t = start.elapsed().as_millis() as u64;
            for &(phase, len) in script {
                if t < len {
                    let ms = phase.frame_ms();
                    let n = t / ms;
                    let n = if matches!(phase, Selected) { n % FRAMES } else { n.min(FRAMES - 1) };
                    return (false, row, phase as usize * FRAMES as usize + n as usize, (ms - t % ms).min(len - t));
                }
                t -= len;
            }
            self.playing = None;
            self.idling = Idling::new();
        }
        let after = Settings::get(cx).read(cx).sleep_after;
        let dozed = match self.napped {
            Some(at) => Some(at.elapsed()),
            None if after > 0 => self.active.elapsed().checked_sub(Duration::from_secs(after * 60)),
            None => None,
        };
        if let Some(t) = dozed {
            let (row, col, wait) = dozing(t.as_millis() as u64);
            return (true, row, col, wait);
        }
        let paused = self.daemon.read(cx).status.as_ref().is_some_and(|s| s.core.paused);
        let ms = Idle.frame_ms() * if paused { 2 } else { 1 };
        let Idling { start, rest, loops } = self.idling;
        let (col, wait) = idle_at(start.elapsed().as_millis() as u64, rest, loops, ms).unwrap_or_else(|| {
            self.idling = Idling::new();
            (0, self.idling.rest)
        });
        (false, self.base(cx), col, wait)
    }
}

impl Render for Mascot {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let (scenes, row, col, _) = self.frame(cx);
        let (cols, rows) = if scenes { (9., 7.) } else { (36., 4.) };
        // The floating window's look, set by its root view just before.
        let dark = crate::brand::is_dark();
        let daemon = self.daemon.read(cx);
        let paused = daemon.status.as_ref().is_some_and(|s| s.core.paused);
        // One cell of the atlas: the whole sheet, shifted, clipped to the cell.
        div()
            .relative()
            .size(px(CELL))
            .flex_none()
            .overflow_hidden()
            .opacity(if !daemon.connected { 0.55 } else if paused { 0.75 } else { 1. })
            .child(
                img(atlas(dark, scenes))
                    .absolute()
                    .left(px(-CELL * col as f32))
                    .top(px(-CELL * row as f32))
                    .w(px(CELL * cols))
                    .h(px(CELL * rows)),
            )
    }
}

#[cfg(test)]
mod tests {
    use super::{KNEAD, KNEAD_MS, KNEADS, SLEEP, SLEEP_MS, dozing, idle_at, roll};

    #[test]
    fn idling_rests_then_loops() {
        assert_eq!(idle_at(0, 1000, 2, 100), Some((0, 1000)));
        assert_eq!(idle_at(999, 1000, 2, 100), Some((0, 1)));
        assert_eq!(idle_at(1250, 1000, 2, 100), Some((2, 50)));
        assert_eq!(idle_at(1000 + 9 * 100, 1000, 2, 100), Some((0, 100)));
        assert_eq!(idle_at(1000 + 18 * 100 - 1, 1000, 2, 100), Some((8, 1)));
        assert_eq!(idle_at(1000 + 18 * 100, 1000, 2, 100), None);
        assert!((0..50).map(|_| roll(4500)).any(|n| n != roll(4500)));
    }

    #[test]
    fn dozing_kneads_then_sleeps() {
        let k = KNEAD_MS;
        assert_eq!(dozing(0), (KNEAD, 0, k));
        assert_eq!(dozing(k), (KNEAD, 1, k));
        assert_eq!(dozing(2 * k + 10), (KNEAD, 2, k - 10));
        // The loop skips the two frames in: 8 → 2.
        assert_eq!(dozing(9 * k), (KNEAD, 2, k));
        let asleep = (2 + 7 * KNEADS) * k;
        assert_eq!(dozing(asleep - 1), (KNEAD, 8, 1));
        assert_eq!(dozing(asleep), (SLEEP, 0, SLEEP_MS));
        assert_eq!(dozing(asleep + 8 * SLEEP_MS), (SLEEP, 8, SLEEP_MS));
        assert_eq!(dozing(asleep + 9 * SLEEP_MS), (SLEEP, 4, SLEEP_MS));
    }
}
