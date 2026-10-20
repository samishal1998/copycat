//! The tray dropdown: the Tauri menu-bar panel's content (mode box, a paged
//! slice of history, the capture shortcuts) over the same shared `Daemon` the
//! main window watches. It is a new view each time it opens and is dropped
//! when it closes.

use std::ops::Range;
use std::time::Duration;

use copycat_core::{ClipSummary, SessionState};
use copycat_protocol::{Action, ResultBody};
use gpui_kit::component::scroll::ScrollableElement as _;
use gpui_kit::component::{h_flex, v_flex};
use gpui_kit::prelude::*;
use gpui_kit::*;

use crate::app::{Daemon, history_mark, injects_paste, request};
use crate::brand::*;
use crate::settings::{PAGE_SIZES, Settings};
use crate::screens::{age, now_ms, one_line};
use crate::tray;

/// How deep the dropdown reaches into history.
const MAX_FETCH: usize = 60;

pub struct Dropdown {
    daemon: Entity<Daemon>,
    clips: Vec<ClipSummary>,
    page: usize,
    /// `history_mark` when the list was last fetched.
    mark: Option<(Option<copycat_core::ClipId>, usize)>,
    /// Whether the panel has been key yet; see `new`.
    was_active: bool,
    toast: Option<SharedString>,
    toast_gen: u64,
    _subs: Vec<Subscription>,
}

impl Dropdown {
    pub fn new(window: &mut Window, cx: &mut Context<Self>) -> Self {
        let daemon = Daemon::get(cx);
        let _subs = vec![
            cx.observe(&daemon, |this, _, cx| this.refresh_if_changed(cx)),
            // The page size is also set from the main window's Settings.
            cx.observe(&Settings::get(cx), |_, _, cx| cx.notify()),
            // Close on blur, as a menu-bar dropdown does. Only after it has
            // been key: GPUI can resign a window's key status as it opens.
            cx.observe_window_activation(window, |this: &mut Self, window, cx| {
                if window.is_window_active() {
                    this.was_active = true;
                } else if this.was_active {
                    tray::close(window, true, cx);
                }
            }),
        ];
        let mut this = Dropdown {
            daemon,
            clips: Vec::new(),
            page: 0,
            mark: None,
            was_active: false,
            toast: None,
            toast_gen: 0,
            _subs,
        };
        this.refresh_if_changed(cx);
        this
    }

    /// Refetch the list when the daemon says it may have changed, instead of
    /// the Tauri panel's 1.5s timer.
    fn refresh_if_changed(&mut self, cx: &mut Context<Self>) {
        let mark = history_mark(self.daemon.read(cx).status.as_ref());
        if mark.is_some() && mark != self.mark {
            self.mark = mark;
            self.load_history(cx);
        }
        cx.notify();
    }

    fn load_history(&mut self, cx: &mut Context<Self>) {
        self.send(Action::HistoryList { limit: MAX_FETCH, raw: false }, None, cx, |this, body, _| {
            if let ResultBody::Clips { clips, .. } = body {
                this.clips = clips;
            }
        });
    }

    /// Send one action; an error becomes the toast. A paste closes the panel
    /// and hands focus back first, so the chord lands in the user's app; it
    /// passes `window` for that. Not while offline: nothing would be pasted,
    /// and the error that says so would land in a closed panel.
    fn send(
        &mut self,
        action: Action,
        window: Option<&mut Window>,
        cx: &mut Context<Self>,
        then: impl FnOnce(&mut Self, ResultBody, &mut Context<Self>) + 'static,
    ) {
        if let Some(window) = window
            && injects_paste(&action)
            && self.daemon.read(cx).connected
        {
            tray::close(window, false, cx);
            tray::deactivate();
            // This view goes with the panel, so the reply goes to the Daemon,
            // which outlives it: a failure opens the main window to show it.
            // Deferred, since a new main window reads the Daemon while this
            // callback still holds it.
            self.daemon.update(cx, |_, cx| {
                request(action, cx, |daemon, reply, cx| {
                    if let Err(error) = reply {
                        daemon.error = Some(error.message.into());
                        cx.defer(tray::show_main);
                    }
                })
            });
            return;
        }
        request(action, cx, move |this, reply, cx| match reply {
            Ok(body) => then(this, body, cx),
            Err(error) => this.toast(error.message, cx),
        });
    }

