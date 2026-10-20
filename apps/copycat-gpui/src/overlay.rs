//! The debug overlay (macOS only): the live mode, the next item and each
//! binding trigger as it fires, over `debug.events` polled with a cursor, as
//! `debug.js` does. Drag it to move it.

use std::collections::VecDeque;
use std::time::Duration;

use copycat_protocol::{Action, DebugEvent, ResultBody, call};
use gpui_kit::component::{h_flex, v_flex};
use gpui_kit::prelude::*;
use gpui_kit::*;

use crate::app::Daemon;
use crate::brand::*;
use crate::floating::{Drag, draggable};
use crate::screens::one_line;

pub struct Overlay {
    daemon: Entity<Daemon>,
    /// The newest event id seen.
    since: u64,
    /// The first poll only sets the cursor, so the backlog does not flash.
    seeded: bool,
    /// A drop means the daemon restarted and numbers its events from 1 again.
    uptime: u64,
    /// Failures of `debug.events` while status still answers.
    misses: u32,
    /// A daemon that predates debug events: stop asking until it restarts.
    unsupported: bool,
    recent: VecDeque<DebugEvent>,
    flash: Option<DebugEvent>,
    drag: Drag,
    _sub: Subscription,
    _poll: Task<()>,
}

impl Overlay {
    pub fn new(cx: &mut Context<Self>) -> Self {
        let daemon = Daemon::get(cx);
        Overlay {
            since: 0,
            seeded: false,
            uptime: 0,
            misses: 0,
            unsupported: false,
            recent: VecDeque::new(),
            flash: None,
            drag: Drag::default(),
            _sub: cx.observe(&daemon, |this, _, cx| this.on_status(cx)),
            _poll: cx.spawn(async move |this, cx| {
                loop {
                    let Ok((since, ask)) = this.update(cx, |this, _| (this.since, !this.unsupported)) else { return };
                    if ask {
                        let reply = cx
                            .background_spawn(async move {
                                call(&copycat_launcher::socket_path(), Action::DebugEvents { since: Some(since) })
                            })
                            .await;
                        if this.update(cx, |this, cx| this.on_events(reply.ok(), cx)).is_err() {
                            return;
                        }
                    }
                    cx.background_executor().timer(Duration::from_millis(250)).await;
                }
            }),
            daemon,
        }
    }

    fn on_status(&mut self, cx: &mut Context<Self>) {
        if let Some(status) = &self.daemon.read(cx).status {
            if status.uptime_ms < self.uptime {
                (self.since, self.seeded, self.misses, self.unsupported) = (0, false, 0, false);
            }
            self.uptime = status.uptime_ms;
        }
        cx.notify();
    }

    fn on_events(&mut self, body: Option<ResultBody>, cx: &mut Context<Self>) {
        let Some(ResultBody::Events { events, latest }) = body else {
            if self.daemon.read(cx).connected {
                self.misses += 1;
                self.unsupported = self.misses >= 8;
            }
            return cx.notify();
        };
        self.misses = 0;
        self.since = latest;
        let fresh = std::mem::take(&mut self.seeded);
        self.seeded = true;
        if !events.is_empty() {
            if !fresh {
                self.flash = events.last().cloned();
            }
            self.recent.extend(events);
            while self.recent.len() > 7 {
                self.recent.pop_front();
            }
        }
        cx.notify();
    }
}

/// Epoch milliseconds as mm:ss.
fn clock(ms: i64) -> String {
    format!("{:02}:{:02}", ms / 60_000 % 60, ms / 1000 % 60)
}

/// The kind's colour: leader loud, hotkey gold, paste chord green.
fn tint(kind: &str) -> Option<u32> {
    match kind {
        "leader" | "leader-key" => Some(ACTIVE),
        "hotkey" => Some(GOLD),
        "paste-chord" => Some(OK),
        _ => None,
    }
}

impl Render for Overlay {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let daemon = self.daemon.read(cx);
        let core = daemon.status.as_ref().map(|s| &s.core);
        let (mode, next) = match core.and_then(|c| c.session.as_ref()) {
            Some(s) => (
                if s.state == copycat_core::SessionState::Capturing {
                    format!("{} · capturing {}", s.mode.as_str().to_uppercase(), s.size)
                } else {
                    format!("{} · {}/{}", s.mode.as_str().to_uppercase(), s.cursor, s.size)
                },
                s.next.map_or("—".into(), |id| format!("#{}", id.0)),
            ),
            None => (
                if core.is_some_and(|c| c.paused) { "PAUSED" } else { "NORMAL" }.to_string(),
                core.and_then(|c| c.latest.as_ref()).map_or("—".into(), |c| one_line(&c.preview, 30)),
            ),
        };

