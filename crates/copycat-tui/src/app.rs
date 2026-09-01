//! TUI state and key handling.
//!
//! Deliberately free of any terminal or socket: keys go in, [`AppRequest`]s
//! come out, and the runner performs them. That makes every binding testable
//! without a pty, and keeps the rule from ADR-003 intact — the TUI decides
//! nothing about clipboard semantics, it only asks the daemon.

use copycat_core::{ClipId, ClipSummary};
use copycat_protocol::{Binding, DoctorReport, RejectedBinding, StatusReport};
use ratatui::crossterm::event::{KeyCode, KeyEvent, KeyModifiers};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Tab {
    History,
    Session,
    Bindings,
    Diagnostics,
}

impl Tab {
    pub const ALL: [Tab; 4] = [Tab::History, Tab::Session, Tab::Bindings, Tab::Diagnostics];

    pub fn title(self) -> &'static str {
        match self {
            Tab::History => "History",
            Tab::Session => "Session",
            Tab::Bindings => "Bindings",
            Tab::Diagnostics => "Diagnostics",
        }
    }

    fn next(self) -> Tab {
        let index = Tab::ALL.iter().position(|t| *t == self).unwrap_or(0);
        Tab::ALL[(index + 1) % Tab::ALL.len()]
    }

    fn previous(self) -> Tab {
        let index = Tab::ALL.iter().position(|t| *t == self).unwrap_or(0);
        Tab::ALL[(index + Tab::ALL.len() - 1) % Tab::ALL.len()]
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum InputMode {
    Normal,
    Search,
}

/// Something the runner should ask the daemon to do.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum AppRequest {
    Refresh,
    Paste(ClipId),
    Delete(ClipId),
    SetPinned(ClipId, bool),
    StackStart,
    QueueCapture,
    QueueSeal,
    GroupCapture,
    GroupPaste,
    SessionStop,
    SessionReset,
    TogglePause,
    ReloadBindings,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Message {
    pub text: String,
    pub is_error: bool,
}

#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub struct BindingsView {
    pub leader: Option<String>,
    pub sequences: Vec<Binding>,
    pub hotkeys: Vec<Binding>,
    pub rejected: Vec<RejectedBinding>,
}

pub struct App {
    pub tab: Tab,
    pub clips: Vec<ClipSummary>,
    pub selected: usize,
    pub status: Option<StatusReport>,
    pub doctor: Option<DoctorReport>,
    pub bindings: BindingsView,
    pub search: String,
    pub input_mode: InputMode,
    pub message: Option<Message>,
    pub raw: bool,
    pub show_help: bool,
    pub should_quit: bool,
}

impl Default for App {
    fn default() -> Self {
        App {
            tab: Tab::History,
            clips: Vec::new(),
            selected: 0,
            status: None,
            doctor: None,
            bindings: BindingsView::default(),
            search: String::new(),
            input_mode: InputMode::Normal,
            message: None,
            raw: false,
            show_help: false,
            should_quit: false,
        }
    }
}

impl App {
    pub fn selected_clip(&self) -> Option<&ClipSummary> {
        self.clips.get(self.selected)
    }

    pub fn set_clips(&mut self, clips: Vec<ClipSummary>) {
        self.clips = clips;
        // Keep the cursor inside the list when it shrinks under us — a delete
        // or a clear from another client should not leave it pointing past the
        // end.
        self.selected = self.selected.min(self.clips.len().saturating_sub(1));
    }

    pub fn note(&mut self, text: impl Into<String>) {
        self.message = Some(Message { text: text.into(), is_error: false });
    }

    pub fn error(&mut self, text: impl Into<String>) {
        self.message = Some(Message { text: text.into(), is_error: true });
    }