    fn toast(&mut self, text: impl Into<SharedString>, cx: &mut Context<Self>) {
        self.toast = Some(text.into());
        self.toast_gen += 1;
        let generation = self.toast_gen;
        cx.spawn(async move |this, cx| {
            cx.background_executor().timer(Duration::from_millis(2200)).await;
            let _ = this.update(cx, |this, cx| {
                if this.toast_gen == generation {
                    this.toast = None;
                    cx.notify();
                }
            });
        })
        .detach();
        cx.notify();
    }

    fn set_page_size(&mut self, size: usize, cx: &mut Context<Self>) {
        Settings::get(cx).update(cx, |settings, cx| settings.change(cx, |s| s.page_size = size));
        self.page = 0;
        cx.notify();
    }

    /// The mode name and its count line, and the next clip's preview while a
    /// session is traversing. Same derivation as `setMode` in the Tauri panel.
    fn mode(&self, cx: &App) -> (String, String, Option<String>) {
        let core = self.daemon.read(cx).status.as_ref().map(|s| &s.core);
        let Some(session) = core.and_then(|core| core.session.as_ref()) else {
            let paused = core.is_some_and(|core| core.paused);
            let latest = core
                .and_then(|core| core.latest.as_ref())
                .map(|clip| format!("latest · {}", one_line(&clip.preview, 26)));
            let name = if paused { "PAUSED" } else { "NORMAL" };
            return (name.into(), latest.unwrap_or_default(), None);
        };
        let name = session.mode.as_str().to_uppercase();
        if session.state == SessionState::Capturing {
            return (name, format!("capturing · {} collected", session.size), None);
        }
        let count = format!("{} / {} · {} left", session.cursor, session.size, session.remaining);
        let next = session.next.map(|id| match self.clips.iter().find(|clip| clip.id == id) {
            Some(clip) => one_line(&clip.preview, 60),
            None => format!("#{}", id.0),
        });
        (name, count, next)
    }
}

/// Which rows page `page` shows, with the page pulled back inside the list
/// when the list has shrunk under it.
fn page_rows(total: usize, page: usize, size: usize) -> (usize, Range<usize>) {
    let page = page.min(total.saturating_sub(1) / size);
    let start = page * size;
    (page, start..(start + size).min(total))
}

impl Render for Dropdown {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let daemon = self.daemon.read(cx);
        let (connected, paused) =
            (daemon.connected, daemon.status.as_ref().is_some_and(|s| s.core.paused));
        let in_session = daemon.status.as_ref().is_some_and(|s| s.core.session.is_some());
        let (name, count, next) = self.mode(cx);
        let size = Settings::get(cx).read(cx).page_size;
        let (page, rows) = page_rows(self.clips.len(), self.page, size);
        self.page = page;
        let total = self.clips.len();
        let pager = if total == 0 {
            "empty".to_string()
        } else {
            format!("{}–{} of {}", rows.start + 1, rows.end, total)
        };
        let now = now_ms();

        // `.head`: mark, wordmark, live dot, and ⤢ to the full window.
        let header = h_flex()
            .px(px(14.))
            .py(px(12.))
            .gap(px(10.))
            .border_b_1()
            .border_color(rgb(LINE_SOFT))
            .child(mark(20.))
            .child(mono(14.).font_weight(FontWeight::BOLD).child("copycat"))
            .child(dot(connected, true))
            .child(div().flex_1())
            .child(glyph_button("open-main", "⤢", 24., 24., false).rounded(px(7.)).text_size(px(13.)).on_click(
                cx.listener(|_, _, window, cx| {
                    tray::close(window, false, cx);
                    tray::show_main(cx);
                }),
            ));

        // `.mode`: washed orange while a session runs, plain otherwise.
        let mode = v_flex()
            .px(px(14.))
            .pt(px(13.))
            .pb(px(14.))
            .border_b_1()
            .border_color(rgb(LINE_SOFT))
            .when(in_session, |this| {
                this.bg(linear_gradient(
                    180.,
                    linear_color_stop(rgba(WASH), 0.),
                    linear_color_stop(rgba(0xE4672B00), 0.92),
                ))
            })
            .child(
                h_flex()
                    .gap(px(10.))
                    .child(
                        mono(12.5)
                            .font_weight(FontWeight::BOLD)
                            .text_color(rgb(if in_session { ACTIVE } else { MUTED }))
                            .child(spaced(&name, 2.)),
                    )
                    .child(div().flex_1())
                    .child(if in_session {
                        count_line(&count)
                    } else {
                        mono(11.5).min_w_0().truncate().text_color(rgb(MUTED)).child(count)
                    }),
            )
            .when_some(next, |this, next| {
                this.child(div().mt(px(12.)).child(accent_edge(
                    h_flex()
                        .gap(px(10.))
                        .px(px(11.))
                        .py(px(10.))
                        .bg(rgb(RAISED_2))
                        .child(mono(12.).flex_1().min_w_0().truncate().child(next))
                        .child(mono(9.).flex_none().text_color(rgb(ACTIVE)).child(spaced("NEXT ⌘V", 1.26))),
                    rgba(ACTIVE_LINE),
                    8.,
                )))
            });

