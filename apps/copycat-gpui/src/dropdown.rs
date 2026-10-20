//! The tray dropdown: the Tauri menu-bar panel's content (mode box, a paged
//! slice of history, the capture shortcuts) over the same shared `Daemon` the
//! main window watches. It is a new view each time it opens and is dropped
//! when it closes.

use std::ops::Range;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::Duration;

use copycat_core::{ClipSummary, SessionState};
use copycat_protocol::{Action, ResultBody};
use gpui_kit::component::button::{Button, ButtonVariants as _};
use gpui_kit::component::scroll::ScrollableElement as _;
use gpui_kit::component::{
    ActiveTheme as _, Disableable as _, IconName, Sizable as _, h_flex, v_flex,
};
use gpui_kit::prelude::*;
use gpui_kit::*;

use crate::app::{Daemon, history_mark, injects_paste, request};
use crate::screens::{age, now_ms, one_line};
use crate::tray;

/// How deep the dropdown reaches into history.
const MAX_FETCH: usize = 60;
const PAGE_SIZES: std::ops::RangeInclusive<usize> = 3..=24;
/// Rows per page. Outlives each dropdown, so the size the user picked sticks
/// for the session (the Tauri app keeps it in localStorage).
static PAGE_SIZE: AtomicUsize = AtomicUsize::new(6);

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
        PAGE_SIZE.store(size.clamp(*PAGE_SIZES.start(), *PAGE_SIZES.end()), Ordering::Relaxed);
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
        let (name, count, next) = self.mode(cx);
        let size = PAGE_SIZE.load(Ordering::Relaxed);
        let (page, rows) = page_rows(self.clips.len(), self.page, size);
        self.page = page;
        let total = self.clips.len();
        let pager = if total == 0 {
            "empty".to_string()
        } else {
            format!("{}–{} of {}", rows.start + 1, rows.end, total)
        };

        let theme = cx.theme();
        let (muted, hover, border) = (theme.muted_foreground, theme.list_hover, theme.border);
        let now = now_ms();

        let header = h_flex()
            .px_3()
            .py_2()
            .gap_2()
            .border_b_1()
            .border_color(border)
            .child(div().font_weight(FontWeight::SEMIBOLD).child("copycat"))
            .child(
                div()
                    .size(px(8.))
                    .rounded_full()
                    .bg(if connected { theme.success } else { theme.danger }),
            )
            .child(div().flex_1())
            .child(
                Button::new("open-main")
                    .ghost()
                    .xsmall()
                    .icon(IconName::Maximize)
                    .tooltip("Open the full window")
                    .on_click(cx.listener(|_, _, window, cx| {
                        tray::close(window, false, cx);
                        tray::show_main(cx);
                    })),
            );

        let mode = v_flex()
            .mx_3()
            .mt_3()
            .p_3()
            .gap_1()
            .rounded_lg()
            .border_1()
            .border_color(border)
            .bg(theme.muted)
            .child(
                h_flex()
                    .gap_2()
                    .child(div().font_weight(FontWeight::SEMIBOLD).child(name))
                    .child(div().min_w_0().truncate().text_sm().text_color(muted).child(count)),
            )
            .when_some(next, |this, next| {
                this.child(
                    h_flex()
                        .gap_2()
                        .text_sm()
                        .child(div().flex_1().min_w_0().truncate().child(next))
                        .child(div().text_xs().text_color(muted).child("next ⌘V")),
                )
            });

        let list_head = h_flex()
            .px_3()
            .pt_3()
            .pb_1()
            .gap_1()
            .child(div().flex_1().text_xs().text_color(muted).child("History"))
            .child(
                Button::new("size-down")
                    .ghost()
                    .xsmall()
                    .icon(IconName::Minus)
                    .tooltip("Fewer per page")
                    .disabled(size <= *PAGE_SIZES.start())
                    .on_click(cx.listener(move |this, _, _, cx| this.set_page_size(size - 1, cx))),
            )
            .child(div().w(px(20.)).text_center().text_sm().child(size.to_string()))
            .child(
                Button::new("size-up")
                    .ghost()
                    .xsmall()
                    .icon(IconName::Plus)
                    .tooltip("More per page")
                    .disabled(size >= *PAGE_SIZES.end())
                    .on_click(cx.listener(move |this, _, _, cx| this.set_page_size(size + 1, cx))),
            );

        let start = rows.start;
        let list = v_flex()
            .flex_1()
            .min_h_0()
            .px_2()
            .overflow_y_scrollbar()
            .when(total == 0, |this| {
                this.child(div().p_3().text_sm().text_color(muted).child("Nothing copied yet."))
            })
            .children(self.clips[rows.clone()].iter().enumerate().map(|(i, clip)| {
                let id = clip.id;
                let pasted = format!("pasted {}", one_line(&clip.preview, 32));
                h_flex()
                    .id(("row", i))
                    .w_full()
                    .gap_2()
                    .px_2()
                    .py_1()
                    .rounded_md()
                    .cursor_pointer()
                    .hover(move |style| style.bg(hover))
                    .on_click(cx.listener(move |this, _, window, cx| {
                        let pasted = pasted.clone();
                        this.send(Action::PasteId { id }, Some(window), cx, move |this, _, cx| {
                            this.toast(pasted, cx)
                        });
                    }))
                    .child(div().w(px(20.)).text_xs().text_color(muted).child((start + i + 1).to_string()))
                    .child(div().flex_1().min_w_0().truncate().text_sm().child(one_line(&clip.preview, 80)))
                    .when(clip.duplicate_run > 1, |this| {
                        this.child(div().text_xs().text_color(muted).child(format!("×{}", clip.duplicate_run)))
                    })
                    .child(div().text_xs().text_color(muted).child(age(now, clip.captured_at)))
                    .when(clip.pinned, |this| this.child(div().text_xs().child("★")))
            }));

        let pager = h_flex()
            .justify_center()
            .gap_2()
            .py_1()
            .child(
                Button::new("prev")
                    .ghost()
                    .xsmall()
                    .icon(IconName::ChevronLeft)
                    .tooltip("Newer")
                    .disabled(page == 0)
                    .on_click(cx.listener(|this, _, _, cx| {
                        this.page = this.page.saturating_sub(1);
                        cx.notify();
                    })),
            )
            .child(div().text_xs().text_color(muted).child(pager))
            .child(
                Button::new("next")
                    .ghost()
                    .xsmall()
                    .icon(IconName::ChevronRight)
                    .tooltip("Older")
                    .disabled(rows.end >= total)
                    .on_click(cx.listener(|this, _, _, cx| {
                        this.page += 1;
                        cx.notify();
                    })),
            );

        // The capture shortcuts; each refreshes the list, as in the Tauri panel.
        let shortcuts: [(&'static str, &'static str, Action); 3] = [
            ("stack", "stack", Action::StackStart { duplicates: None }),
            ("queue", "queue", Action::QueueCapture { duplicates: None }),
            ("group", "group", Action::GroupCapture { delimiter: None, duplicates: None }),
        ];
        let footer = h_flex()
            .px_3()
            .py_2()
            .gap_1()
            .border_t_1()
            .border_color(border)
            .children(shortcuts.into_iter().map(|(id, label, action)| {
                Button::new(id).small().label(label).on_click(cx.listener(move |this, _, _, cx| {
                    this.send(action.clone(), None, cx, |this, _, cx| this.load_history(cx));
                }))
            }))
            .child(div().flex_1())
            .child(
                Button::new("pause")
                    .small()
                    .icon(if paused { IconName::Play } else { IconName::Pause })
                    .label(if paused { "resume" } else { "pause" })
                    .on_click(cx.listener(move |this, _, _, cx| {
                        let action = if paused { Action::HistoryResume } else { Action::HistoryPause };
                        this.send(action, None, cx, |_, _, _| {});
                    })),
            );

        v_flex()
            .size_full()
            .bg(theme.background)
            .text_color(theme.foreground)
            .child(header)
            .child(mode)
            .child(list_head)
            .child(list)
            .child(pager)
            .child(footer)
            .when_some(self.toast.clone(), |this, toast| {
                this.child(
                    div()
                        .px_3()
                        .py_1()
                        .border_t_1()
                        .border_color(border)
                        .text_xs()
                        .truncate()
                        .child(toast),
                )
            })
    }
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