    /// Translate a key press into zero or more daemon requests.
    pub fn on_key(&mut self, key: KeyEvent) -> Vec<AppRequest> {
        if self.input_mode == InputMode::Search {
            return self.search_key(key);
        }
        if self.show_help {
            // Any key dismisses help, and only dismisses it: a keystroke aimed
            // at the help screen should not also delete a clip.
            self.show_help = false;
            return Vec::new();
        }

        self.message = None;

        match key.code {
            KeyCode::Char('c') if key.modifiers.contains(KeyModifiers::CONTROL) => {
                self.should_quit = true;
            }
            KeyCode::Char('q') | KeyCode::Esc => self.should_quit = true,
            KeyCode::Char('?') => self.show_help = true,

            KeyCode::Tab => self.tab = self.tab.next(),
            KeyCode::BackTab => self.tab = self.tab.previous(),
            KeyCode::Char('1') => self.tab = Tab::History,
            KeyCode::Char('2') => self.tab = Tab::Session,
            KeyCode::Char('3') => self.tab = Tab::Bindings,
            KeyCode::Char('4') => self.tab = Tab::Diagnostics,

            KeyCode::Char('j') | KeyCode::Down => self.move_selection(1),
            KeyCode::Char('k') | KeyCode::Up => self.move_selection(-1),
            KeyCode::Home => self.selected = 0,
            KeyCode::End => self.selected = self.clips.len().saturating_sub(1),

            KeyCode::Char('r') => return vec![AppRequest::Refresh],
            KeyCode::Char('/') if self.tab == Tab::History => {
                self.input_mode = InputMode::Search;
                self.search.clear();
            }
            KeyCode::Char('a') if self.tab == Tab::History => {
                self.raw = !self.raw;
                return vec![AppRequest::Refresh];
            }

            KeyCode::Enter => {
                return match self.tab {
                    Tab::History => self
                        .selected_clip()
                        .map(|clip| vec![AppRequest::Paste(clip.id)])
                        .unwrap_or_default(),
                    Tab::Bindings => vec![AppRequest::ReloadBindings],
                    _ => Vec::new(),
                };
            }
            KeyCode::Char('d') if self.tab == Tab::History => {
                if let Some(clip) = self.selected_clip() {
                    return vec![AppRequest::Delete(clip.id)];
                }
            }
            KeyCode::Char('p') if self.tab == Tab::History => {
                if let Some(clip) = self.selected_clip() {
                    return vec![AppRequest::SetPinned(clip.id, !clip.pinned)];
                }
            }
            KeyCode::Char(' ') => return vec![AppRequest::TogglePause],

            // Session controls. Available from any screen: they are the fastest
            // path to starting a mode, and hunting for the right tab first
            // would defeat that.
            KeyCode::Char('s') => return vec![AppRequest::StackStart],
            KeyCode::Char('c') => return vec![AppRequest::QueueCapture],
            KeyCode::Char('S') => return vec![AppRequest::QueueSeal],
            KeyCode::Char('g') => return vec![AppRequest::GroupCapture],
            KeyCode::Char('G') => return vec![AppRequest::GroupPaste],
            KeyCode::Char('x') => return vec![AppRequest::SessionStop],
            KeyCode::Char('0') => return vec![AppRequest::SessionReset],
            _ => {}
        }
        Vec::new()
    }

    fn search_key(&mut self, key: KeyEvent) -> Vec<AppRequest> {
        match key.code {
            KeyCode::Esc => {
                self.input_mode = InputMode::Normal;
                self.search.clear();
                return vec![AppRequest::Refresh];
            }
            KeyCode::Enter => {
                self.input_mode = InputMode::Normal;
                return vec![AppRequest::Refresh];
            }
            KeyCode::Backspace => {
                self.search.pop();
            }
            KeyCode::Char(c) => self.search.push(c),
            _ => return Vec::new(),
        }
        vec![AppRequest::Refresh]
    }

