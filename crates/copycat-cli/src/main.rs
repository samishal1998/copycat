//! The Copycat command line.

mod cli;
mod daemon;
mod render;

use std::path::PathBuf;
use std::process::ExitCode;

use clap::Parser;
use copycat_core::{CoreError, ErrorKind};
use copycat_protocol::{Action, ResultBody};

use crate::cli::*;

fn main() -> ExitCode {
    let cli = Cli::parse();

    let socket = match resolve_socket(cli.socket.clone()) {
        Ok(socket) => socket,
        Err(error) => return fail(&error, cli.json),
    };

    match run(&cli, &socket) {
        Ok(Some(body)) => {
            if cli.json {
                println!("{}", serde_json::to_string_pretty(&body).unwrap_or_default());
            } else {
                let text = render::result(&body);
                if !text.is_empty() {
                    println!("{text}");
                }
            }
            ExitCode::SUCCESS
        }
        Ok(None) => ExitCode::SUCCESS,
        Err(error) => fail(&error, cli.json),
    }
}

/// Errors go to stderr with the daemon's own machine-readable code, and the
/// exit status is the one CLI_SPEC documents — so a script can branch on the
/// reason without parsing the message.
fn fail(error: &CoreError, json: bool) -> ExitCode {
    if json {
        eprintln!("{}", serde_json::to_string_pretty(error).unwrap_or_default());
    } else {
        eprintln!("copycat: {} [{}]", error.message, error.code);
        if error.kind == ErrorKind::DaemonUnavailable {
            eprintln!("hint: start it with `copycat daemon start`");
        }
    }
    ExitCode::from(error.exit_code() as u8)
}

fn resolve_socket(override_path: Option<PathBuf>) -> Result<PathBuf, CoreError> {
    match override_path {
        Some(path) => Ok(path),
        None => copycat_protocol::default_socket_path().ok_or_else(|| {
            CoreError::invalid(
                "no_socket_path",
                "cannot determine a socket path; pass --socket",
            )
        }),
    }
}

fn run(cli: &Cli, socket: &std::path::Path) -> Result<Option<ResultBody>, CoreError> {
    match &cli.command {
        Command::Daemon { command } => daemon::run(command, socket, cli.json).map(|_| None),
        Command::Tui => Err(CoreError::new(
            ErrorKind::InvalidRequest,
            "tui_unavailable",
            "this build does not include the terminal interface",
        )),
        command => copycat_protocol::call(socket, action_for(command)?).map(Some),
    }
}

/// Map a subcommand to the single daemon action it stands for.
fn action_for(command: &Command) -> Result<Action, CoreError> {
    Ok(match command {
        Command::Doctor => Action::Doctor,
        Command::Status => Action::Status,

        Command::Paste(args) => paste_action(args),

        Command::Stack { command } => match command {
            StackCommand::Start { duplicates } => {
                Action::StackStart { duplicates: duplicates.map(Into::into) }
            }
            StackCommand::Stop => Action::SessionStop,
            StackCommand::Status => Action::SessionStatus,
            StackCommand::Reset => Action::SessionReset,
        },

        Command::Queue { command } => match command {
            QueueCommand::Start { last, duplicates } => {
                Action::QueueStart { last: *last, duplicates: duplicates.map(Into::into) }
            }
            QueueCommand::Capture { duplicates } => {
                Action::QueueCapture { duplicates: duplicates.map(Into::into) }
            }
            QueueCommand::Seal => Action::QueueSeal,
            QueueCommand::Stop => Action::SessionStop,
            QueueCommand::Status => Action::SessionStatus,
        },

        Command::Group { command } => match command {
            // `--last` aggregates history; without it, the captured group.
            GroupCommand::Paste { last: Some(last), delimiter, raw } => Action::GroupPasteLast {
                last: *last,
                delimiter: delimiter.clone(),
                raw: *raw,
            },
            GroupCommand::Paste { last: None, .. } => Action::GroupPaste,
            GroupCommand::Capture { delimiter, duplicates } => Action::GroupCapture {
                delimiter: delimiter.clone(),
                duplicates: duplicates.map(Into::into),
            },
            GroupCommand::End => Action::SessionStop,
        },

        Command::History { command } => match command {
            HistoryCommand::List { limit, raw } => {
                Action::HistoryList { limit: *limit, raw: *raw }
            }
            HistoryCommand::Show { id } => Action::HistoryShow { id: *id },
            HistoryCommand::Search { query, limit } => {
                Action::HistorySearch { query: query.clone(), limit: *limit }
            }
            HistoryCommand::Delete { id } => Action::HistoryDelete { id: *id },
            HistoryCommand::Clear { keep_pinned } => {
                Action::HistoryClear { keep_pinned: *keep_pinned }
            }
            HistoryCommand::Pin { id } => Action::HistoryPin { id: *id, pinned: true },
            HistoryCommand::Unpin { id } => Action::HistoryPin { id: *id, pinned: false },
            HistoryCommand::Pause => Action::HistoryPause,
            HistoryCommand::Resume => Action::HistoryResume,
        },

        Command::Bind { command } => match command {
            BindCommand::List => Action::BindList,
            BindCommand::Reload => Action::BindReload,
        },

        Command::Config { command } => match command {
            ConfigCommand::Show | ConfigCommand::Path => Action::ConfigShow,
        },

        Command::Daemon { .. } | Command::Tui => {
            return Err(CoreError::invalid("not_an_action", "handled locally"));
        }
    })
}

