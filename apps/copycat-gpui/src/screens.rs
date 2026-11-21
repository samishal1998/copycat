//! The three screens, each a thin view over daemon actions — the same actions
//! the Tauri app's History, Session and Settings screens send.

use copycat_core::{ClipId, ClipSummary, DuplicatePolicy, SessionMode, SessionState, SessionSummary};
use copycat_protocol::{Action, CheckStatus};
use gpui_kit::component::button::{Button, ButtonGroup, ButtonVariants as _};
use gpui_kit::component::input::Input;
use gpui_kit::component::progress::Progress;
use gpui_kit::component::switch::Switch;
use gpui_kit::component::tag::Tag;
use gpui_kit::component::{ActiveTheme as _, Disableable as _, Selectable as _, Sizable as _, h_flex, v_flex};
use gpui_kit::prelude::*;
use gpui_kit::*;

use crate::app::{APP_VERSION, AppView, Update, app_bundle};
use crate::bindings::help;
use crate::brand::*;
use crate::brand::px;
use crate::settings::{Appearance, CLOSE_AFTER, Nap, PAGE_SIZES, SLEEP_AFTER, Settings, UI_SCALES};

/// What a dragged session row carries: where it was and which clip it is (the
/// daemon refuses a move whose id no longer sits at `from`).
#[derive(Clone)]
pub struct DragRow {
    pub ix: usize,
    pub id: ClipId,
}

/// The picture that follows the pointer while a row is dragged.
pub struct DragGhost(pub SharedString);

