//! The interactive floating button (Settings → "Interactive mascot"): the
//! Copycat cat, animated from the `copycat-sprites-v2` atlases, in place of
//! the round button.
//!
//! The atlases are a 12 × 4 grid: one row per look (stack, queue, group, and
//! "leader", the plain cat), and per row four phases of three frames each:
//! idle, pressed, transition, selected. Downscaled to 144px cells, 2x the 72pt
//! the cat is drawn at.
//!
//! What it shows:
//! - no mode (normal, paused, offline): the plain cat idling; paused idles at
//!   half speed and is dimmed, offline is dimmed more.
//! - a stack / queue / group session: that row idling (the cat with its prop).
//! - a click, and the panel collapsing back to the button: the manifest's
//!   click sequence, pressed → transition → selected, selected held ~1.2s.
//! - a mode starting / changing / ending, a paste (cursor moves) or a copy
//!   (history changes): transition → selected, then idle.
//! - the leader key: the plain cat's selected frames, which wear a crown.
//!   Since the crown means "leader", the plain cat's other reactions stop on
//!   transition's last frame (a happy cat) instead of reaching selected.

use std::sync::{Arc, LazyLock};
use std::time::{Duration, Instant};

use copycat_core::SessionMode;
use copycat_protocol::{Action, ResultBody, call};
use gpui_kit::*;

use crate::app::{Daemon, history_mark};

/// The cat's size on screen, in points.
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

impl Phase {
    /// The manifest's `durations_ms`, per frame.
    fn frame_ms(self) -> u64 {
        match self {
            Idle => 420,
            Pressed => 95,
            Transition => 120,
            Selected => 320,
        }
    }
}

/// A reaction: phases and how long each lasts. A looping phase (idle,
/// selected) cycles its three frames; the others play once and hold the last.
type Script = &'static [(Phase, u64)];
const CLICK: Script = &[(Pressed, 285), (Transition, 360), (Selected, 1200)];
const CHEER: Script = &[(Transition, 360), (Selected, 1200)];
const PLAIN_CLICK: Script = &[(Pressed, 285), (Transition, 1200)];
const PLAIN_CHEER: Script = &[(Transition, 1200)];
const CROWN: Script = &[(Selected, 1500)];

fn atlas(dark: bool) -> Arc<Image> {
    static LIGHT: LazyLock<Arc<Image>> = LazyLock::new(|| {
        Arc::new(Image::from_bytes(ImageFormat::Png, include_bytes!("../assets/mascot/light.png").to_vec()))
    });
    static DARK: LazyLock<Arc<Image>> = LazyLock::new(|| {
        Arc::new(Image::from_bytes(ImageFormat::Png, include_bytes!("../assets/mascot/dark.png").to_vec()))
    });
    if dark { DARK.clone() } else { LIGHT.clone() }
}

/// What a daemon change is compared on: the session and where it stands, and
/// the history mark.
type Seen = (Option<(u64, SessionMode, usize, usize)>, Option<(Option<copycat_core::ClipId>, usize)>);

pub struct Mascot {
    daemon: Entity<Daemon>,
    seen: Option<Seen>,
    /// The reaction playing: its row, script and start. None is idling.
    playing: Option<(usize, Script, Instant)>,
    /// The idle loop's clock.
    born: Instant,
    /// The newest debug event seen; None until the first answer.
    since: Option<u64>,
    _sub: Subscription,
    _tick: Task<()>,
    _leader: Task<()>,
}