fn paste_action(args: &PasteArgs) -> Action {
    if let Some(id) = args.id {
        return Action::PasteId { id };
    }
    if let Some(offset) = args.offset {
        return Action::PasteOffset { offset, raw: args.raw };
    }
    match args.target {
        PasteTarget::Latest => Action::PasteLatest { raw: args.raw },
        PasteTarget::Next => Action::PasteNext { peek: args.peek },
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use clap::{CommandFactory, Parser};

    fn action_of(argv: &[&str]) -> Action {
        let cli = Cli::try_parse_from(argv).expect("should parse");
        action_for(&cli.command).expect("should map to an action")
    }

    #[test]
    fn paste_forms_map_to_distinct_actions() {
        assert_eq!(action_of(&["copycat", "paste"]), Action::PasteLatest { raw: false });
        assert_eq!(action_of(&["copycat", "paste", "next"]), Action::PasteNext { peek: false });
        assert_eq!(
            action_of(&["copycat", "paste", "next", "--peek"]),
            Action::PasteNext { peek: true }
        );
        assert_eq!(
            action_of(&["copycat", "paste", "--offset", "4"]),
            Action::PasteOffset { offset: 4, raw: false }
        );
        assert_eq!(
            action_of(&["copycat", "paste", "--offset", "1", "--raw"]),
            Action::PasteOffset { offset: 1, raw: true }
        );
        assert_eq!(
            action_of(&["copycat", "paste", "--id", "9"]),
            Action::PasteId { id: copycat_core::ClipId(9) }
        );
    }

    #[test]
    fn offset_and_id_cannot_be_combined() {
        assert!(Cli::try_parse_from(["copycat", "paste", "--offset", "1", "--id", "2"]).is_err());
    }

    #[test]
    fn every_mode_stop_ends_whatever_session_is_active() {
        // R4: one session exists, so these are the same action by design.
        assert_eq!(action_of(&["copycat", "stack", "stop"]), Action::SessionStop);
        assert_eq!(action_of(&["copycat", "queue", "stop"]), Action::SessionStop);
        assert_eq!(action_of(&["copycat", "group", "end"]), Action::SessionStop);
    }

    #[test]
    fn group_paste_chooses_between_history_and_the_captured_group() {
        assert_eq!(action_of(&["copycat", "group", "paste"]), Action::GroupPaste);
        assert_eq!(
            action_of(&["copycat", "group", "paste", "--last", "3", "--delimiter", ", "]),
            Action::GroupPasteLast { last: 3, delimiter: Some(", ".into()), raw: false }
        );
    }

    #[test]
    fn duplicate_policy_is_optional_and_passes_through() {
        assert_eq!(
            action_of(&["copycat", "stack", "start"]),
            Action::StackStart { duplicates: None }
        );
        assert_eq!(
            action_of(&["copycat", "stack", "start", "--duplicates", "preserve"]),
            Action::StackStart { duplicates: Some(copycat_core::DuplicatePolicy::Preserve) }
        );
    }

    #[test]
    fn pin_and_unpin_are_one_action_with_a_flag() {
        assert_eq!(
            action_of(&["copycat", "history", "pin", "3"]),
            Action::HistoryPin { id: copycat_core::ClipId(3), pinned: true }
        );
        assert_eq!(
            action_of(&["copycat", "history", "unpin", "3"]),
            Action::HistoryPin { id: copycat_core::ClipId(3), pinned: false }
        );
    }

    #[test]
    fn the_command_tree_is_internally_consistent() {
        Cli::command().debug_assert();
    }
}