impl Render for DragGhost {
    fn render(&mut self, _: &mut Window, _: &mut Context<Self>) -> impl IntoElement {
        mono(12.)
            .max_w(px(420.))
            .truncate()
            .px(px(10.))
            .py(px(6.))
            .rounded(px(8.))
            .border_1()
            .border_color(rgba(ACTIVE_LINE))
            .bg(rgb(pal().raised_2))
            .text_color(rgb(pal().text))
            .opacity(0.9)
            .child(self.0.clone())
    }
}

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
                .border_color(rgb(pal().line))
                .bg(rgb(pal().raised))
                .child(
                    h_flex()
                        .flex_1()
                        .min_w_0()
                        .gap(px(8.))
                        .child(mono(12.5).min_w_0().truncate().child(one_line(&clip.preview, 200)))
                        .when(clip.duplicate_run > 1, |this| {
                            this.child(
                                mono(12.5).flex_none().text_color(rgb(pal().muted)).child(format!("×{}", clip.duplicate_run)),
                            )
                        }),
                )
                .child(
                    h_flex()
                        .gap(px(6.))
                        .child(mono(10.5).text_color(rgb(pal().muted)).child(age(now, clip.captured_at)))
                        // Colour carries the state: JetBrains Mono has no ☆, so
                        // both glyphs fall back to the same filled star.
                        .child(icon_button(("pin", ix), if pinned { IconName::StarFill } else { IconName::Star })
                            .tooltip(if pinned { "Unpin" } else { "Pin" })
                            .text_color(if pinned { cx.theme().warning } else { cx.theme().muted_foreground })
                            .on_click(
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
                        .child(icon_button(("delete", ix), IconName::CircleX).tooltip("Delete").text_color(cx.theme().danger).on_click(cx.listener(
                            move |this, _, _, cx| {
                                this.send(Action::HistoryDelete { id }, cx, |this, _, cx| this.load_history(cx));
                            },
                        ))),
                )
        });

        let clear = |this: &mut Self, keep_pinned: bool, cx: &mut Context<Self>| {
            this.confirm_clear = None;
            this.send(Action::HistoryClear { keep_pinned }, cx, |this, _, cx| this.load_history(cx));
        };
        // Pinned items go too, so it takes a second click within 4s.
        let clear_all = cx.listener(move |this, _, _, cx| {
            if this.confirm_clear.is_some() {
                return clear(this, false, cx);
            }
            this.confirm_clear = Some(cx.spawn(async move |this, cx| {
                cx.background_executor().timer(std::time::Duration::from_secs(4)).await;
                let _ = this.update(cx, |view, cx| {
                    view.confirm_clear = None;
                    cx.notify();
                });
            }));
            cx.notify();
        });
        let all_label = if self.confirm_clear.is_some() { "Click again to clear pinned too" } else { "Clear all" };
        let searching = !self.search.read(cx).value().trim().is_empty();

        v_flex()
            .child(heading("History", "Everything you've copied. Paste one again, or pin the ones you want to keep."))
            // `.search`: a mono field, 340px at most.
            .child(
                div()
                    .max_w(px(340.))
                    .mb(px(16.))
                    .py(px(3.))
                    .rounded(px(8.))
                    .border_1()
                    .border_color(rgb(pal().line))
                    .bg(rgb(pal().surface))
                    .font_family(MONO)
                    .text_size(px(12.5))
                    .child(Input::new(&self.search).appearance(false).cleanable(true)),
            )
            .child(
                h_flex()
                    .gap(px(8.))
                    .mb(px(14.))
                    .child(
                        button("clear-unpinned", "Clear history", Kind::Ghost, true)
                            .tooltip("Keeps pinned items")
                            .on_click(cx.listener(move |this, _, _, cx| clear(this, true, cx))),
                    )
                    .child(button("clear-all", all_label, Kind::GhostDanger, true).on_click(clear_all)),
            )
            .when(self.clips.is_empty(), |this| {
                this.child(empty(if searching { "No matches." } else { "Nothing copied yet. Copy something and it shows up here." }))
            })
            .children(rows)
            .when(self.truncated, |this| this.child(empty("Showing the first matches. Type more to narrow it down.")))
    }

    pub fn session_screen(&self, cx: &Context<Self>) -> impl IntoElement {
        let session = self.session();
        let (title, subtitle) = match session {
            None if self.paused() => ("Paused".into(), "Copycat is paused — new copies aren't being saved.".into()),
            None => (
                "Session".into(),
                "Paste several things in a row without switching back and forth. Pick how:".into(),
            ),
            Some(s) => (mode_title(s.mode).to_string(), session_subtitle(s)),
        };

        // Each control says what it did; a paste says so by pasting.
        let act = |id: &'static str, label: &'static str, kind: Kind, action: Action, done: Option<&'static str>| {
            button(id, label, kind, false).on_click(cx.listener(move |this, _, _, cx| {
                this.send(action.clone(), cx, move |this, _, cx| {
                    if let Some(done) = done {
                        this.toast(done, cx)
                    }
                });
            }))
        };
        let stop = || {
            act("session-stop", "Stop", Kind::Danger, Action::SessionStop, Some("Stopped. Your copies are still in History."))
        };

        let body = match session {
            None => {
                let start = |id, name: &'static str, about: &'static str, action, done| {
                    h_flex()
                        .gap(px(12.))
                        .py(px(8.))
                        .child(
                            v_flex()
                                .flex_1()
                                .min_w_0()
                                .child(div().font_weight(FontWeight::MEDIUM).child(name))
                                .child(div().text_color(rgb(pal().muted)).child(about)),
                        )
                        .child(act(id, "Start", Kind::Plain, action, Some(done)))
                };
                section("Modes")
                    .child(start(
                        "stack-start",
                        "Stack",
                        "Paste your recent copies, newest first.",
                        Action::StackStart { duplicates: None },
                        "Stack started",
                    ))
                    .child(start(
                        "queue-capture",
                        "Queue",
                        "Collect what you copy next, then paste it in the same order.",
                        Action::QueueCapture { duplicates: None },
                        "Collecting…",
                    ))
                    .child(start(
                        "group-capture",
                        "Group",
                        "Collect what you copy next, then paste it all at once.",
                        Action::GroupCapture { delimiter: None, duplicates: None },
                        "Collecting…",
                    ))
                    .into_any_element()
            }
            Some(s) => {
                let controls = match (s.state, s.mode) {
                    (SessionState::Capturing, SessionMode::Group) => vec![
                        act("group-paste", "Paste all", Kind::Primary, Action::GroupPaste, None),
                        stop(),
                    ],
                    (SessionState::Capturing, _) => vec![
                        act("queue-seal", "Done collecting", Kind::Primary, Action::QueueSeal, Some("Ready to paste")),
                        stop(),
                    ],
                    _ => vec![
                        act("paste-mode", "Paste next", Kind::Primary, Action::PasteMode { inject: true }, None),
                        act("session-reset", "Start over", Kind::Plain, Action::SessionReset, Some("Back to the first item")),
                        stop(),
                    ],
                };
                let notes = session_notes(s);
                v_flex()
                    .gap(px(10.))
                    .child(div().child(session_status(s)))
                    .when(s.state == SessionState::Ready, |this| {
                        this.child(
                            Progress::new("session-progress").value(s.cursor as f32 * 100. / s.size.max(1) as f32),
                        )
                    })
                    .when(!notes.is_empty(), |this| this.child(help(notes)))
                    .child(h_flex().flex_wrap().gap(px(8.)).mt(px(6.)).children(controls))
                    .into_any_element()
            }
        };

        let items = session.map(|s| {
            let items = self.items_for(s.id);
            section("Items")
                .child(help("In paste order. Drag a row to reorder, or use the arrows. Pasted items stay put."))
                .when(items.is_empty(), |this| {
                    this.child(mono(12.).text_color(rgb(pal().faint)).py(px(8.)).child("Nothing collected yet — copy something."))
                })
                .child(v_flex().gap(px(5.)).children(
                    items.iter().enumerate().map(|(ix, clip)| self.session_row(ix, clip, s.cursor, cx)),
                ))
        });

        v_flex().gap(px(16.)).child(heading(title, subtitle)).child(body).children(items)
    }

    /// Move an item (by button or drop). The target is kept at or after the
    /// cursor, as `session-list.js` does; the daemon has the last word.
    fn move_item(&mut self, from: usize, id: ClipId, to: usize, cx: &mut Context<Self>) {
        let cursor = self.session().map_or(0, |s| s.cursor);
        let to = to.min(self.items.len().saturating_sub(1)).max(cursor);
        if from != to {
            self.send(Action::SessionMove { from, to, id }, cx, |this, _, cx| this.load_items(cx));
        }
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
        let mover = |id: &'static str, icon: IconName, tip: &'static str, to: usize, disabled: bool| {
            let clip_id = clip.id;
            icon_button((id, ix), icon).tooltip(tip).disabled(disabled).on_click(
                cx.listener(move |this, _, _, cx| this.move_item(ix, clip_id, to, cx)),
            )
        };
        let tag = (done || next).then(|| {
            if next { Tag::primary().outline().small().child("NEXT") } else { Tag::secondary().small().child("PASTED") }
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
            .bg(rgb(pal().raised))
            .when(done, |this| this.opacity(0.5))
            // Pasted rows stay put: they neither drag nor take a drop. A row
            // dropped on row `i` lands at `i`, so the marker sits where it will
            // end up: above `i` when it comes from below, under it when from above.
            .when(!done, |this| {
                let ghost = preview.clone();
                this.cursor_grab()
                    .on_drag(DragRow { ix, id: clip_id }, move |_, _, _, cx| {
                        let text = ghost.clone();
                        cx.new(|_| DragGhost(text))
                    })
                    .drag_over::<DragRow>(move |style, from, _, _| {
                        if from.ix == ix {
                            style
                        } else {
                            style.shadow(marker(if from.ix > ix { -2. } else { 2. }))
                        }
                    })
                    .on_drop(cx.listener(move |this, from: &DragRow, _, cx| this.move_item(from.ix, from.id, ix, cx)))
            })
                        .child(div().w(px(20.)).flex_none().text_right().text_size(px(10.5)).text_color(rgb(pal().faint)).child((ix + 1).to_string()))
            .child(div().flex_1().min_w_0().truncate().child(preview.clone()))
            .children(tag)
            .when(!done, |this| {
                this.child(
                    h_flex()
                        .gap(px(2.))
                        .child(mover("up", IconName::ArrowUp, "Move up", ix.saturating_sub(1).max(cursor), ix == cursor))
                        .child(mover("down", IconName::ArrowDown, "Move down", (ix + 1).min(last), ix == last)),
                )
            });
        if next {
            accent_edge(row.bg(rgb(pal().raised)), rgba(ACTIVE_LINE), 8.).into_any_element()
        } else {
            row.rounded(px(8.)).border_1().border_color(rgb(pal().line)).into_any_element()
        }
    }

    pub fn settings_screen(&self, cx: &Context<Self>) -> impl IntoElement {
        let settings = self.settings.read(cx).clone();
        let flag = |id: &'static str, text: &'static str, on: bool, set: fn(&mut Settings, bool)| {
            Switch::new(id).label(text).checked(on).on_click(cx.listener(move |this, on: &bool, _, cx| {
                let on = *on;
                this.settings.update(cx, |s, cx| s.change(cx, |s| set(s, on)));
            }))
        };
        let step = |id: &'static str, icon: IconName, tip: &'static str, to: usize| {
            icon_button(id, icon).tooltip(tip).on_click(cx.listener(move |this, _, _, cx| {
                this.settings.update(cx, |s, cx| s.change(cx, |s| s.page_size = to));
            }))
        };
        // One of `looks` picked.
        let look = |id: &'static str, now: Appearance, looks: &[(Appearance, &'static str)], set: fn(&mut Settings, Appearance)| {
            ButtonGroup::new(id)
                .outline()
                .small()
                // Each button its own click: the group's callback misses keyboard presses.
                .children(looks.iter().enumerate().map(|(ix, &(look, label))| {
                    Button::new((id, ix))
                        .label(label)
                        .selected(now == look)
                        .when(now == look, |b| b.primary())
                        .on_click(move |_, _, cx| Settings::get(cx).update(cx, |s, cx| s.change(cx, |s| set(s, look))))
                }))
        };
        // 90% to 150%: one picked.
        let scale = |id: &'static str, now: f32, set: fn(&mut Settings, f32)| {
            ButtonGroup::new(id).outline().small().children(UI_SCALES.iter().enumerate().map(|(ix, &(scale, label))| {
                Button::new((id, ix))
                    .label(label)
                    .selected(now == scale)
                    .when(now == scale, |b| b.primary())
                    .on_click(move |_, _, cx| Settings::get(cx).update(cx, |s, cx| s.change(cx, |s| set(s, scale))))
            }))
        };
        let sleep_after = ButtonGroup::new("sleep-after").outline().small().children(SLEEP_AFTER.iter().enumerate().map(
            |(ix, &(after, label))| {
                Button::new(("sleep-after", ix))
                    .label(label)
                    .selected(settings.sleep_after == after)
                    .when(settings.sleep_after == after, |b| b.primary())
                    .on_click(move |_, _, cx| Settings::get(cx).update(cx, |s, cx| s.change(cx, |s| s.sleep_after = after)))
            },
        ));
        let close_after = ButtonGroup::new("close-after").outline().small().children(CLOSE_AFTER.iter().enumerate().map(
            |(ix, &(after, label))| {
                Button::new(("close-after", ix))
                    .label(label)
                    .selected(settings.close_after == after)
                    .when(settings.close_after == after, |b| b.primary())
                    .on_click(move |_, _, cx| Settings::get(cx).update(cx, |s, cx| s.change(cx, |s| s.close_after = after)))
            },
        ));
        let size = settings.page_size;
        // Opposite is the floating button's; a main window already set to it keeps showing it.
        let main_looks: &[_] =
            if settings.appearance == Appearance::ReverseSystem { &Appearance::FLOATING } else { &Appearance::MAIN };
        let gap = || div().mt(px(10.));

        v_flex()
            .gap(px(16.))
            .child(heading("Settings", "How Copycat looks and behaves."))
            .when(self.permission_blocked(), |this| this.child(self.permission_callout(cx)))
            .child(
                section("Appearance")
                    .child(row("Look", look("look-app", settings.appearance, main_looks, |s, look| s.appearance = look)))
                    .child(gap().child(row("Size", scale("ui-scale", settings.ui_scale, |s, scale| s.ui_scale = scale))))
                    .child(help("Also applies to the menu bar list.")),
            )
            .child(
                section("Floating button")
                    .child(flag("floating-on", "Show the floating button", settings.floating, |s, on| s.floating = on))
                    .child(help("A small button that stays above your apps. Click it to see what's next to paste."))
                    .when(settings.floating, |this| {
                        this.child(gap().child(flag(
                            "floating-every",
                            "Show on every display",
                            settings.every_screen,
                            |s, on| s.every_screen = on,
                        )))
                        .child(gap().child(flag("floating-mascot", "Show it as a cat", settings.mascot, |s, on| {
                            s.mascot = on
                        })))
                        .child(help("The cat reacts when you copy and paste, and naps when you're away."))
                        .when(settings.mascot, |this| {
                            this.child(gap().child(row(
                                "Cat naps after",
                                h_flex().gap(px(10.)).child(sleep_after).child(
                                    button("sleep-now", "Nap now", Kind::Plain, true).on_click(|_, _, cx| cx.set_global(Nap)),
                                ),
                            )))
                        })
                        .child(gap().child(flag(
                            "floating-leader",
                            "Show shortcut keys as you type them",
                            settings.leader_bubble,
                            |s, on| s.leader_bubble = on,
                        )))
                        .child(help("A bubble beside the button shows the keys of a shortcut sequence as you press them."))
                        .child(gap().child(flag(
                            "close-away",
                            "Close the panel when I click elsewhere",
                            settings.close_on_click_away,
                            |s, on| s.close_on_click_away = on,
                        )))
                        .child(gap().child(flag(
                            "close-paste",
                            "Close the panel after pasting from it",
                            settings.close_on_paste,
                            |s, on| s.close_on_paste = on,
                        )))
                        .child(gap().child(row("Close the panel after", close_after)))
                        .child(help("Counted while the pointer is away from it."))
                        .when(settings.auto_closes(), |this| {
                            this.child(help("The pin at the top of the panel keeps it open."))
                        })
                        .child(gap().child(row(
                            "Look",
                            look("look-floating", settings.floating_appearance, &Appearance::FLOATING, |s, look| {
                                s.floating_appearance = look
                            }),
                        )))
                        .child(gap().child(row(
                            "Size",
                            scale("floating-scale", settings.floating_scale, |s, scale| s.floating_scale = scale),
                        )))
                        .child(help(
                            "Drag it anywhere. Throw it toward a corner to tuck it away; hold ⌃ Control while dragging to place it exactly.",
                        ))
                    })
            )
            .child(
                section("Menu bar").child(row(
                    "Items shown in the menu bar",
                    h_flex()
                        .gap(px(10.))
                        .child(step("mb-down", IconName::Minus, "Fewer", size.saturating_sub(1).max(*PAGE_SIZES.start())))
                        .child(mono(13.5).min_w(px(14.)).text_center().child(size.to_string()))
                        .child(step("mb-up", IconName::Plus, "More", size + 1)),
                )),
            )
            .child(self.working_section())
            .child(self.updates_section(cx))
            .child(self.advanced_section(settings.debug, cx))
    }

    /// What's working: only what isn't, unless everything is.
    fn working_section(&self) -> impl IntoElement {
        let section = section("What's working");
        let Some(doctor) = &self.doctor else {
            return section.child(empty("Checking…"));
        };
        let problems: Vec<_> = doctor.checks.iter().filter(|c| c.status != CheckStatus::Ok).collect();
        if problems.is_empty() {
            return section.child(div().text_color(rgb(pal().ok)).child("Everything is working."));
        }
        section.children(problems.into_iter().map(|c| check_row(Some(c.status), &c.name, false, c.detail.clone())))
    }

    /// Settings → Advanced: closed until asked for.
    fn advanced_section(&self, debug: bool, cx: &Context<Self>) -> impl IntoElement {
        let open = self.advanced;
        let toggle = button("advanced", "Advanced", Kind::Ghost, false)
            .icon(if open { IconName::ChevronDown } else { IconName::ChevronRight })
            .on_click(cx.listener(|this, _, _, cx| {
                this.advanced = !this.advanced;
                cx.notify();
            }));
        let path = self.config_path.clone();
        let checks = self.doctor.iter().flat_map(|doctor| {
            let platform =
                check_row(None, "platform", true, format!("{} — {}", doctor.display_server, doctor.platform_support));
            std::iter::once(platform)
                .chain(doctor.checks.iter().map(|c| check_row(Some(c.status), &c.name, true, c.detail.clone())))
        });

        v_flex().gap(px(16.)).child(div().flex().child(toggle)).when(open, |this| {
            this.child(help("For troubleshooting and power users."))
                .child(
                    section("Advanced")
                        .child(
                            Switch::new("debug-overlay").label("Show live activity panel").checked(debug).on_click(
                                cx.listener(|this, on: &bool, _, cx| {
                                    let on = *on;
                                    this.settings.update(cx, |s, cx| s.change(cx, |s| s.debug = on));
                                }),
                            ),
                        )
                        .child(help("A small panel that shows each shortcut as it fires. Handy when a shortcut doesn't seem to work."))
                        .child(
                            div().mt(px(14.)).child(row(
                                "Config file",
                                h_flex()
                                    .gap(px(8.))
                                    .child(
                                        button("cfg-open", "Open config file", Kind::Plain, true)
                                            .disabled(path.is_none())
                                            .on_click(move |_, _, cx| {
                                                if let Some(path) = &path {
                                                    open_config(path, cx)
                                                }
                                            }),
                                    )
                                    .child(
                                        button("cfg-reload", "Reload", Kind::Plain, true)
                                            .on_click(cx.listener(|this, _, _, cx| this.reload_config(cx))),
                                    ),
                            )),
                        )
                        .child(help("More options (how long history is kept, privacy) live in this text file. Edit it, then click Reload."))
                        .when(!self.installed, |this| {
                            this.child(
                                div().mt(px(14.)).child(row(
                                    "Terminal tools",
                                    button("install-copy", "Copy install command", Kind::Plain, true)
                                        .on_click(cx.listener(|this, _, _, cx| this.copy_install(cx))),
                                )),
                            )
                            .child(help("Use your clipboard history from Terminal too."))
                        })
                        .children(self.status.as_ref().map(|status| {
                            div()
                                .mt(px(14.))
                                .child(row("Background service", mono(12.5).child(status.daemon_version.clone())))
                        })),
                )
                .child(
                    section("All checks")
                        .when(self.doctor.is_none(), |this| this.child(empty("Checking…")))
                        .children(checks),
                )
        })
    }

    fn updates_section(&self, cx: &Context<Self>) -> impl IntoElement {
        let line = match &self.update {
            Update::Idle => None,
            Update::Checking => Some(("Checking…".into(), pal().muted)),
            Update::UpToDate => Some(("Up to date.".into(), pal().muted)),
            Update::Available { tag, .. } => Some((format!("{tag} is available."), pal().text)),
            Update::Installing => Some(("Installing… Copycat will restart.".into(), pal().muted)),
            Update::Failed(error) => Some((error.clone(), pal().bad)),
        };
        let busy = matches!(self.update, Update::Checking | Update::Installing);
        // Installs in place only from a macOS bundle; anywhere else, the release page.
        let get = match &self.update {
            Update::Available { tag, .. } if app_bundle().is_some() => Some(
                button("update-install", format!("Install {tag}"), Kind::Primary, true)
                    .on_click(cx.listener(|this, _, _, cx| this.install_update(cx))),
            ),
            Update::Available { tag, .. } => {
                let url = format!("https://github.com/samishal1998/copycat/releases/tag/{tag}");
                Some(button("update-page", format!("Download {tag}"), Kind::Primary, true)
                    .on_click(move |_, _, cx| cx.open_url(&url)))
            }
            _ => None,
        };

        section("Updates")
            .child(format!("Copycat {APP_VERSION}"))
            .children(line.map(|(text, color)| div().mt(px(4.)).text_color(rgb(color)).child(text)))
            .child(
                h_flex()
                    .gap(px(8.))
                    .mt(px(10.))
                    .child(
                        button("update-check", "Check for updates", Kind::Plain, true)
                            .disabled(busy)
                            .on_click(cx.listener(|this, _, _, cx| this.check_update(true, cx))),
                    )
                    .children(get),
            )
            .child(help("Copycat restarts after updating."))
    }

    /// When the event tap is refused it is almost always Accessibility not
    /// granted to Copycat itself: the app launches the daemon, so macOS
    /// attributes the daemon's keyboard access to the app. Say so, offer the
    /// exact panes, and a restart, since a refusal sticks until a new process.
    fn permission_callout(&self, cx: &Context<Self>) -> impl IntoElement {
        let restart = button("restart", "Try again", Kind::Ghost, false)
            .on_click(cx.listener(|this, _, _, cx| this.restart_daemon("Trying again…", cx)));
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
        section("Copycat needs your permission")
            .child(
                div()
                    .text_color(rgb(pal().muted))
                    .mb(px(12.))
                    .child("To catch your shortcuts and paste for you, Copycat needs Accessibility access. Turn Copycat on in the list, then click Try again."),
            )
            .child(buttons.child(restart))
            .child(div().mt(px(5.)).text_size(px(11.5)).text_color(rgb(pal().faint)).child(
                "Already on? Select Copycat, remove it with −, then add it again.",
            ))
    }
}

