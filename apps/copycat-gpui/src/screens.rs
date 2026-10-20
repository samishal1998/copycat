//! The three screens, each a thin view over daemon actions — the same actions
//! the Tauri app's History, Session and Settings screens send.

use copycat_core::{ClipSummary, SessionState};
use copycat_protocol::{Action, CheckStatus};
use gpui_kit::component::button::{Button, ButtonVariants as _};
use gpui_kit::component::input::Input;
use gpui_kit::component::scroll::ScrollableElement as _;
use gpui_kit::component::tag::Tag;
use gpui_kit::component::{
    ActiveTheme as _, Disableable as _, IconName, Sizable as _, h_flex, v_flex,
};
use gpui_kit::prelude::*;
use gpui_kit::*;

use crate::app::AppView;

impl AppView {
    pub fn history_screen(&self, cx: &Context<Self>) -> impl IntoElement {
        let (muted, hover) = (cx.theme().muted_foreground, cx.theme().list_hover);
        let now = now_ms();

        let rows = self.clips.iter().enumerate().map(|(ix, clip)| {
            let (id, pinned) = (clip.id, clip.pinned);
            let preview = one_line(&clip.preview, 120);
            let pasted = format!("pasted {}", one_line(&clip.preview, 32));
            h_flex()
                .id(("clip", ix))
                .w_full()
                .gap_2()
                .px_2()
                .py_1()
                .rounded_md()
                .cursor_pointer()
                .hover(move |style| style.bg(hover))
                .on_click(cx.listener(move |this, _, _, cx| {
                    let pasted = pasted.clone();
                    this.send(Action::PasteId { id }, cx, move |this, _, cx| this.toast(pasted, cx));
                }))
                .child(div().flex_1().min_w_0().truncate().child(preview))
                .when(clip.duplicate_run > 1, |this| {
                    this.child(div().text_xs().text_color(muted).child(format!("×{}", clip.duplicate_run)))
                })
                .child(div().text_xs().text_color(muted).child(age(now, clip.captured_at)))
                .child(
                    Button::new(("pin", ix))
                        .ghost()
                        .xsmall()
                        .icon(if pinned { IconName::StarFill } else { IconName::Star })
                        .tooltip(if pinned { "Unpin" } else { "Pin" })
                        .on_click(cx.listener(move |this, _, _, cx| {
                            // A Button lets the click bubble on to the row,
                            // which would paste the clip too.
                            cx.stop_propagation();
                            this.send(Action::HistoryPin { id, pinned: !pinned }, cx, |this, _, cx| {
                                this.load_history(cx)
                            });
                        })),
                )
                .child(
                    Button::new(("delete", ix))
                        .ghost()
                        .xsmall()
                        .icon(IconName::Close)
                        .tooltip("Delete")
                        .on_click(cx.listener(move |this, _, _, cx| {
                            cx.stop_propagation();
                            this.send(Action::HistoryDelete { id }, cx, |this, _, cx| this.load_history(cx));
                        })),
                )
        });

        v_flex()
            .size_full()
            .gap_3()
            .child(
                h_flex()
                    .gap_2()
                    .child(div().flex_1().child(Input::new(&self.search).cleanable(true)))
                    .child(Button::new("clear-unpinned").small().label("Clear unpinned").on_click(
                        cx.listener(|this, _, _, cx| {
                            this.send(Action::HistoryClear { keep_pinned: true }, cx, |this, _, cx| {
                                this.load_history(cx)
                            });
                        }),
                    ))
                    .child(Button::new("clear-all").small().danger().label("Clear all").on_click(
                        cx.listener(|this, _, _, cx| {
                            this.send(Action::HistoryClear { keep_pinned: false }, cx, |this, _, cx| {
                                this.load_history(cx)
                            });
                        }),
                    )),
            )
            .child(
                v_flex()
                    .flex_1()
                    .min_h_0()
                    .overflow_y_scrollbar()
                    .when(self.clips.is_empty(), |this| {
                        this.child(div().p_4().text_color(muted).child("No clips."))
                    })
                    .children(rows)
                    .when(self.truncated, |this| {
                        this.child(
                            div().p_2().text_xs().text_color(muted).child("Search stopped at the scan limit."),
                        )
                    }),
            )
    }

