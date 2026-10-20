//! The three screens, each a thin view over daemon actions — the same actions
//! the Tauri app's History, Session and Settings screens send.

use copycat_core::{ClipSummary, SessionState};
use copycat_protocol::{Action, CheckStatus};
use gpui_kit::component::input::Input;
use gpui_kit::component::{h_flex, v_flex};
use gpui_kit::prelude::*;
use gpui_kit::*;

use crate::app::AppView;
use crate::brand::*;

impl AppView {
    pub fn history_screen(&self, cx: &Context<Self>) -> impl IntoElement {
        let now = now_ms();
        let rows = self.clips.iter().enumerate().map(|(ix, clip)| {
            let (id, pinned) = (clip.id, clip.pinned);
            let pasted = format!("pasted {}", one_line(&clip.preview, 32));
            // `.rowline`: the preview, then age, pin, Paste and delete.
            h_flex()
                .gap(px(12.))
                .px(px(12.))
                .py(px(9.))
                .mb(px(6.))
                .rounded(px(8.))
                .border_1()
                .border_color(rgb(LINE))
                .bg(rgb(RAISED))
                .child(
                    h_flex()
                        .flex_1()
                        .min_w_0()
                        .gap(px(8.))
                        .child(mono(12.5).min_w_0().truncate().child(one_line(&clip.preview, 200)))
                        .when(clip.duplicate_run > 1, |this| {
                            this.child(
                                mono(12.5).flex_none().text_color(rgb(MUTED)).child(format!("×{}", clip.duplicate_run)),
                            )
                        }),
                )
                .child(
                    h_flex()
                        .gap(px(6.))
                        .child(mono(10.5).text_color(rgb(MUTED)).child(age(now, clip.captured_at)))
                        .child(button(("pin", ix), if pinned { "★" } else { "☆" }, Kind::Ghost, true).on_click(
                            cx.listener(move |this, _, _, cx| {
                                this.send(Action::HistoryPin { id, pinned: !pinned }, cx, |this, _, cx| {
                                    this.load_history(cx)
                                });
                            }),
                        ))
                        .child(button(("paste", ix), "Paste", Kind::Ghost, true).on_click(cx.listener(
                            move |this, _, _, cx| {
                                let pasted = pasted.clone();
                                this.send(Action::PasteId { id }, cx, move |this, _, cx| this.toast(pasted, cx));
                            },
                        )))
                        .child(button(("delete", ix), "✕", Kind::GhostDanger, true).on_click(cx.listener(
                            move |this, _, _, cx| {
                                this.send(Action::HistoryDelete { id }, cx, |this, _, cx| this.load_history(cx));
                            },
                        ))),
                )
        });

        let clear = |id: &'static str, label: &'static str, kind: Kind, keep_pinned: bool| {
            button(id, label, kind, true).on_click(cx.listener(move |this, _, _, cx| {
                this.send(Action::HistoryClear { keep_pinned }, cx, |this, _, cx| this.load_history(cx));
            }))
        };

        v_flex()
            .child(heading("History", "Every copy the daemon recorded. Paste one; pin what you want to keep."))
            // `.search`: a mono field, 340px at most.
            .child(
                div()
                    .max_w(px(340.))
                    .mb(px(16.))
                    .px(px(4.))
                    .rounded(px(8.))
                    .border_1()
                    .border_color(rgb(LINE))
                    .bg(rgb(SURFACE))
                    .font_family(MONO)
                    .text_size(px(12.5))
                    .child(Input::new(&self.search).appearance(false).cleanable(true)),
            )
            .child(
                h_flex()
                    .gap(px(8.))
                    .mb(px(14.))
                    .child(clear("clear-unpinned", "Clear unpinned", Kind::Ghost, true))
                    .child(clear("clear-all", "Clear all", Kind::GhostDanger, false)),
            )
            .when(self.clips.is_empty(), |this| this.child(empty("No clips.")))
            .children(rows)
            .when(self.truncated, |this| this.child(empty("Search stopped at the scan limit.")))
    }