/// `.empty`: a quiet line where a list would be.
pub fn empty(text: &'static str) -> Div {
    div().text_color(rgb(pal().faint)).py(px(14.)).px(px(2.)).child(text)
}

/// Open a macOS Privacy & Security pane, so the user does not have to hunt for it.
#[cfg(target_os = "macos")]
fn open_pane(pane: &str) {
    let url = format!("x-apple.systempreferences:com.apple.preference.security?{pane}");
    let _ = std::process::Command::new("open").arg(url).spawn();
}

/// Open the config file in a text editor: on a stock Mac a .toml has no app
/// of its own, so `open -t` picks the default editor.
fn open_config(path: &str, cx: &App) {
    if cfg!(target_os = "macos") {
        let _ = std::process::Command::new("open").args(["-t", path]).spawn();
    } else {
        cx.open_with_system(std::path::Path::new(path));
    }
}

/// One `.check` line: the status pill, the check's name (and its id under it,
/// when `raw`), its detail.
fn check_row(status: Option<CheckStatus>, name: &str, raw: bool, detail: String) -> impl IntoElement {
    let pill = status.map(|status| {
        let (word, tag) = match status {
            CheckStatus::Ok => ("Working", Tag::success()),
            CheckStatus::Degraded => ("Limited", Tag::warning()),
            CheckStatus::Unavailable => ("Not working", Tag::danger()),
        };
        tag.outline().small().rounded_full().child(word)
    });
    h_flex()
        .items_start()
        .gap(px(14.))
        .py(px(8.))
        .border_b_1()
        .border_color(rgb(pal().line_soft))
        .text_size(px(12.5))
        .child(div().w(px(96.)).flex_none().children(pill))
        .child(
            v_flex()
                .w(px(190.))
                .flex_none()
                .child(check_label(name).to_string())
                .when(raw, |this| this.child(mono(10.5).text_color(rgb(pal().faint)).child(name.to_string()))),
        )
        .child(div().flex_1().min_w_0().text_color(rgb(pal().muted)).child(detail))
}