        let (kind, detail) = match (&self.flash, self.unsupported) {
            (_, true) => (
                "this daemon can't report triggers".to_string(),
                "Restart it from Copycat → Settings to use the bundled one".to_string(),
            ),
            (Some(e), _) => (
                match e.kind.as_str() {
                    "leader" | "leader-key" => "LEADER".to_string(),
                    "hotkey" => "HOTKEY".to_string(),
                    "paste-chord" => "PASTE".to_string(),
                    other => other.to_uppercase(),
                },
                e.detail.clone(),
            ),
            _ => ("waiting for a trigger…".to_string(), String::new()),
        };
        let accent = self.flash.as_ref().filter(|_| !self.unsupported).and_then(|e| tint(&e.kind));
        let flash = v_flex()
            .gap(px(2.))
            .px(px(10.))
            .py(px(9.))
            .rounded(px(8.))
            .border_1()
            .border_color(rgb(LINE))
            .bg(rgb(RAISED))
            .when_some(accent, |this, c| {
                this.border_color(rgba(c << 8 | 0xB3)).bg(rgba(c << 8 | 0x29))
            })
            .child(div().text_size(px(12.)).when(accent.is_none(), |t| t.text_color(rgb(FAINT))).when_some(accent, |t, c| t.text_color(rgb(c)).font_weight(FontWeight::SEMIBOLD)).child(kind))
            .child(div().text_size(px(10.5)).text_color(rgb(MUTED)).truncate().child(one_line(&detail, 60)));

        let feed = v_flex().flex_1().min_h_0().overflow_hidden().gap(px(3.)).children(self.recent.iter().rev().map(|e| {
            h_flex()
                .gap(px(8.))
                .text_size(px(10.5))
                .text_color(rgb(MUTED))
                .child(div().w(px(36.)).flex_none().text_color(rgb(FAINT)).child(clock(e.at)))
                .child(div().w(px(78.)).flex_none().when_some(tint(&e.kind), |t, c| t.text_color(rgb(c))).child(e.kind.clone()))
                .child(div().flex_1().min_w_0().truncate().child(one_line(&e.detail, 40)))
        }));

        let shadow = vec![BoxShadow {
            color: rgba(0x000000CC).into(),
            offset: point(px(0.), px(16.)),
            blur_radius: px(44.),
            spread_radius: px(-16.),
            inset: false,
        }];
        let panel = v_flex()
            .id("overlay")
            .size_full()
            .gap(px(8.))
            .px(px(12.))
            .py(px(11.))
            .rounded(px(12.))
            .border_1()
            .border_color(rgb(LINE))
            .bg(rgba(0x14161AF0))
            .shadow(shadow)
            .cursor_grab()
            .child(
                h_flex()
                    .items_center()
                    .gap(px(9.))
                    .child(
                        mono(9.)
                            .font_weight(FontWeight::BOLD)
                            .text_color(rgb(ACTIVE))
                            .px(px(6.))
                            .py(px(1.))
                            .rounded(px(5.))
                            .border_1()
                            .border_color(rgba(0xE4672B80))
                            .child(spaced("DEBUG", 1.6)),
                    )
                    .child(mono(12.5).font_weight(FontWeight::BOLD).child(spaced(&mode, 0.75)))
                    .child(div().flex_1())
                    .child(dot(daemon.connected, false)),
            )
            .child(
                mono(11.)
                    .flex()
                    .items_center()
                    .gap(px(9.))
                    .child(mono(9.).text_color(rgb(FAINT)).child(spaced("NEXT", 1.1)))
                    .child(div().min_w_0().truncate().text_color(rgb(MUTED)).child(next)),
            )
            .child(flash)
            .child(feed);
        let panel = draggable(panel, cx, |this| &mut this.drag, |_, _, _| {});
        mono(11.).size_full().p(px(8.)).text_color(rgb(TEXT)).child(panel)
    }
}