    pub fn session_screen(&self, cx: &Context<Self>) -> impl IntoElement {
        let muted = cx.theme().muted_foreground;
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

        let controls: [(&'static str, &'static str, Action); 8] = [
            ("paste-mode", "Paste next", Action::PasteMode { inject: true }),
            ("stack-start", "Start stack", Action::StackStart { duplicates: None }),
            ("queue-capture", "Capture queue", Action::QueueCapture { duplicates: None }),
            ("queue-seal", "Seal queue", Action::QueueSeal),
            ("group-capture", "Capture group", Action::GroupCapture { delimiter: None, duplicates: None }),
            ("group-paste", "Paste group", Action::GroupPaste),
            ("session-reset", "Reset", Action::SessionReset),
            ("session-stop", "Stop", Action::SessionStop),
        ];
        let buttons = controls.into_iter().map(|(id, label, action)| {
            let button = Button::new(id).small().label(label);
            let button = match id {
                "paste-mode" => button.primary(),
                "session-stop" => button.danger(),
                _ => button,
            };
            button.on_click(cx.listener(move |this, _, _, cx| {
                this.send(action.clone(), cx, |this, _, cx| this.toast("done", cx));
            }))
        });

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
            v_flex().gap_1().text_sm().children(rows.into_iter().map(|(key, value)| {
                h_flex().gap_3().child(div().w(px(90.)).text_color(muted).child(key)).child(value)
            }))
        });

        v_flex()
            .size_full()
            .gap_3()
            .child(div().text_xl().font_weight(FontWeight::SEMIBOLD).child(title))
            .child(div().text_sm().text_color(muted).child(subtitle))
            .child(h_flex().flex_wrap().gap_2().children(buttons))
            .children(details)
            .when_some(session, |this, s| {
                this.child(
                    v_flex()
                        .flex_1()
                        .min_h_0()
                        .overflow_y_scrollbar()
                        .when(self.items_for(s.id).is_empty(), |this| {
                            this.child(
                                div().p_2().text_color(muted).child("Nothing collected yet — copy something."),
                            )
                        })
                        .children(self.items_for(s.id).iter().enumerate().map(|(ix, clip)| {
                            self.session_row(ix, clip, s.cursor, cx)
                        })),
                )
            })
    }

    /// One session item, with ▲/▼ to reorder it. Items already pasted stay put,
    /// dimmed; nothing can move in front of the cursor.
    fn session_row(
        &self,
        ix: usize,
        clip: &ClipSummary,
        cursor: usize,
        cx: &Context<Self>,
    ) -> impl IntoElement {
        let muted = cx.theme().muted_foreground;
        let last = self.items.len() - 1;
        let done = ix < cursor;
        let tag = if done {
            Some(Tag::secondary().small().child("pasted"))
        } else if ix == cursor {
            Some(Tag::info().small().child("next"))
        } else {
            None
        };
        let mover = |id: &'static str, icon: IconName, to: usize, disabled: bool| {
            let clip_id = clip.id;
            Button::new((id, ix))
                .ghost()
                .xsmall()
                .icon(icon)
                .disabled(disabled)
                .on_click(cx.listener(move |this, _, _, cx| {
                    this.send(Action::SessionMove { from: ix, to, id: clip_id }, cx, |this, _, cx| {
                        this.load_items(cx)
                    });
                }))
        };

        h_flex()
            .w_full()
            .gap_2()
            .px_2()
            .py_1()
            .when(done, |this| this.opacity(0.5))
            .child(div().w(px(24.)).text_xs().text_color(muted).child((ix + 1).to_string()))
            .child(div().flex_1().min_w_0().truncate().child(one_line(&clip.preview, 120)))
            .children(tag)
            .when(!done, |this| {
                this.child(mover("up", IconName::ChevronUp, ix.saturating_sub(1).max(cursor), ix == cursor))
                    .child(mover("down", IconName::ChevronDown, (ix + 1).min(last), ix == last))
            })
    }

    pub fn settings_screen(&self, cx: &Context<Self>) -> impl IntoElement {
        let muted = cx.theme().muted_foreground;
        let checks = self.doctor.iter().flat_map(|doctor| {
            let platform = check_row(
                None,
                "platform",
                format!("{} — {}", doctor.display_server, doctor.platform_support),
                muted,
            );
            std::iter::once(platform).chain(doctor.checks.iter().map(move |c| {
                check_row(Some(c.status), &c.name, c.detail.clone(), muted)
            }))
        });

        v_flex()
            .size_full()
            .gap_3()
            .child(div().text_xl().font_weight(FontWeight::SEMIBOLD).child("Diagnostics"))
            .when(self.permission_blocked(), |this| this.child(self.permission_callout(cx)))
            .child(
                v_flex()
                    .flex_1()
                    .min_h_0()
                    .gap_2()
                    .overflow_y_scrollbar()
                    .when(self.doctor.is_none(), |this| {
                        this.child(div().text_color(muted).child("No diagnostics yet."))
                    })
                    .children(checks),
            )
    }

    /// When the event tap is refused it is almost always Accessibility not
    /// granted to Copycat itself: the app launches the daemon, so macOS
    /// attributes the daemon's keyboard access to the app. Say so, offer the
    /// exact panes, and a restart, since a refusal sticks until a new process.
    fn permission_callout(&self, cx: &Context<Self>) -> impl IntoElement {
        let theme = cx.theme();
        let restart = Button::new("restart").small().label("Restart daemon").on_click(cx.listener(
            |this, _, _, cx| {
                this.toast("restarting the daemon…", cx);
                cx.spawn(async move |this, cx| {
                    cx.background_spawn(async { copycat_launcher::restart_daemon() }).await;
                    let _ = this.update(cx, |view, cx| view.load_doctor(cx));
                })
                .detach();
            },
        ));
        let buttons = h_flex().gap_2().flex_wrap();
        #[cfg(target_os = "macos")]
        let buttons = buttons
            .child(Button::new("open-ax").small().primary().label("Open Accessibility").on_click(
                |_, _, _| open_pane("Privacy_Accessibility"),
            ))
            .child(Button::new("open-listen").small().label("Open Input Monitoring").on_click(
                |_, _, _| open_pane("Privacy_ListenEvent"),
            ));

        v_flex()
            .gap_2()
            .p_3()
            .rounded_lg()
            .border_1()
            .border_color(theme.warning)
            .child(div().font_weight(FontWeight::SEMIBOLD).child("Grant permission to Copycat"))
            .child(div().text_sm().text_color(theme.muted_foreground).child(
                "Copycat launches the daemon, so macOS attributes its keyboard access to Copycat — \
                 not to a terminal. Turn Copycat on under Accessibility, then restart the daemon.",
            ))
            .child(buttons.child(restart))
    }
}

