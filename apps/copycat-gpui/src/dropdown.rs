//! The tray dropdown: the Tauri menu-bar panel's content (mode box, a paged
//! slice of history, the modes to start or stop) over the same shared
//! `Daemon` the main window watches. It is a new view each time it opens and
//! is dropped when it closes.

use std::ops::Range;
use std::time::Duration;

use copycat_core::{ClipSummary, SessionMode, SessionState};
use copycat_protocol::{Action, ResultBody};
use gpui_kit::component::scroll::ScrollableElement as _;
use gpui_kit::component::kbd::Kbd;
use gpui_kit::component::tag::Tag;
use gpui_kit::component::{ActiveTheme as _, Disableable as _, Icon, Sizable as _, h_flex, v_flex};
use gpui_kit::prelude::*;
use gpui_kit::*;

use crate::app::{Daemon, history_mark, injects_paste, request, status_words};
use crate::brand::*;
use crate::brand::px;
use crate::floating::starts;
use crate::settings::Settings;
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
            // The page size is set from the main window's Settings.
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

    /// The mode name and its count in plain words (`status_words`), and the
    /// next clip's preview while a session is traversing.
    fn mode(&self, cx: &App) -> (String, String, Option<String>) {
        let daemon = self.daemon.read(cx);
        let core = daemon.status.as_ref().map(|s| &s.core);
        let (name, count) = status_words(daemon.connected, core);
        let next = core
            .and_then(|core| core.session.as_ref())
            .filter(|s| s.state != SessionState::Capturing)
            .and_then(|s| s.next)
            // Not on this page: no preview, and no raw id either.
            .and_then(|id| self.clips.iter().find(|clip| clip.id == id))
            .map(|clip| one_line(&clip.preview, 60));
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
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        themed(false, window, cx);
        let daemon = self.daemon.read(cx);
        let (connected, paused) =
            (daemon.connected, daemon.status.as_ref().is_some_and(|s| s.core.paused));
        let session = daemon.status.as_ref().and_then(|s| s.core.session.as_ref());
        let in_session = session.is_some();
        // While collecting: what ⌘V will do with it.
        let hint = session.filter(|s| s.state == SessionState::Capturing).and_then(|s| match s.mode {
            SessionMode::Queue => Some("Keep copying. Press ⌘V to paste them in order."),
            SessionMode::Group => Some("Keep copying. Press ⌘V to paste them all at once."),
            SessionMode::Stack => None,
        });
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
            .border_color(cx.theme().sidebar_border)
            .child(mark(20.))
            .child(mono(14.).font_weight(FontWeight::BOLD).child("copycat"))
            .child(dot(connected, true))
            .child(div().flex_1())
            .child(icon_button("open-main", IconName::ExternalLink).tooltip("Open Copycat").on_click(cx.listener(
                |_, _, window, cx| {
                    tray::close(window, false, cx);
                    tray::show_main(cx);
                },
            )));

        // `.mode`: washed orange while a session runs, plain otherwise. Not
        // when idle: it would only repeat the first row.
        let mode = v_flex()
            .px(px(14.))
            .pt(px(13.))
            .pb(px(14.))
            .border_b_1()
            .border_color(cx.theme().sidebar_border)
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
                            .text_color(rgb(if in_session { ACTIVE } else { pal().muted }))
                            .child(spaced(&name, 2.)),
                    )
                    .child(div().flex_1())
                    .child(if in_session {
                        count_line(&count)
                    } else {
                        mono(11.5).min_w_0().truncate().text_color(rgb(pal().muted)).child(count)
                    }),
            )
            .when_some(next, |this, next| {
                this.child(div().mt(px(12.)).child(accent_edge(
                    h_flex()
                        .gap(px(10.))
                        .px(px(11.))
                        .py(px(10.))
                        .bg(rgb(pal().raised_2))
                        .child(mono(12.).flex_1().min_w_0().truncate().child(next))
                        .child(Tag::primary().outline().small().child("NEXT"))
                        .child(kbd("cmd-v")),
                    rgba(ACTIVE_LINE),
                    8.,
                )))
            })
            .when_some(hint, |this, hint| this.child(mono(11.5).mt(px(8.)).text_color(rgb(pal().muted)).child(hint)));

        // `.list-head`: the heading. Rows per page are set in Settings.
        let list_head = h_flex()
            .px(px(14.))
            .pt(px(12.))
            .pb(px(4.))
            .child(div().font_family(DISPLAY).font_weight(FontWeight::BOLD).text_size(px(13.)).child("History"));

        let start = rows.start;
        let list = v_flex()
            .flex_1()
            .min_h_0()
            .px(px(8.))
            .pt(px(4.))
            .pb(px(6.))
            .overflow_y_scrollbar()
            .when(total == 0, |this| {
                this.child(div().py(px(16.)).px(px(8.)).text_center().text_size(px(12.)).text_color(rgb(pal().faint)).child("Nothing copied yet."))
            })
            .children(self.clips[rows.clone()].iter().enumerate().map(|(i, clip)| {
                let id = clip.id;
                h_flex()
                    .id(("row", i))
                    .flex_none()
                    .gap(px(10.))
                    .p(px(8.))
                    .rounded(px(8.))
                    .cursor_pointer()
                    .hover(|style| style.bg(rgb(pal().raised)))
                    // The panel closes with the paste, so there is nothing to say after.
                    .on_click(cx.listener(move |this, _, window, cx| {
                        this.send(Action::PasteId { id }, Some(window), cx, |_, _, _| {});
                    }))
                    .child(mono(10.5).w(px(20.)).flex_none().text_center().text_color(rgb(pal().faint)).child((start + i + 1).to_string()))
                    .child(mono(12.5).flex_1().min_w_0().truncate().child(one_line(&clip.preview, 80)))
                    .child(
                        h_flex()
                            .flex_none()
                            .gap(px(8.))
                            .when(clip.duplicate_run > 1, |this| {
                                this.child(div().text_size(px(10.)).text_color(rgb(pal().faint)).child(format!("×{}", clip.duplicate_run)))
                            })
                            .child(mono(10.5).text_color(rgb(pal().faint)).child(age(now, clip.captured_at)))
                            .when(clip.pinned, |this| this.child(Icon::new(IconName::Star).xsmall().text_color(rgb(pal().warn)))),
                    )
            }));

        // `.pager`: ◀ the range ▶.
        let pager = h_flex()
            .justify_center()
            .gap(px(12.))
            .px(px(12.))
            .py(px(6.))
            .border_t_1()
            .border_color(cx.theme().sidebar_border)
            .child(
                icon_button("prev", IconName::ChevronLeft).disabled(page == 0).on_click(cx.listener(|this, _, _, cx| {
                    this.page = this.page.saturating_sub(1);
                    cx.notify();
                })),
            )
            .child(mono(10.5).min_w(px(112.)).text_center().text_color(rgb(pal().muted)).child(pager))
            .child(icon_button("next", IconName::ChevronRight).disabled(rows.end >= total).on_click(cx.listener(
                |this, _, _, cx| {
                    this.page += 1;
                    cx.notify();
                },
            )));

        // `.foot`: the modes to start, or Stop in one; pause on the right. Each
        // refreshes the list, as in the Tauri panel.
        let go = |id: &'static str, label: &'static str, kind: Kind, action: Action| {
            button(id, label, kind, true).on_click(cx.listener(move |this, _, _, cx| {
                this.send(action.clone(), None, cx, |this, _, cx| this.load_history(cx));
            }))
        };
        let footer = h_flex()
            .gap(px(6.))
            .px(px(12.))
            .py(px(9.))
            .border_t_1()
            .border_color(cx.theme().sidebar_border)
            .bg(cx.theme().group_box)
            // Offline, neither: the mode box says NOT RUNNING.
            .map(|this| match (connected, in_session) {
                (false, _) => this,
                (_, true) => this.child(go("stop", "Stop", Kind::Danger, Action::SessionStop)),
                _ => this.children(starts().into_iter().map(|(id, label, tip, action)| go(id, label, Kind::Ghost, action).tooltip(tip))),
            })
            .child(div().flex_1())
            .child(
                ghost("pause", if paused { "Resume saving" } else { "Pause saving" })
                    .small()
                    .when(!paused, |this| this.tooltip("Stop saving new copies to History until you resume."))
                    .on_click(cx.listener(move |this, _, _, cx| {
                        let action = if paused { Action::HistoryResume } else { Action::HistoryPause };
                        this.send(action, None, cx, |_, _, _| {});
                    })),
            );

        v_flex()
            .relative()
            .size_full()
            .bg(rgb(pal().surface))
            .text_color(rgb(pal().text))
            .font_family(UI)
            .text_size(px(13.))
            .line_height(relative(1.5))
            .child(header)
            .when(in_session || paused || !connected, |this| this.child(mode))
            .child(list_head)
            .child(list)
            .child(pager)
            .child(footer)
            // `.toast`: over the list, just above the footer.
            .when_some(self.toast.clone(), |this, toast| {
                this.child(div().absolute().left(px(16.)).right(px(16.)).bottom(px(58.)).child(accent_edge(
                    mono(11.5).px(px(11.)).py(px(8.)).bg(rgb(pal().raised_2)).truncate().child(toast),
                    rgb(pal().line),
                    8.,
                )))
            })
    }
}

/// A key cap for `keys` ("cmd-v").
fn kbd(keys: &str) -> Kbd {
    Kbd::new(Keystroke::parse(keys).expect("static keystroke"))
}

/// A session's count line with the numbers the Tauri panel sets in `<b>` in
/// full white: "3 left", "2 copied". Word by word, a
/// mono space apart, since a GPUI text run takes one colour.
fn count_line(count: &str) -> Div {
    let words = count.split(' ').map(|word| {
        let bold = word.starts_with(|c: char| c.is_ascii_digit());
        div().text_color(rgb(if bold { pal().text } else { pal().muted })).child(word.to_string())
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
