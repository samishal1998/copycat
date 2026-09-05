//! The Copycat terminal interface.
//!
//! A client of the daemon like any other (ADR-003): it holds no clipboard state
//! and decides nothing about what a stack means. Every key turns into an
//! [`AppRequest`], which turns into exactly one protocol action.

pub mod app;
pub mod theme;
pub mod ui;

use std::path::Path;
use std::time::{Duration, Instant};

use copycat_core::{CoreError, ErrorKind};
use copycat_protocol::{Action, ResultBody};
use ratatui::crossterm::event::{self, Event, KeyEventKind};

pub use app::{App, AppRequest, Tab};

/// How often to pull fresh state from the daemon.
///
/// Polling rather than subscribing to an event stream: the TUI needs a
/// consistent snapshot of several things at once, and at this interval a poll
/// is cheaper to get right than a subscription plus reconciliation would be.
const REFRESH: Duration = Duration::from_millis(500);
const TICK: Duration = Duration::from_millis(100);
const LIST_LIMIT: usize = 200;

pub fn run(socket: &Path) -> Result<(), CoreError> {
    // Fail before taking over the terminal, so the error is readable.
    if !copycat_protocol::is_running(socket) {
        return Err(CoreError::new(
            ErrorKind::DaemonUnavailable,
            "daemon_unavailable",
            // No "start it with ..." here: the CLI appends that hint to every
            // daemon_unavailable error, and printing it twice reads as a stutter.
            format!("cannot reach the daemon at {}", socket.display()),
        ));
    }

    let mut terminal = ratatui::init();
    let enhanced = request_keyboard_enhancement();
    let result = event_loop(&mut terminal, socket, enhanced);
    release_keyboard_enhancement(enhanced);
    ratatui::restore();
    result
}

/// Ask the terminal for the Kitty keyboard protocol.
///
/// `ratatui::init` does not, and without it a terminal reports only shift,
/// ctrl and alt — Command and Super never arrive, so a chord bound to them
/// looks unbound rather than unreportable. Only `DISAMBIGUATE_ESCAPE_CODES` is
/// requested: it is what makes modified keys carry their full modifier set,
/// and asking for more would turn ordinary typing into escape codes for no
/// gain here.
///
/// Returns whether the terminal agreed, because that answer has to be shown to
/// the user rather than assumed.
fn request_keyboard_enhancement() -> bool {
    use ratatui::crossterm::event::{KeyboardEnhancementFlags, PushKeyboardEnhancementFlags};
    use ratatui::crossterm::execute;
    use ratatui::crossterm::terminal::supports_keyboard_enhancement;

    if !supports_keyboard_enhancement().unwrap_or(false) {
        return false;
    }
    execute!(
        std::io::stdout(),
        PushKeyboardEnhancementFlags(KeyboardEnhancementFlags::DISAMBIGUATE_ESCAPE_CODES)
    )
    .is_ok()
}

fn release_keyboard_enhancement(enhanced: bool) {
    if !enhanced {
        return;
    }
    use ratatui::crossterm::event::PopKeyboardEnhancementFlags;
    use ratatui::crossterm::execute;
    // Leaving the flags pushed would change how the user's shell reads keys
    // after copycat exits.
    let _ = execute!(std::io::stdout(), PopKeyboardEnhancementFlags);
}

fn event_loop(
    terminal: &mut ratatui::DefaultTerminal,
    socket: &Path,
    keyboard_enhanced: bool,
) -> Result<(), CoreError> {
    let mut app = App { keyboard_enhanced, ..App::default() };
    refresh(&mut app, socket);
    let mut last_refresh = Instant::now();

    loop {
        terminal.draw(|frame| ui::draw(frame, &mut app)).map_err(terminal_error)?;

        if event::poll(TICK).map_err(terminal_error)? {
            match event::read().map_err(terminal_error)? {
                // Windows reports key releases too; acting on both would run
                // every binding twice.
                Event::Key(key) if key.kind == KeyEventKind::Press => {
                    for request in app.on_key(key) {
                        perform(&mut app, socket, request);
                    }
                    last_refresh = Instant::now().checked_sub(REFRESH).unwrap_or(last_refresh);
                }
                _ => {}
            }
        }

        if app.should_quit {
            return Ok(());
        }
        if last_refresh.elapsed() >= REFRESH {
            refresh(&mut app, socket);
            last_refresh = Instant::now();
        }
    }
}