    pub fn session_screen(&self, cx: &Context<Self>) -> impl IntoElement {
        let session = self.session();
        let (title, subtitle) = match session {
            None if self.paused() => {
                ("Paused".to_string(), "Capture is paused — copies are not being recorded.")
            }
            None => ("Session".to_string(), "No mode active — the clipboard behaves normally."),
            Some(s) => {
                let mode = s.mode.as_str();
                let title = mode[..1].to_uppercase() + &mode[1..];
                let subtitle = if s.state == SessionState::Capturing {
                    "Capturing — copies are being collected. The first paste seals it."
                } else {
                    "Traversing — your own paste chord advances the cursor."
                };
                (title, subtitle)
            }
        };

        let controls: [(&'static str, &'static str, Kind, Action); 8] = [
            ("paste-mode", "Paste next", Kind::Primary, Action::PasteMode { inject: true }),
            ("stack-start", "Start stack", Kind::Plain, Action::StackStart { duplicates: None }),
            ("queue-capture", "Capture queue", Kind::Plain, Action::QueueCapture { duplicates: None }),
            ("queue-seal", "Seal queue", Kind::Plain, Action::QueueSeal),
            (
                "group-capture",
                "Capture group",
                Kind::Plain,
                Action::GroupCapture { delimiter: None, duplicates: None },
            ),
            ("group-paste", "Paste group", Kind::Plain, Action::GroupPaste),
            ("session-reset", "Reset", Kind::Plain, Action::SessionReset),
            ("session-stop", "Stop", Kind::Danger, Action::SessionStop),
        ];
        let buttons = controls.into_iter().map(|(id, label, kind, action)| {
            button(id, label, kind, false).on_click(cx.listener(move |this, _, _, cx| {
                this.send(action.clone(), cx, |this, _, cx| this.toast("done", cx));
            }))
        });

        // `Active`: one `.rowline` per detail, key muted, value plain.
        let details = session.map(|s| {
            let mut rows = vec![
                ("state", format!("{:?}", s.state).to_lowercase()),
                ("duplicates", format!("{:?}", s.duplicate_policy).to_lowercase()),
                ("size", s.size.to_string()),
                ("cursor", format!("{} / {}", s.cursor, s.size)),
                ("remaining", s.remaining.to_string()),
            ];
            if let Some(delimiter) = &s.delimiter {
                rows.push(("delimiter", format!("{delimiter:?}")));
            }
            card("Active").children(rows.into_iter().map(|(key, value)| {
                mono(12.5)
                    .flex()
                    .gap(px(15.))
                    .px(px(12.))
                    .py(px(9.))
                    .mb(px(6.))
                    .rounded(px(8.))
                    .border_1()
                    .border_color(rgb(LINE))
                    .bg(rgb(RAISED))
                    .child(div().text_color(rgb(MUTED)).child(key))
                    .child(value)
            }))
        });

        let items = session.map(|s| {
            let items = self.items_for(s.id);
            card("Items")
                .child(
                    div()
                        .text_size(px(11.5))
                        .text_color(rgb(FAINT))
                        .mt(px(-6.))
                        .mb(px(12.))
                        .child("In paste order. Use ▲ ▼ to reorder. Pasted items stay put."),
                )
                .when(items.is_empty(), |this| {
                    this.child(mono(12.).text_color(rgb(FAINT)).py(px(8.)).child("Nothing collected yet — copy something."))
                })
                .child(v_flex().gap(px(5.)).children(
                    items.iter().enumerate().map(|(ix, clip)| self.session_row(ix, clip, s.cursor, cx)),
                ))
        });

        v_flex()
            .child(heading(title, subtitle))
            .children(details)
            .children(items)
            .child(card("Controls").child(h_flex().flex_wrap().gap(px(8.)).children(buttons)))
    }

