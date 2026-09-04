//! Rendering.
//!
//! The TUI is an operational console, not a history picker (§12): the same
//! screen that lists clips also says which session is live, which bindings the
//! platform refused, and whether the keyring is doing anything. Those are the
//! questions a clipboard daemon actually generates.

use copycat_core::SessionState;
use ratatui::Frame;
use ratatui::layout::{Alignment, Constraint, Layout, Rect};
use ratatui::style::Style;
use ratatui::text::{Line, Span};
use ratatui::widgets::{
    Block, BorderType, Borders, Clear, List, ListItem, ListState, Paragraph, Wrap,
};

use crate::app::{App, BindingDraft, BindingTarget, DraftField, InputMode, Tab};
use crate::theme;

pub fn draw(frame: &mut Frame, app: &mut App) {
    let [header, body, footer] =
        Layout::vertical([Constraint::Length(1), Constraint::Min(1), Constraint::Length(1)])
            .areas(frame.area());

    draw_header(frame, header, app);
    match app.tab {
        Tab::History => draw_history(frame, body, app),
        Tab::Session => draw_session(frame, body, app),
        Tab::Bindings => draw_bindings(frame, body, app),
        Tab::Diagnostics => draw_diagnostics(frame, body, app),
    }
    draw_footer(frame, footer, app);

    if let Some(draft) = &app.draft {
        draw_binding_form(frame, frame.area(), draft);
    } else if app.show_help {
        draw_help(frame, frame.area());
    }
}

fn draw_header(frame: &mut Frame, area: Rect, app: &App) {
    let mut spans = vec![Span::styled("copycat ", theme::title())];
    for (index, tab) in Tab::ALL.iter().enumerate() {
        let style = if *tab == app.tab { theme::tab_active() } else { theme::tab_inactive() };
        spans.push(Span::styled(format!(" {}·{} ", index + 1, tab.title()), style));
    }

    // Paused capture is the one piece of global state that silently changes
    // what everything else means, so it belongs in the header.
    if app.status.as_ref().is_some_and(|s| s.core.paused) {
        spans.push(Span::styled("  PAUSED", theme::notice().bold()));
    }
    frame.render_widget(Paragraph::new(Line::from(spans)), area);
}

fn draw_footer(frame: &mut Frame, area: Rect, app: &App) {
    // The mode lives bottom-left, where tmux and vim put it, because it is the
    // thing that changes what every other key means.
    let mut spans = vec![
        Span::styled(format!(" {} ", app.mode_label()), theme::selected()),
        Span::raw(" "),
    ];

    // Keys typed that have not resolved yet, shown the way vim shows a partial
    // command — otherwise a half-finished sequence looks like a dropped
    // keystroke.
    if !app.pending.is_empty() {
        spans.push(Span::styled(format!("{} ", app.pending), theme::notice().bold()));
    }

    match &app.message {
        Some(message) if message.is_error => spans.push(Span::styled(
            message.text.clone(),
            Style::default().fg(theme::ACCENT).bold(),
        )),
        Some(message) => spans.push(Span::styled(message.text.clone(), theme::notice())),
        None if app.input_mode == InputMode::Testing => spans.extend(probe_spans(app)),
        None => spans.push(Span::styled(
            match (app.tab, app.input_mode) {
                (_, InputMode::Editing) => "tab field · space toggles kind · enter save · esc cancel",
                (_, InputMode::Search) => "enter accept · esc cancel",
                (Tab::History, _) => "enter paste · dd delete · p pin · / search · a raw · ? help",
                (Tab::Session, _) => "s stack · c queue · S seal · g group · G paste · x stop · ? help",
                (Tab::Bindings, _) => "a add · e edit · dd delete · t test · r reload · ? help",
                (Tab::Diagnostics, _) => "r refresh · ? help",
            },
            theme::label(),
        )),
    }

    frame.render_widget(Paragraph::new(Line::from(spans)), area);
}