        // `.list-head`: the heading and the − n + rows-per-page stepper.
        let step = |id: &'static str, glyph: &'static str, to: usize, disabled: bool| {
            glyph_button(id, glyph, 18., 18., disabled).text_size(px(12.)).when(!disabled, |this| {
                this.on_click(cx.listener(move |this, _, _, cx| this.set_page_size(to, cx)))
            })
        };
        let list_head = h_flex()
            .px(px(14.))
            .pt(px(12.))
            .pb(px(4.))
            .gap(px(8.))
            .child(div().font_family(DISPLAY).font_weight(FontWeight::BOLD).text_size(px(13.)).child("History"))
            .child(div().flex_1())
            .child(
                h_flex()
                    .gap(px(2.))
                    .child(step("size-down", "−", size.saturating_sub(1), size <= *PAGE_SIZES.start()))
                    .child(mono(10.5).min_w(px(14.)).px(px(2.)).text_center().text_color(rgb(MUTED)).child(size.to_string()))
                    .child(step("size-up", "+", size + 1, size >= *PAGE_SIZES.end())),
            );

        let start = rows.start;
        let list = v_flex()
            .flex_1()
            .min_h_0()
            .px(px(8.))
            .pt(px(4.))
            .pb(px(6.))
            .overflow_y_scrollbar()
            .when(total == 0, |this| {
                this.child(div().py(px(16.)).px(px(8.)).text_center().text_size(px(12.)).text_color(rgb(FAINT)).child("Nothing copied yet."))
            })
            .children(self.clips[rows.clone()].iter().enumerate().map(|(i, clip)| {
                let id = clip.id;
                let pasted = format!("pasted {}", one_line(&clip.preview, 32));
                h_flex()
                    .id(("row", i))
                    .flex_none()
                    .gap(px(10.))
                    .p(px(8.))
                    .rounded(px(8.))
                    .cursor_pointer()
                    .hover(|style| style.bg(rgb(RAISED)))
                    .on_click(cx.listener(move |this, _, window, cx| {
                        let pasted = pasted.clone();
                        this.send(Action::PasteId { id }, Some(window), cx, move |this, _, cx| {
                            this.toast(pasted, cx)
                        });
                    }))
                    .child(mono(10.5).w(px(20.)).flex_none().text_center().text_color(rgb(FAINT)).child((start + i + 1).to_string()))
                    .child(mono(12.5).flex_1().min_w_0().truncate().child(one_line(&clip.preview, 80)))
                    .child(
                        h_flex()
                            .flex_none()
                            .gap(px(8.))
                            .when(clip.duplicate_run > 1, |this| {
                                this.child(div().text_size(px(10.)).text_color(rgb(FAINT)).child(format!("×{}", clip.duplicate_run)))
                            })
                            .child(mono(10.5).text_color(rgb(FAINT)).child(age(now, clip.captured_at)))
                            .when(clip.pinned, |this| this.child(div().text_size(px(11.)).text_color(rgb(GOLD)).child("★"))),
                    )
            }));

        // `.pager`: ◀ the range ▶.
        let pager = h_flex()
            .justify_center()
            .gap(px(12.))
            .px(px(12.))
            .py(px(6.))
            .border_t_1()
            .border_color(rgb(LINE_SOFT))
            .child(glyph_button("prev", "◀", 24., 22., page == 0).text_size(px(11.)).when(page > 0, |this| {
                this.on_click(cx.listener(|this, _, _, cx| {
                    this.page = this.page.saturating_sub(1);
                    cx.notify();
                }))
            }))
            .child(mono(10.5).min_w(px(112.)).text_center().text_color(rgb(MUTED)).child(pager))
            .child(glyph_button("next", "▶", 24., 22., rows.end >= total).text_size(px(11.)).when(
                rows.end < total,
                |this| {
                    this.on_click(cx.listener(|this, _, _, cx| {
                        this.page += 1;
                        cx.notify();
                    }))
                },
            ));