/// The paste chord, as the user presses it.
const PASTE_KEY: &str = if cfg!(target_os = "macos") { "⌘V" } else { "Ctrl+V" };

/// The Session screen's title for a mode: its name and how it pastes.
fn mode_title(mode: SessionMode) -> &'static str {
    match mode {
        SessionMode::Stack => "Stack — newest first",
        SessionMode::Queue => "Queue — in the order copied",
        SessionMode::Group => "Group — all at once",
    }
}

/// What the user's own paste does now.
fn session_subtitle(s: &SessionSummary) -> String {
    match (s.state, s.mode) {
        (SessionState::Capturing, SessionMode::Group) => "Copy as much as you like, then paste it all at once.".into(),
        (SessionState::Capturing, _) => format!("Copy as much as you like. Your first {PASTE_KEY} starts pasting."),
        _ => format!("Each {PASTE_KEY} pastes the next item."),
    }
}

/// Where the session stands, in a sentence.
fn session_status(s: &SessionSummary) -> String {
    match s.state {
        SessionState::Capturing => format!("Collecting — {} so far.", s.size),
        _ if s.remaining == 0 => "All pasted. Start over or stop.".into(),
        _ => format!("Pasted {} of {} — {} to go.", s.cursor, s.size, s.remaining),
    }
}