/// What the last tested keystroke resolved to.
fn probe_spans(app: &App) -> Vec<Span<'static>> {
    let Some(probe) = &app.probe else {
        return vec![Span::styled(
            "press a chord to see which binding it hits · esc to stop",
            theme::label(),
        )];
    };

    let mut spans = vec![Span::styled(format!("{} ", probe.chord), theme::body().bold())];
    match (probe.matched, probe.armed) {
        (Some(_), true) => spans.push(Span::styled(
            format!("→ leader armed, press the sequence key ({} bound)", leader_sequences(app)),
            theme::notice(),
        )),
        (Some(index), false) => {
            let row = &app.binding_rows[index];
            spans.push(Span::styled(format!("→ {}", row.action), theme::title()));
            // A binding can match and still never fire; saying only "matched"
            // would be the more comfortable half of the truth.
            if let Some(reason) = &row.inactive {
                spans.push(Span::styled(format!("  (not active: {reason})"), theme::notice()));
            }
        }
        (None, _) => spans.push(Span::styled(
            "→ no binding — a terminal intercepts many chords, so this may not reach us",
            theme::label(),
        )),
    }
    spans
}

fn leader_sequences(app: &App) -> usize {
    app.binding_rows
        .iter()
        .filter(|row| row.target == BindingTarget::Binding(copycat_protocol::BindingKind::Leader))
        .count()
}

fn panel(title: &str) -> Block<'_> {
    Block::default()
        .borders(Borders::ALL)
        .border_type(BorderType::Rounded)
        .border_style(theme::label())
        .title(Span::styled(format!(" {title} "), theme::title()))
}

// --------------------------------------------------------------------- history

fn draw_history(frame: &mut Frame, area: Rect, app: &mut App) {
    let searching = app.input_mode == InputMode::Search || !app.search.is_empty();
    let [search_area, list_area] = Layout::vertical([
        Constraint::Length(if searching { 1 } else { 0 }),
        Constraint::Min(1),
    ])
    .areas(area);

    if searching {
        let cursor = if app.input_mode == InputMode::Search { "_" } else { "" };
        frame.render_widget(
            Paragraph::new(Line::from(vec![
                Span::styled(" search ", theme::label()),
                Span::styled(format!("{}{cursor}", app.search), theme::body().bold()),
            ])),
            search_area,
        );
    }

    let title = if app.raw { "History — raw log" } else { "History — collapsed" };

    if app.clips.is_empty() {
        let message = if app.search.is_empty() {
            "Nothing recorded yet.\n\nCopy something and it will appear here."
        } else {
            "No clips match that search."
        };
        frame.render_widget(
            Paragraph::new(message)
                .style(theme::label())
                .alignment(Alignment::Center)
                .wrap(Wrap { trim: true })
                .block(panel(title)),
            list_area,
        );
        return;
    }

    let items: Vec<ListItem> = app
        .clips
        .iter()
        .map(|clip| {
            let mut spans = vec![
                Span::styled(format!("{:>5} ", format!("#{}", clip.id)), theme::label()),
                Span::styled(clip.preview.clone(), theme::body()),
            ];
            if clip.duplicate_run > 1 {
                spans.push(Span::styled(format!("  x{}", clip.duplicate_run), theme::label()));
            }
            if clip.pinned {
                spans.push(Span::styled("  pinned", theme::notice()));
            }
            ListItem::new(Line::from(spans))
        })
        .collect();

    let mut state = ListState::default().with_selected(Some(app.selected));
    frame.render_stateful_widget(
        List::new(items).block(panel(title)).highlight_style(theme::selected()),
        list_area,
        &mut state,
    );
}

// --------------------------------------------------------------------- session