        // `.foot`: the capture shortcuts as key caps, pause on the right. Each
        // refreshes the list, as in the Tauri panel.
        let act = |id: &'static str, key: &'static str, label: &'static str| {
            h_flex()
                .id(id)
                .gap(px(6.))
                .px(px(8.))
                .py(px(5.))
                .rounded(px(7.))
                .text_size(px(11.))
                .text_color(rgb(MUTED))
                .cursor_pointer()
                .hover(|style| style.bg(rgb(RAISED)).text_color(rgb(TEXT)))
                .child(
                    mono(10.5)
                        .min_w(px(17.))
                        .px(px(5.))
                        .text_center()
                        .text_color(rgb(TEXT))
                        .bg(rgb(RAISED))
                        .border_1()
                        .border_b_2()
                        .border_color(rgb(LINE))
                        .rounded(px(5.))
                        .child(key),
                )
                .child(label)
        };
        let shortcuts: [(&'static str, &'static str, Action); 3] = [
            ("stack", "S", Action::StackStart { duplicates: None }),
            ("queue", "Q", Action::QueueCapture { duplicates: None }),
            ("group", "G", Action::GroupCapture { delimiter: None, duplicates: None }),
        ];
        let footer = h_flex()
            .gap(px(6.))
            .px(px(12.))
            .py(px(9.))
            .border_t_1()
            .border_color(rgb(LINE_SOFT))
            .bg(rgb(GROUND))
            .children(shortcuts.into_iter().map(|(id, key, action)| {
                act(id, key, id).on_click(cx.listener(move |this, _, _, cx| {
                    this.send(action.clone(), None, cx, |this, _, cx| this.load_history(cx));
                }))
            }))
            .child(div().flex_1())
            .child(act("pause", "⏯", if paused { "resume" } else { "pause" }).on_click(cx.listener(
                move |this, _, _, cx| {
                    let action = if paused { Action::HistoryResume } else { Action::HistoryPause };
                    this.send(action, None, cx, |_, _, _| {});
                },
            )));

        v_flex()
            .relative()
            .size_full()
            .bg(rgb(SURFACE))
            .text_color(rgb(TEXT))
            .font_family(UI)
            .text_size(px(13.))
            .line_height(relative(1.5))
            .child(header)
            .child(mode)
            .child(list_head)
            .child(list)
            .child(pager)
            .child(footer)
            // `.toast`: over the list, just above the footer.
            .when_some(self.toast.clone(), |this, toast| {
                this.child(div().absolute().left(px(16.)).right(px(16.)).bottom(px(58.)).child(accent_edge(
                    mono(11.5).px(px(11.)).py(px(8.)).bg(rgb(RAISED_2)).truncate().child(toast),
                    rgb(LINE),
                    8.,
                )))
            })
    }
}

/// A session's count line with the numbers the Tauri panel sets in `<b>` in
/// full white: "1 / 4 · 3 left", "capturing · 2 collected". Word by word, a
/// mono space apart, since a GPUI text run takes one colour.
fn count_line(count: &str) -> Div {
    let words: Vec<&str> = count.split(' ').collect();
    let words = words.iter().enumerate().map(|(ix, word)| {
        // The total after "/" stays muted, as in the panel.
        let bold = word.starts_with(|c: char| c.is_ascii_digit()) && (ix == 0 || words[ix - 1] != "/");
        div().text_color(rgb(if bold { TEXT } else { MUTED })).child(word.to_string())
    });
    mono(11.5).flex().flex_none().gap(px(6.9)).children(words)
}

#[cfg(test)]
mod tests {
    use super::page_rows;

    #[test]
    fn pages_slice_the_list_and_pull_back_when_it_shrinks() {
        assert_eq!(page_rows(13, 0, 6), (0, 0..6));
        assert_eq!(page_rows(13, 2, 6), (2, 12..13));
        assert_eq!(page_rows(13, 5, 6), (2, 12..13));
        assert_eq!(page_rows(12, 2, 6), (1, 6..12));
        assert_eq!(page_rows(0, 3, 6), (0, 0..0));
    }
}