    fn move_selection(&mut self, delta: isize) {
        if self.clips.is_empty() {
            self.selected = 0;
            return;
        }
        let last = self.clips.len() - 1;
        self.selected = match delta {
            d if d < 0 => self.selected.saturating_sub(d.unsigned_abs()),
            d => (self.selected + d as usize).min(last),
        };
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use copycat_core::ContentHash;

    fn key(code: KeyCode) -> KeyEvent {
        KeyEvent::new(code, KeyModifiers::NONE)
    }

    fn clip(id: u64, pinned: bool) -> ClipSummary {
        ClipSummary {
            id: ClipId(id),
            captured_at: 0,
            content_hash: ContentHash([0; 32]),
            media_types: vec!["text/plain".into()],
            byte_len: 1,
            preview: format!("clip {id}"),
            pinned,
            duplicate_run: 1,
        }
    }

    fn app_with_clips() -> App {
        let mut app = App::default();
        app.set_clips(vec![clip(3, false), clip(2, true), clip(1, false)]);
        app
    }

    fn app_on(tab: Tab) -> App {
        App { tab, ..App::default() }
    }

    #[test]
    fn selection_stays_inside_the_list() {
        let mut app = app_with_clips();
        for _ in 0..10 {
            app.on_key(key(KeyCode::Char('j')));
        }
        assert_eq!(app.selected, 2);
        for _ in 0..10 {
            app.on_key(key(KeyCode::Char('k')));
        }
        assert_eq!(app.selected, 0);
    }

    #[test]
    fn selection_survives_the_list_shrinking_underneath_it() {
        let mut app = app_with_clips();
        app.selected = 2;
        app.set_clips(vec![clip(3, false)]);
        assert_eq!(app.selected, 0);
    }

    #[test]
    fn enter_pastes_the_selected_clip() {
        let mut app = app_with_clips();
        app.on_key(key(KeyCode::Char('j')));
        assert_eq!(app.on_key(key(KeyCode::Enter)), vec![AppRequest::Paste(ClipId(2))]);
    }

    #[test]
    fn pin_toggles_against_the_clip_state() {
        let mut app = app_with_clips();
        assert_eq!(
            app.on_key(key(KeyCode::Char('p'))),
            vec![AppRequest::SetPinned(ClipId(3), true)]
        );
        app.selected = 1; // already pinned
        assert_eq!(
            app.on_key(key(KeyCode::Char('p'))),
            vec![AppRequest::SetPinned(ClipId(2), false)]
        );
    }

    #[test]
    fn an_empty_list_yields_no_requests() {
        let mut app = App::default();
        assert!(app.on_key(key(KeyCode::Enter)).is_empty());
        assert!(app.on_key(key(KeyCode::Char('d'))).is_empty());
        assert!(app.on_key(key(KeyCode::Char('p'))).is_empty());
    }

    #[test]
    fn search_captures_text_instead_of_commands() {
        let mut app = app_with_clips();
        app.on_key(key(KeyCode::Char('/')));
        assert_eq!(app.input_mode, InputMode::Search);

        // 'q' would quit in normal mode; here it is just a letter.
        for c in "qdx".chars() {
            app.on_key(key(KeyCode::Char(c)));
        }
        assert_eq!(app.search, "qdx");
        assert!(!app.should_quit);

        app.on_key(key(KeyCode::Backspace));
        assert_eq!(app.search, "qd");

        app.on_key(key(KeyCode::Esc));
        assert_eq!(app.input_mode, InputMode::Normal);
        assert!(app.search.is_empty(), "escape abandons the search");
        assert!(!app.should_quit, "escape leaves search rather than quitting");
    }

    #[test]
    fn help_swallows_the_key_that_dismisses_it() {
        // Otherwise dismissing help with 'd' would also delete a clip.
        let mut app = app_with_clips();
        app.on_key(key(KeyCode::Char('?')));
        assert!(app.show_help);

        let requests = app.on_key(key(KeyCode::Char('d')));

        assert!(!app.show_help);
        assert!(requests.is_empty());
        assert_eq!(app.clips.len(), 3);
    }

    #[test]
    fn tabs_cycle_in_both_directions() {
        let mut app = App::default();
        app.on_key(key(KeyCode::Tab));
        assert_eq!(app.tab, Tab::Session);
        app.on_key(key(KeyCode::BackTab));
        assert_eq!(app.tab, Tab::History);
        app.on_key(key(KeyCode::Char('4')));
        assert_eq!(app.tab, Tab::Diagnostics);
        app.on_key(key(KeyCode::Tab));
        assert_eq!(app.tab, Tab::History, "the last tab wraps to the first");
    }

    #[test]
    fn ctrl_c_quits() {
        let mut app = App::default();
        app.on_key(KeyEvent::new(KeyCode::Char('c'), KeyModifiers::CONTROL));
        assert!(app.should_quit);
    }

    #[test]
    fn plain_c_starts_a_queue_capture_rather_than_quitting() {
        let mut app = App::default();
        assert_eq!(app.on_key(key(KeyCode::Char('c'))), vec![AppRequest::QueueCapture]);
        assert!(!app.should_quit);
    }

    #[test]
    fn session_controls_work_from_every_tab() {
        let mut app = app_on(Tab::Diagnostics);
        assert_eq!(app.on_key(key(KeyCode::Char('s'))), vec![AppRequest::StackStart]);
        assert_eq!(app.on_key(key(KeyCode::Char('x'))), vec![AppRequest::SessionStop]);
    }

    #[test]
    fn toggling_the_raw_view_asks_for_fresh_data() {
        let mut app = app_with_clips();
        assert_eq!(app.on_key(key(KeyCode::Char('a'))), vec![AppRequest::Refresh]);
        assert!(app.raw);
    }
}