fn draw_session(frame: &mut Frame, area: Rect, app: &App) {
    let mut lines = Vec::new();

    match app.status.as_ref().and_then(|s| s.core.session.as_ref()) {
        Some(session) => {
            lines.push(field("mode", session.mode.as_str()));
            lines.push(field(
                "state",
                match session.state {
                    SessionState::Capturing => "capturing — copies are being collected",
                    SessionState::Ready => "ready — traversable",
                },
            ));
            lines.push(field("duplicates", &format!("{:?}", session.duplicate_policy).to_lowercase()));
            lines.push(field("size", &session.size.to_string()));
            lines.push(field("cursor", &format!("{} of {}", session.cursor, session.size)));
            lines.push(field("remaining", &session.remaining.to_string()));
            lines.push(field(
                "next",
                &match session.next {
                    Some(id) => {
                        let preview = app
                            .clips
                            .iter()
                            .find(|c| c.id == id)
                            .map(|c| c.preview.clone())
                            .unwrap_or_else(|| "(not in the loaded view)".into());
                        format!("#{id}  {preview}")
                    }
                    None => "nothing — the session is exhausted".to_string(),
                },
            ));
            if let Some(delimiter) = &session.delimiter {
                lines.push(field("delimiter", &format!("{delimiter:?}")));
            }
        }
        None => {
            lines.push(Line::from(Span::styled("No active session.", theme::label())));
            lines.push(Line::from(""));
            lines.push(Line::from(Span::styled(
                "The clipboard behaves normally: the last copy is what pastes.",
                theme::label(),
            )));
            lines.push(Line::from(""));
            lines.push(Line::from(vec![
                Span::styled("s", theme::title()),
                Span::styled("  stack — traverse history newest first", theme::body()),
            ]));
            lines.push(Line::from(vec![
                Span::styled("c", theme::title()),
                Span::styled("  queue capture — collect copies, then seal", theme::body()),
            ]));
            lines.push(Line::from(vec![
                Span::styled("g", theme::title()),
                Span::styled("  group capture — collect copies, paste as one", theme::body()),
            ]));
        }
    }

    frame.render_widget(Paragraph::new(lines).block(panel("Session")), area);
}

/// Distinguish "nothing is there" from "we could not look".
fn describe_value(value: Option<&str>) -> &str {
    match value {
        None => "(unreadable)",
        Some("") => "(empty)",
        Some(text) => text,
    }
}

fn field<'a>(label: &'a str, value: &str) -> Line<'a> {
    Line::from(vec![
        Span::styled(format!("{label:<12}"), theme::label()),
        Span::styled(value.to_string(), theme::body()),
    ])
}

// -------------------------------------------------------------------- bindings

fn draw_bindings(frame: &mut Frame, area: Rect, app: &mut App) {
    if app.binding_rows.is_empty() {
        frame.render_widget(
            Paragraph::new("Waiting for the daemon…")
                .style(theme::label())
                .alignment(Alignment::Center)
                .block(panel("Bindings")),
            area,
        );
        return;
    }

    let items: Vec<ListItem> = app
        .binding_rows
        .iter()
        .enumerate()
        .map(|(index, row)| {
            let leader = row.target == BindingTarget::Leader;
            let hit = app.probe.as_ref().is_some_and(|p| p.matched == Some(index));
            let mut spans = vec![
                Span::styled(
                    format!("{:<7}", row.target.label()),
                    // The leader is what every sequence below it hangs off, so
                    // it does not read as just another row.
                    if leader { theme::title() } else { theme::label() },
                ),
                Span::styled(
                    format!("{:<18}", if row.trigger.is_empty() { "—" } else { &row.trigger }),
                    theme::body().bold(),
                ),
                Span::styled(format!("{:<20}", row.action), theme::body()),
            ];
            match &row.args {
                serde_json::Value::Null => {}
                args if args.as_object().is_some_and(|o| o.is_empty()) => {}
                args => spans.push(Span::styled(args.to_string(), theme::label())),
            }
            // A binding that will never fire has to look different from one
            // that will; that is the whole point of showing them together.
            if let Some(reason) = &row.inactive {
                spans.push(Span::styled(format!("  ✗ {reason}"), theme::notice()));
            }
            if hit {
                spans.push(Span::styled("  ◀ hit", theme::title()));
            }
            ListItem::new(Line::from(spans))
        })
        .collect();

    let mut state = ListState::default().with_selected(Some(app.binding_selected));
    frame.render_stateful_widget(
        List::new(items).block(panel("Bindings")).highlight_style(theme::selected()),
        area,
        &mut state,
    );
}