fn terminal_error(error: std::io::Error) -> CoreError {
    CoreError::new(ErrorKind::InvalidRequest, "terminal_error", format!("{error}"))
}

fn perform(app: &mut App, socket: &Path, request: AppRequest) {
    let action = match request {
        AppRequest::Refresh => {
            refresh(app, socket);
            return;
        }
        AppRequest::Paste(id) => Action::PasteId { id },
        AppRequest::Delete(id) => Action::HistoryDelete { id },
        AppRequest::SetPinned(id, pinned) => Action::HistoryPin { id, pinned },
        AppRequest::StackStart => Action::StackStart { duplicates: None },
        AppRequest::QueueCapture => Action::QueueCapture { duplicates: None },
        AppRequest::QueueSeal => Action::QueueSeal,
        AppRequest::GroupCapture => Action::GroupCapture { delimiter: None, duplicates: None },
        AppRequest::GroupPaste => Action::GroupPaste,
        AppRequest::SessionStop => Action::SessionStop,
        AppRequest::SessionReset => Action::SessionReset,
        AppRequest::ReloadBindings => Action::BindReload,
        AppRequest::SetBinding { kind, trigger, action, args } => {
            Action::BindSet { kind, trigger, action, args }
        }
        AppRequest::RemoveBinding { kind, trigger } => Action::BindRemove { kind, trigger },
        AppRequest::SetLeader { trigger, enabled } => Action::BindLeader { trigger, enabled },
        AppRequest::TogglePause => {
            let paused = app.status.as_ref().is_some_and(|s| s.core.paused);
            if paused { Action::HistoryResume } else { Action::HistoryPause }
        }
    };

    match copycat_protocol::call(socket, action) {
        Ok(ResultBody::Bindings { leader, sequences, hotkeys, rejected }) => {
            // A binding edit replies with the list as it now stands, so there
            // is nothing to go and ask for.
            app.set_bindings(app::BindingsView { leader, sequences, hotkeys, rejected });
            app.note("bindings updated");
        }
        Ok(body) => {
            app.note(describe(&body));
            refresh(app, socket);
        }
        // The daemon's message is already written for a person; repeating it is
        // better than inventing a second vocabulary for the same failure.
        Err(error) => app.error(error.message),
    }
}

fn describe(body: &ResultBody) -> String {
    match body {
        ResultBody::Pasted { preview, injected, .. } => {
            if *injected {
                format!("pasted {preview:?}")
            } else {
                format!("{preview:?} is on the clipboard — press paste yourself")
            }
        }
        ResultBody::SessionStarted(started) => match &started.replaced {
            Some(replaced) => format!(
                "{} started, replacing the {}",
                started.session.mode.as_str(),
                replaced.mode.as_str()
            ),
            None => format!("{} started", started.session.mode.as_str()),
        },
        ResultBody::Session { session: Some(session) } => {
            format!("{} session updated", session.mode.as_str())
        }
        ResultBody::Session { session: None } => "session ended".to_string(),
        ResultBody::Removed { count } => format!("removed {count}"),
        _ => "done".to_string(),
    }
}

/// Pull the state the current screen needs.
fn refresh(app: &mut App, socket: &Path) {
    let listing = if app.search.is_empty() {
        Action::HistoryList { limit: LIST_LIMIT, raw: app.raw }
    } else {
        Action::HistorySearch { query: app.search.clone(), limit: LIST_LIMIT }
    };

    match copycat_protocol::call(socket, listing) {
        Ok(ResultBody::Clips { clips, .. }) => app.set_clips(clips),
        Ok(_) => {}
        Err(error) => app.error(error.message),
    }

    if let Ok(ResultBody::Status(status)) = copycat_protocol::call(socket, Action::Status) {
        app.status = Some(*status);
    }

    // Diagnostics and bindings change only when the config does, so they are
    // fetched for the screens that show them rather than on every tick.
    match app.tab {
        Tab::Diagnostics => {
            if let Ok(ResultBody::Doctor(report)) = copycat_protocol::call(socket, Action::Doctor) {
                app.doctor = Some(*report);
            }
        }
        Tab::Bindings => {
            if let Ok(ResultBody::Bindings { leader, sequences, hotkeys, rejected }) =
                copycat_protocol::call(socket, Action::BindList)
            {
                app.set_bindings(app::BindingsView { leader, sequences, hotkeys, rejected });
            }
        }
        _ => {}
    }
}
