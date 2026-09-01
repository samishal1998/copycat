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

use crate::app::{App, InputMode, Tab};
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

    if app.show_help {
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
    let line = match &app.message {
        Some(message) if message.is_error => Line::from(Span::styled(
            format!(" {}", message.text),
            Style::default().fg(theme::ACCENT).bold(),
        )),
        Some(message) => {
            Line::from(Span::styled(format!(" {}", message.text), theme::notice()))
        }
        None => Line::from(Span::styled(
            match app.tab {
                Tab::History => " enter paste · d delete · p pin · / search · a raw · ? help",
                Tab::Session => " s stack · c queue · S seal · g group · G paste · x stop · ? help",
                Tab::Bindings => " enter reload · ? help",
                Tab::Diagnostics => " r refresh · ? help",
            },
            theme::label(),
        )),
    };
    frame.render_widget(Paragraph::new(line), area);
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

fn draw_bindings(frame: &mut Frame, area: Rect, app: &App) {
    let mut lines = Vec::new();

    lines.push(field(
        "leader",
        app.bindings.leader.as_deref().unwrap_or("disabled"),
    ));
    lines.push(Line::from(""));

    if !app.bindings.sequences.is_empty() {
        lines.push(Line::from(Span::styled("sequences", theme::title())));
        for binding in &app.bindings.sequences {
            lines.push(binding_line(&binding.trigger, &binding.action));
        }
        lines.push(Line::from(""));
    }

    if !app.bindings.hotkeys.is_empty() {
        lines.push(Line::from(Span::styled("hotkeys", theme::title())));
        for binding in &app.bindings.hotkeys {
            lines.push(binding_line(&binding.trigger, &binding.action));
        }
        lines.push(Line::from(""));
    }

    // Rejected bindings are the reason this screen exists: a key that silently
    // does nothing is the worst failure mode a binding can have.
    if !app.bindings.rejected.is_empty() {
        lines.push(Line::from(Span::styled("not active", theme::notice().bold())));
        for rejected in &app.bindings.rejected {
            lines.push(Line::from(vec![
                Span::styled(format!("  {:<16}", rejected.trigger), theme::notice()),
                Span::styled(rejected.reason.clone(), theme::label()),
            ]));
        }
    }

    frame.render_widget(
        Paragraph::new(lines).wrap(Wrap { trim: false }).block(panel("Bindings")),
        area,
    );
}

fn binding_line<'a>(trigger: &str, action: &str) -> Line<'a> {
    Line::from(vec![
        Span::styled(format!("  {trigger:<16}"), theme::body().bold()),
        Span::styled(action.to_string(), theme::label()),
    ])
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
    let height = area.height.min(22);
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
        ("d", "delete the selected clip"),
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
    fn rejected_bindings_are_shown_prominently() {
        // A binding that silently does nothing is the failure this screen is
        // for, so it must be impossible to miss.
        let mut app = App {
            tab: Tab::Bindings,
            bindings: BindingsView {
                leader: Some("ctrl+alt+space".into()),
                rejected: vec![copycat_protocol::RejectedBinding {
                    trigger: "ctrl+alt+v".into(),
                    reason: "already registered by another application".into(),
                }],
                ..Default::default()
            },
            ..App::default()
        };

        let screen = render(&mut app);

        assert!(screen.contains("not active"));
        assert!(screen.contains("ctrl+alt+v"));
        assert!(screen.contains("already registered"));
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