/// The add/edit form.
fn draw_binding_form(frame: &mut Frame, area: Rect, draft: &BindingDraft) {
    let width = area.width.min(70);
    let height = area.height.min(12);
    let popup = Rect {
        x: area.x + (area.width.saturating_sub(width)) / 2,
        y: area.y + (area.height.saturating_sub(height)) / 2,
        width,
        height,
    };

    let mut lines = Vec::new();
    for &field in draft.fields() {
        let focused = field == draft.field;
        let value = draft.value(field);
        let shown = match (field, value.is_empty()) {
            (DraftField::Args, true) => "(none)".to_string(),
            (_, true) => String::new(),
            _ => value,
        };
        lines.push(Line::from(vec![
            Span::styled(
                format!(" {} {:<8} ", if focused { "›" } else { " " }, field.label()),
                if focused { theme::title() } else { theme::label() },
            ),
            Span::styled(shown, if focused { theme::body().bold() } else { theme::body() }),
            // A visible caret is the only cue that typing goes here.
            Span::styled(
                if focused && !matches!(field, DraftField::Kind | DraftField::Enabled) { "_" } else { "" },
                theme::title(),
            ),
        ]));
    }

    lines.push(Line::from(""));
    match (&draft.error, draft.target) {
        (Some(error), _) => lines.push(Line::from(Span::styled(
            format!(" {error}"),
            Style::default().fg(theme::ACCENT).bold(),
        ))),
        (None, BindingTarget::Leader) => lines.push(Line::from(Span::styled(
            " a chord like ctrl+alt+space — space toggles enabled",
            theme::label(),
        ))),
        (None, _) => lines.push(Line::from(Span::styled(
            " args is JSON, e.g. {\"duplicates\":\"preserve\"}",
            theme::label(),
        ))),
    }

    let title = match (draft.target, draft.replacing.is_some()) {
        (BindingTarget::Leader, _) => "Leader",
        (_, true) => "Edit binding",
        (_, false) => "New binding",
    };
    frame.render_widget(Clear, popup);
    frame.render_widget(Paragraph::new(lines).block(panel(title)), popup);
}

// ----------------------------------------------------------------- diagnostics

fn draw_diagnostics(frame: &mut Frame, area: Rect, app: &App) {
    let mut lines = Vec::new();

    if let Some(status) = &app.status {
        lines.push(field("daemon", &format!("{} (protocol {})", status.daemon_version, status.protocol_version)));
        lines.push(field("backend", &status.clipboard_backend));
        lines.push(field("polling", &format!("every {} ms", status.watch_interval_ms)));
        lines.push(field("persistence", &status.persistence));
        lines.push(field("key storage", &status.key_storage));
        lines.push(field("socket", &status.socket_path));
        lines.push(Line::from(""));
        // Both, always: after a paste they differ on purpose (R15), and showing
        // only one would make that look like a bug.
        lines.push(field("offset 0", describe_value(status.core.latest.as_ref().map(|c| c.preview.as_str()))));
        lines.push(field("clipboard", describe_value(status.os_clipboard.as_deref())));
        lines.push(Line::from(""));
    }

    if let Some(doctor) = &app.doctor {
        lines.push(field("platform", &format!("{} — {}", doctor.display_server, doctor.platform_support)));
        lines.push(Line::from(""));
        for check in &doctor.checks {
            let style = match check.status {
                copycat_protocol::CheckStatus::Ok => theme::label(),
                copycat_protocol::CheckStatus::Degraded => theme::notice(),
                copycat_protocol::CheckStatus::Unavailable => Style::default().fg(theme::ACCENT),
            };
            lines.push(Line::from(vec![
                Span::styled(format!("{:<12}", check.status.glyph()), style),
                Span::styled(format!("{:<22}", check.name), theme::body()),
                Span::styled(check.detail.clone(), theme::label()),
            ]));
        }
    } else {
        lines.push(Line::from(Span::styled("Waiting for diagnostics…", theme::label())));
    }

    frame.render_widget(
        Paragraph::new(lines).wrap(Wrap { trim: false }).block(panel("Diagnostics")),
        area,
    );
}