    /// One `.sl-row`, with ▲/▼ to reorder it. Items already pasted stay put,
    /// dimmed; nothing can move in front of the cursor.
    fn session_row(
        &self,
        ix: usize,
        clip: &ClipSummary,
        cursor: usize,
        cx: &Context<Self>,
    ) -> AnyElement {
        let last = self.items.len() - 1;
        let (done, next) = (ix < cursor, ix == cursor);
        let mover = |id: &'static str, glyph: &'static str, to: usize, disabled: bool| {
            let clip_id = clip.id;
            glyph_button((id, ix), glyph, 24., 24., disabled)
                .border_color(rgba(0))
                .bg(rgba(0))
                .text_size(px(9.))
                .when(!disabled, |this| {
                    this.hover(|style| style.bg(rgb(RAISED_2)).border_color(rgb(LINE))).on_click(cx.listener(
                        move |this, _, _, cx| {
                            this.send(Action::SessionMove { from: ix, to, id: clip_id }, cx, |this, _, cx| {
                                this.load_items(cx)
                            });
                        },
                    ))
                })
        };
        let tag = (done || next).then(|| {
            mono(9.5)
                .text_color(rgb(if next { ACTIVE } else { FAINT }))
                .child(spaced(if next { "NEXT" } else { "PASTED" }, 1.15))
        });

        let row = mono(12.)
            .flex()
            .items_center()
            .gap(px(8.))
            .pl(px(10.))
            .pr(px(8.))
            .py(px(6.))
            .min_h(px(38.))
            .bg(rgb(RAISED))
            .when(done, |this| this.opacity(0.5))
            .child(div().w(px(20.)).flex_none().text_right().text_size(px(10.5)).text_color(rgb(FAINT)).child((ix + 1).to_string()))
            .child(div().flex_1().min_w_0().truncate().child(one_line(&clip.preview, 160)))
            .children(tag)
            .when(!done, |this| {
                this.child(
                    h_flex()
                        .gap(px(2.))
                        .child(mover("up", "▲", ix.saturating_sub(1).max(cursor), ix == cursor))
                        .child(mover("down", "▼", (ix + 1).min(last), ix == last)),
                )
            });
        if next {
            accent_edge(row.bg(rgb(RAISED)), rgba(ACTIVE_LINE), 8.).into_any_element()
        } else {
            row.rounded(px(8.)).border_1().border_color(rgb(LINE)).into_any_element()
        }
    }

    pub fn settings_screen(&self, cx: &Context<Self>) -> impl IntoElement {
        let checks = self.doctor.iter().flat_map(|doctor| {
            let platform =
                check_row(None, "platform", format!("{} — {}", doctor.display_server, doctor.platform_support));
            std::iter::once(platform)
                .chain(doctor.checks.iter().map(move |c| check_row(Some(c.status), &c.name, c.detail.clone())))
        });

        v_flex()
            .child(heading("Settings", "What works on this machine, and what Copycat needs from it."))
            .when(self.permission_blocked(), |this| this.child(self.permission_callout(cx)))
            .child(
                card("Diagnostics")
                    .when(self.doctor.is_none(), |this| this.child(empty("No diagnostics yet.")))
                    .children(checks),
            )
    }

    /// When the event tap is refused it is almost always Accessibility not
    /// granted to Copycat itself: the app launches the daemon, so macOS
    /// attributes the daemon's keyboard access to the app. Say so, offer the
    /// exact panes, and a restart, since a refusal sticks until a new process.
    fn permission_callout(&self, cx: &Context<Self>) -> impl IntoElement {
        let restart = button("restart", "Restart daemon", Kind::Ghost, false).on_click(cx.listener(
            |this, _, _, cx| {
                this.toast("restarting the daemon…", cx);
                cx.spawn(async move |this, cx| {
                    cx.background_spawn(async { copycat_launcher::restart_daemon() }).await;
                    let _ = this.update(cx, |view, cx| view.load_doctor(cx));
                })
                .detach();
            },
        ));
        let buttons = h_flex().gap(px(8.)).flex_wrap();
        #[cfg(target_os = "macos")]
        let buttons = buttons
            .child(
                button("open-ax", "Open Accessibility", Kind::Primary, false)
                    .on_click(|_, _, _| open_pane("Privacy_Accessibility")),
            )
            .child(
                button("open-listen", "Open Input Monitoring", Kind::Plain, false)
                    .on_click(|_, _, _| open_pane("Privacy_ListenEvent")),
            );

        // A card with its h2 in orange, over an orange-tinted border.
        card_in("Grant permission to Copycat", ACTIVE)
            .border_color(rgba(0xE4672B66))
            .child(
                div()
                    .text_color(rgb(MUTED))
                    .mb(px(12.))
                    .child("Copycat launches the daemon, so macOS attributes its keyboard access to Copycat — not to a terminal. Turn Copycat on under Accessibility, then restart the daemon."),
            )
            .child(buttons.child(restart))
            .child(div().mt(px(5.)).text_size(px(11.5)).text_color(rgb(FAINT)).child(
                "Accessibility lets the daemon catch your hotkeys and paste chord, and type pastes. Already on? Remove Copycat with − and add it again.",
            ))
    }
}