/// Open a macOS Privacy & Security pane, so the user does not have to hunt for it.
#[cfg(target_os = "macos")]
fn open_pane(pane: &str) {
    let url = format!("x-apple.systempreferences:com.apple.preference.security?{pane}");
    let _ = std::process::Command::new("open").arg(url).spawn();
}

fn check_row(status: Option<CheckStatus>, name: &str, detail: String, muted: Hsla) -> impl IntoElement {
    let pill = status.map(|status| {
        let tag = match status {
            CheckStatus::Ok => Tag::success(),
            CheckStatus::Degraded => Tag::warning(),
            CheckStatus::Unavailable => Tag::danger(),
        };
        tag.small().child(status.glyph())
    });
    h_flex()
        .items_start()
        .gap_3()
        .text_sm()
        .child(div().w(px(90.)).flex_none().children(pill))
        .child(div().w(px(160.)).flex_none().font_weight(FontWeight::MEDIUM).child(name.to_string()))
        .child(div().flex_1().min_w_0().text_color(muted).child(detail))
}

/// A clip preview on one line, cut to `max` characters.
fn one_line(text: &str, max: usize) -> String {
    let flat = text.split_whitespace().collect::<Vec<_>>().join(" ");
    if flat.is_empty() {
        return "(empty)".into();
    }
    match flat.char_indices().nth(max) {
        Some((cut, _)) => format!("{}…", &flat[..cut]),
        None => flat,
    }
}

fn now_ms() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |d| d.as_millis() as i64)
}

/// How long ago, in the Tauri app's compact form: 12s, 4m, 3h, 2d.
fn age(now_ms: i64, then_ms: i64) -> String {
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