// ------------------------------------------------------------------------ help

fn draw_help(frame: &mut Frame, area: Rect) {
    let width = area.width.min(64);
    let height = area.height.min(30);
    let popup = Rect {
        x: area.x + (area.width.saturating_sub(width)) / 2,
        y: area.y + (area.height.saturating_sub(height)) / 2,
        width,
        height,
    };

    let rows = [
        ("1-4, tab", "switch screens"),
        ("j / k", "move the selection"),
        ("enter", "paste the selected clip"),
        ("dd", "delete the selected clip"),
        ("p", "pin or unpin"),
        ("/", "search"),
        ("a", "toggle the raw log"),
        ("space", "pause or resume capture"),
        ("", ""),
        ("s", "start a stack"),
        ("c", "start a queue capture"),
        ("S", "seal the queue"),
        ("g", "start a group capture"),
        ("G", "paste the group"),
        ("0", "reset the cursor"),
        ("x", "end the session"),
        ("", ""),
        ("", ""),
        ("a", "add a binding (Bindings)"),
        ("e", "edit the selected binding"),
        ("t", "test which binding a chord hits"),
        ("dd", "delete — clip or binding"),
        ("", ""),
        ("r", "refresh"),
        ("q", "quit"),
    ];

    let lines: Vec<Line> = rows
        .iter()
        .map(|(key, description)| {
            Line::from(vec![
                Span::styled(format!(" {key:<10}"), theme::title()),
                Span::styled(description.to_string(), theme::body()),
            ])
        })
        .collect();

    frame.render_widget(Clear, popup);
    frame.render_widget(Paragraph::new(lines).block(panel("Keys")), popup);
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::app::BindingsView;
    use copycat_core::{ClipId, ClipSummary, ContentHash};
    use ratatui::crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
    use ratatui::Terminal;
    use ratatui::backend::TestBackend;

    fn render(app: &mut App) -> String {
        let mut terminal = Terminal::new(TestBackend::new(100, 24)).unwrap();
        terminal.draw(|frame| draw(frame, app)).unwrap();
        let buffer = terminal.backend().buffer().clone();
        let area = buffer.area;
        (0..area.height)
            .map(|y| {
                (0..area.width)
                    .map(|x| buffer[(x, y)].symbol().to_string())
                    .collect::<String>()
            })
            .collect::<Vec<_>>()
            .join("\n")
    }

    fn clip(id: u64, preview: &str) -> ClipSummary {
        ClipSummary {
            id: ClipId(id),
            captured_at: 0,
            content_hash: ContentHash([0; 32]),
            media_types: vec!["text/plain".into()],
            byte_len: 1,
            preview: preview.into(),
            pinned: false,
            duplicate_run: 1,
        }
    }

    #[test]
    fn the_history_screen_lists_clips_and_marks_duplicates() {
        let mut app = App::default();
        let mut repeated = clip(2, "repeated");
        repeated.duplicate_run = 3;
        let mut pinned = clip(1, "kept");
        pinned.pinned = true;
        app.set_clips(vec![clip(3, "newest"), repeated, pinned]);

        let screen = render(&mut app);

        assert!(screen.contains("newest"));
        assert!(screen.contains("x3"), "a folded run should show its length");
        assert!(screen.contains("pinned"));
        assert!(screen.contains("History"));
    }

    #[test]
    fn an_empty_history_explains_itself_rather_than_showing_a_blank_box() {
        let mut app = App::default();
        assert!(render(&mut app).contains("Copy something"));
    }

    #[test]
    fn the_session_screen_offers_the_starting_keys_when_nothing_is_active() {
        let mut app = App { tab: Tab::Session, ..App::default() };
        let screen = render(&mut app);
        assert!(screen.contains("No active session"));
        assert!(screen.contains("stack"));
    }

    #[test]
    fn a_binding_that_cannot_fire_is_marked_on_its_own_row() {
        // Showing configured and refused bindings in one list is only useful if
        // they are told apart at a glance.
        let mut app = App { tab: Tab::Bindings, ..App::default() };
        app.set_bindings(BindingsView {
            leader: Some("ctrl+alt+space".into()),
            hotkeys: vec![copycat_protocol::Binding {
                trigger: "ctrl+alt+v".into(),
                action: "paste.next".into(),
                args: serde_json::Value::Null,
            }],
            rejected: vec![copycat_protocol::RejectedBinding {
                trigger: "ctrl+alt+v".into(),
                reason: "already registered by another application".into(),
            }],
            ..Default::default()
        });

        let screen = render(&mut app);

        assert!(screen.contains("ctrl+alt+v"));
        assert!(screen.contains("paste.next"));
        assert!(screen.contains("already registered"), "{screen}");
        assert!(screen.contains("ctrl+alt+space"), "the leader should be visible too");
    }

    #[test]
    fn the_mode_and_any_unresolved_keys_sit_in_the_bottom_left() {
        let mut app = App::default();
        assert!(render(&mut app).contains("NORMAL"));

        app.pending.push('d');
        let screen = render(&mut app);
        let footer = screen.lines().last().unwrap();
        assert!(footer.contains("NORMAL"), "{footer}");
        assert!(footer.trim_start().starts_with("NORMAL"), "mode belongs first: {footer}");
        assert!(footer.contains('d'), "a half-typed command must be visible: {footer}");
    }

    #[test]
    fn the_binding_form_shows_its_fields_and_its_complaint() {
        let mut app = App { tab: Tab::Bindings, ..App::default() };
        let press = |app: &mut App, code| {
            app.on_key(KeyEvent::new(code, KeyModifiers::NONE));
        };

        press(&mut app, KeyCode::Char('a'));
        for c in "ctrl+alt+v".chars() {
            press(&mut app, KeyCode::Char(c));
        }
        // Move to `action`, leave it blank, and submit: the form should say
        // what is wrong rather than closing and losing the typing.
        press(&mut app, KeyCode::Tab);
        press(&mut app, KeyCode::Enter);

        let screen = render(&mut app);

        assert!(screen.contains("New binding"), "{screen}");
        assert!(screen.contains("trigger"), "{screen}");
        assert!(screen.contains("ctrl+alt+v"), "typing must survive a rejected submit: {screen}");
        assert!(screen.contains("args"), "{screen}");
        assert!(screen.contains("an action is required"), "{screen}");
        assert!(screen.contains("EDIT"), "the mode chip should say the form has the keys");
    }

    #[test]
    fn help_covers_the_screen_when_asked_for() {
        let mut app = App { show_help: true, ..App::default() };
        let screen = render(&mut app);
        assert!(screen.contains("Keys"));
        assert!(screen.contains("pin or unpin"));
    }

    #[test]
    fn the_search_bar_appears_only_while_searching() {
        let mut app = App::default();
        assert!(!render(&mut app).contains("postgres"));

        app.input_mode = InputMode::Search;
        "postgres".chars().for_each(|c| app.search.push(c));
        let screen = render(&mut app);
        assert!(screen.contains("postgres"), "the query should be visible while typing");
        assert!(screen.contains("_"), "and so should the cursor");
    }

    #[test]
    fn rendering_survives_a_tiny_terminal() {
        // Panicking on a small window would take the user's terminal with it.
        let mut app = App { show_help: true, ..App::default() };
        app.set_clips(vec![clip(1, "something")]);
        for (width, height) in [(1, 1), (3, 2), (20, 4), (200, 60)] {
            let mut terminal = Terminal::new(TestBackend::new(width, height)).unwrap();
            terminal.draw(|frame| draw(frame, &mut app)).expect("should render");
        }
    }
}