/// `.empty`: a quiet line where a list would be.
fn empty(text: &'static str) -> Div {
    div().text_color(rgb(FAINT)).py(px(14.)).px(px(2.)).child(text)
}

/// Open a macOS Privacy & Security pane, so the user does not have to hunt for it.
#[cfg(target_os = "macos")]
fn open_pane(pane: &str) {
    let url = format!("x-apple.systempreferences:com.apple.preference.security?{pane}");
    let _ = std::process::Command::new("open").arg(url).spawn();
}

/// One `.check` line: the status pill, the check's name, its detail.
fn check_row(status: Option<CheckStatus>, name: &str, detail: String) -> impl IntoElement {
    let pill = status.map(|status| {
        let (word, color, border) = match status {
            CheckStatus::Ok => ("OK", OK, 0x63C08859),
            CheckStatus::Degraded => ("DEGRADED", WARN, 0xD2A84A59),
            CheckStatus::Unavailable => ("UNAVAILABLE", BAD, 0xE0705E66),
        };
        mono(10.)
            .w(px(88.))
            .flex()
            .justify_center()
            .py(px(2.))
            .rounded_full()
            .bg(rgb(RAISED))
            .border_1()
            .border_color(rgba(border))
            .text_color(rgb(color))
            .child(spaced(word, 1.))
    });
    h_flex()
        .items_start()
        .gap(px(14.))
        .py(px(8.))
        .border_b_1()
        .border_color(rgb(LINE_SOFT))
        .text_size(px(12.5))
        .child(div().w(px(96.)).flex_none().children(pill))
        .child(mono(12.).w(px(150.)).flex_none().child(name.to_string()))
        .child(div().flex_1().min_w_0().text_color(rgb(MUTED)).child(detail))
}

/// A clip preview on one line, cut to `max` characters.
pub fn one_line(text: &str, max: usize) -> String {
    let flat = text.split_whitespace().collect::<Vec<_>>().join(" ");
    if flat.is_empty() {
        return "(empty)".into();
    }
    match flat.char_indices().nth(max) {
        Some((cut, _)) => format!("{}…", &flat[..cut]),
        None => flat,
    }
}

pub fn now_ms() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |d| d.as_millis() as i64)
}

/// How long ago, in the Tauri app's compact form: 12s, 4m, 3h, 2d.
pub fn age(now_ms: i64, then_ms: i64) -> String {
    let s = (now_ms - then_ms).max(0) / 1000;
    match s {
        0..60 => format!("{s}s"),
        60..3600 => format!("{}m", s / 60),
        3600..86400 => format!("{}h", s / 3600),
        _ => format!("{}d", s / 86400),
    }
}

#[cfg(test)]
mod tests {
    use super::{age, one_line};

    #[test]
    fn previews_flatten_and_cut_on_char_boundaries() {
        assert_eq!(one_line("a\n  b\tc", 10), "a b c");
        assert_eq!(one_line("héllo wörld", 5), "héllo…");
        assert_eq!(one_line("  ", 5), "(empty)");
    }

    #[test]
    fn ages_use_the_largest_whole_unit() {
        assert_eq!(age(59_000, 0), "59s");
        assert_eq!(age(61_000, 0), "1m");
        assert_eq!(age(7_200_000, 0), "2h");
        assert_eq!(age(0, 5_000), "0s");
    }
}