/// The session's settings worth a mention: only those off the default.
fn session_notes(s: &SessionSummary) -> String {
    let mut notes = Vec::new();
    if s.duplicate_policy == DuplicatePolicy::Preserve {
        notes.push("Repeated copies are kept.".to_string());
    }
    if let Some(delimiter) = &s.delimiter {
        notes.push(format!("Items are joined with {}.", joined_with(delimiter)));
    }
    notes.join(" ")
}

/// A group's delimiter in words.
fn joined_with(delimiter: &str) -> String {
    match delimiter {
        "\n" | "\r\n" => "a new line".into(),
        "\t" => "a tab".into(),
        " " => "a space".into(),
        d => format!("\"{d}\""),
    }
}

/// A diagnostic check's name, as a person would put it; a name this build
/// does not know shows as is.
fn check_label(name: &str) -> &str {
    match name {
        "platform" => "System",
        "clipboard" => "Clipboard",
        "clipboard-formats" => "Clipboard formats",
        "html-capture" => "Formatted text",
        "image-capture" => "Images",
        "repeat-copy-detection" => "Copying the same thing twice",
        "macos-permissions" => "macOS permissions",
        "paste-injection" => "Pasting for you",
        "paste-interception" => "⌘V during a session",
        "global-hotkeys" => "Keyboard shortcuts",
        "binding" => "A shortcut",
        "leader-sequences" => "Shortcut sequences",
        "key-storage" => "Encryption key",
        "history-store" => "Saving history",
        "data-directory" => "Data folder",
        "ipc-access-control" => "Access control",
        "pause-on-lock" => "Pause on lock screen",
        name => name,
    }
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
    use super::{age, check_label, joined_with, one_line, session_notes, session_status, session_subtitle};
    use copycat_core::{DuplicatePolicy, SessionMode, SessionState, SessionSummary};

    #[test]
    fn previews_flatten_and_cut_on_char_boundaries() {
        assert_eq!(one_line("a\n  b\tc", 10), "a b c");
        assert_eq!(one_line("héllo wörld", 5), "héllo…");
        assert_eq!(one_line("  ", 5), "(empty)");
    }

    #[test]
    fn the_session_reads_as_sentences() {
        let s = |state, size, cursor| SessionSummary {
            id: 1,
            mode: SessionMode::Queue,
            state,
            duplicate_policy: DuplicatePolicy::Collapse,
            size,
            cursor,
            remaining: size - cursor,
            next: None,
            delimiter: None,
            created_at: 0,
        };
        assert_eq!(session_status(&s(SessionState::Capturing, 3, 0)), "Collecting — 3 so far.");
        assert_eq!(session_status(&s(SessionState::Ready, 10, 1)), "Pasted 1 of 10 — 9 to go.");
        assert_eq!(session_status(&s(SessionState::Ready, 2, 2)), "All pasted. Start over or stop.");
        assert!(session_subtitle(&s(SessionState::Capturing, 3, 0)).ends_with("starts pasting."));
        assert_eq!(session_notes(&s(SessionState::Ready, 2, 0)), "");
        let group = SessionSummary {
            mode: SessionMode::Group,
            duplicate_policy: DuplicatePolicy::Preserve,
            delimiter: Some("\n".into()),
            ..s(SessionState::Capturing, 2, 0)
        };
        assert_eq!(session_subtitle(&group), "Copy as much as you like, then paste it all at once.");
        assert_eq!(session_notes(&group), "Repeated copies are kept. Items are joined with a new line.");
        assert_eq!(joined_with("\t"), "a tab");
        assert_eq!(joined_with(", "), "\", \"");
        assert_eq!(check_label("paste-injection"), "Pasting for you");
        assert_eq!(check_label("something-new"), "something-new");
    }

    #[test]
    fn ages_use_the_largest_whole_unit() {
        assert_eq!(age(59_000, 0), "59s");
        assert_eq!(age(61_000, 0), "1m");
        assert_eq!(age(7_200_000, 0), "2h");
        assert_eq!(age(0, 5_000), "0s");
    }
}