impl Mascot {
    pub fn new(cx: &mut Context<Self>) -> Self {
        let daemon = Daemon::get(cx);
        Mascot {
            seen: None,
            playing: None,
            born: Instant::now(),
            since: None,
            _sub: cx.observe(&daemon, |this, _, cx| this.on_daemon(cx)),
            daemon,
            // One frame at a time, until the window (and so this entity) goes.
            // A redraw asked for while the panel is up draws nothing.
            _tick: cx.spawn(async move |this, cx| {
                loop {
                    let Ok(wait) = this.update(cx, |this, cx| {
                        cx.notify();
                        this.frame(cx).2
                    }) else {
                        return;
                    };
                    cx.background_executor().timer(Duration::from_millis(wait)).await;
                }
            }),
            // The leader key is only in the daemon's debug feed, as the
            // overlay reads it.
            // ponytail: one small poll per floating window; share one in
            // `Daemon` if several screens' worth ever shows up in a profile.
            _leader: cx.spawn(async move |this, cx| {
                loop {
                    let Ok(since) = this.update(cx, |this, _| this.since) else { return };
                    let reply = cx
                        .background_spawn(async move {
                            call(&copycat_launcher::socket_path(), Action::DebugEvents { since })
                        })
                        .await;
                    if this.update(cx, |this, _| this.on_events(reply.ok())).is_err() {
                        return;
                    }
                    cx.background_executor().timer(Duration::from_millis(400)).await;
                }
            }),
        }
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

    fn play(&mut self, row: usize, script: Script) {
        self.playing = Some((row, script, Instant::now()));
    }

    /// Clicked, or shown again after the panel.
    pub fn clicked(&mut self, cx: &mut Context<Self>) {
        let row = self.base(cx);
        self.play(row, if row == PLAIN { PLAIN_CLICK } else { CLICK });
        cx.notify();
    }

    fn on_daemon(&mut self, cx: &mut Context<Self>) {
        // Offline is nothing to react to; coming back compares with before.
        let Some(status) = self.daemon.read(cx).status.as_ref() else { return };
        let session = status.core.session.as_ref().map(|s| (s.id, s.mode, s.cursor, s.remaining));
        let seen = (session, history_mark(Some(status)));
        if self.seen.as_ref().is_some_and(|old| *old != seen) {
            let row = self.base(cx);
            self.play(row, if row == PLAIN { PLAIN_CHEER } else { CHEER });
        }
        self.seen = Some(seen);
    }

    fn on_events(&mut self, reply: Option<ResultBody>) {
        let Some(ResultBody::Events { events, latest }) = reply else { return };
        // The first answer only sets the cursor, so old presses do not replay;
        // a smaller `latest` is a restarted daemon numbering from 1 again.
        let fresh = self.since.is_none_or(|since| latest < since);
        self.since = Some(latest);
        // "leader" is also logged for a timeout; only a press crowns the cat.
        if !fresh && events.iter().any(|e| e.kind == "leader" && !e.detail.starts_with("timed out")) {
            self.play(PLAIN, CROWN);
        }
    }

    /// The atlas cell to draw now, as (row, column), and the milliseconds
    /// until the next frame.
    fn frame(&mut self, cx: &App) -> (usize, usize, u64) {
        if let Some((row, script, start)) = self.playing {
            let mut t = start.elapsed().as_millis() as u64;
            for &(phase, len) in script {
                if t < len {
                    let ms = phase.frame_ms();
                    let n = t / ms;
                    let n = if matches!(phase, Selected) { n % 3 } else { n.min(2) };
                    return (row, phase as usize * 3 + n as usize, (ms - t % ms).min(len - t));
                }
                t -= len;
            }
            self.playing = None;
        }
        let paused = self.daemon.read(cx).status.as_ref().is_some_and(|s| s.core.paused);
        let ms = Idle.frame_ms() * if paused { 2 } else { 1 };
        let t = self.born.elapsed().as_millis() as u64;
        (self.base(cx), (t / ms % 3) as usize, ms - t % ms)
    }
}

impl Render for Mascot {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let (row, col, _) = self.frame(cx);
        let dark = matches!(window.appearance(), WindowAppearance::Dark | WindowAppearance::VibrantDark);
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
                img(atlas(dark))
                    .absolute()
                    .left(px(-CELL * col as f32))
                    .top(px(-CELL * row as f32))
                    .w(px(CELL * 12.))
                    .h(px(CELL * 4.)),
            )
    }
}
