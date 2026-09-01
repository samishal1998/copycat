//! Command definitions.
//!
//! The CLI is a thin client (§11): almost every subcommand turns into exactly
//! one [`Action`] and prints the result. Nothing here decides what a stack
//! means — that lives in the daemon, so a hotkey and a command cannot drift
//! apart (ADR-003).

use std::path::PathBuf;

use clap::{Parser, Subcommand, ValueEnum};
use copycat_core::{ClipId, DuplicatePolicy};

#[derive(Parser, Debug)]
#[command(
    name = "copycat",
    version,
    about = "A programmable clipboard: stacks, queues, groups, and paste by position",
    long_about = None,
)]
pub struct Cli {
    /// Daemon socket. Defaults to the platform runtime directory.
    #[arg(long, global = true, value_name = "PATH")]
    pub socket: Option<PathBuf>,

    /// Print machine-readable JSON instead of formatted text.
    #[arg(long, global = true)]
    pub json: bool,

    #[command(subcommand)]
    pub command: Command,
}

#[derive(Subcommand, Debug)]
pub enum Command {
    /// Start, stop, or inspect the daemon.
    Daemon {
        #[command(subcommand)]
        command: DaemonCommand,
    },
    /// Report clipboard, input, storage, and key-storage capabilities.
    Doctor,
    /// Open the terminal interface.
    Tui,
    /// Show what the daemon and the OS clipboard each currently hold.
    Status,

    /// Paste an item: the latest, an offset, an id, or the session's next.
    Paste(PasteArgs),

    /// LIFO traversal of history.
    Stack {
        #[command(subcommand)]
        command: StackCommand,
    },
    /// FIFO traversal, from a snapshot or a capture.
    Queue {
        #[command(subcommand)]
        command: QueueCommand,
    },
    /// Aggregate several clips into one pasted value.
    Group {
        #[command(subcommand)]
        command: GroupCommand,
    },

    /// List, search, and manage recorded clips.
    History {
        #[command(subcommand)]
        command: HistoryCommand,
    },
    /// Inspect and reload key bindings.
    Bind {
        #[command(subcommand)]
        command: BindCommand,
    },
    /// Show the loaded configuration.
    Config {
        #[command(subcommand)]
        command: ConfigCommand,
    },
}

#[derive(Debug, clap::Args)]
pub struct PasteArgs {
    /// `latest` pastes the newest clip; `next` consumes the active session.
    #[arg(value_enum, default_value_t = PasteTarget::Latest)]
    pub target: PasteTarget,

    /// Zero-based offset from the newest clip. `--offset 1` is the one before it.
    #[arg(long, value_name = "N", conflicts_with = "id")]
    pub offset: Option<usize>,

    /// Paste a specific clip by id.
    #[arg(long, value_name = "ID")]
    pub id: Option<ClipId>,

    /// Index the raw append-only log instead of the collapsed view.
    #[arg(long)]
    pub raw: bool,

    /// Resolve and paste without advancing the active session.
    ///
    /// Only meaningful with `next`: addressing a clip by offset or id never
    /// advances a session in the first place.
    #[arg(long)]
    pub peek: bool,
}

#[derive(Copy, Clone, Debug, PartialEq, Eq, ValueEnum)]
pub enum PasteTarget {
    Latest,
    Next,
}

#[derive(Copy, Clone, Debug, PartialEq, Eq, ValueEnum)]
pub enum Duplicates {
    /// Fold runs of identical adjacent copies into one entry.
    Collapse,
    /// Keep every copy, so a value copied twice pastes twice.
    Preserve,
}

impl From<Duplicates> for DuplicatePolicy {
    fn from(value: Duplicates) -> Self {
        match value {
            Duplicates::Collapse => DuplicatePolicy::Collapse,
            Duplicates::Preserve => DuplicatePolicy::Preserve,
        }
    }
}

#[derive(Subcommand, Debug)]
pub enum DaemonCommand {
    /// Start the daemon in the background.
    Start {
        /// Run in the foreground instead of detaching.
        #[arg(long)]
        foreground: bool,
        /// Extra arguments passed through to `copycatd`.
        #[arg(last = true)]
        args: Vec<String>,
    },
    /// Ask the daemon to shut down.
    Stop,
    /// Stop the daemon and start it again.
    Restart,
    /// Report whether the daemon is reachable.
    Status,
}

#[derive(Subcommand, Debug)]
pub enum StackCommand {
    /// Begin LIFO traversal of the current history.
    Start {
        #[arg(long, value_enum, value_name = "POLICY")]
        duplicates: Option<Duplicates>,
    },
    /// End the active session.
    Stop,
    /// Show the active session.
    Status,
    /// Return the cursor to the start without changing the contents.
    Reset,
}

#[derive(Subcommand, Debug)]
pub enum QueueCommand {
    /// Snapshot the newest N clips and paste them oldest first.
    Start {
        #[arg(long, value_name = "N")]
        last: usize,
        #[arg(long, value_enum, value_name = "POLICY")]
        duplicates: Option<Duplicates>,
    },
    /// Begin an empty queue that collects everything copied from now on.
    Capture {
        #[arg(long, value_enum, value_name = "POLICY")]
        duplicates: Option<Duplicates>,
    },
    /// Stop collecting and make the queue traversable.
    Seal,
    /// End the active session.
    Stop,
    /// Show the active session.
    Status,
}

#[derive(Subcommand, Debug)]
pub enum GroupCommand {
    /// Paste several clips as one value.
    Paste {
        /// Aggregate the newest N clips instead of the captured group.
        #[arg(long, value_name = "N")]
        last: Option<usize>,
        /// Text placed between entries. Defaults to a newline.
        #[arg(long, value_name = "TEXT")]
        delimiter: Option<String>,
        /// Index the raw log instead of the collapsed view.
        #[arg(long)]
        raw: bool,
    },
    /// Begin collecting clips into a group.
    Capture {
        #[arg(long, value_name = "TEXT")]
        delimiter: Option<String>,
        #[arg(long, value_enum, value_name = "POLICY")]
        duplicates: Option<Duplicates>,
    },
    /// End the active session.
    End,
}

#[derive(Subcommand, Debug)]
pub enum HistoryCommand {
    /// List recorded clips, newest first.
    List {
        #[arg(long, default_value_t = 20, value_name = "N")]
        limit: usize,
        /// Show the raw log, including consecutive duplicates.
        #[arg(long)]
        raw: bool,
    },
    /// Print one clip in full.
    Show { id: ClipId },
    /// Find clips containing a substring.
    Search {
        query: String,
        #[arg(long, default_value_t = 20, value_name = "N")]
        limit: usize,
    },
    /// Delete one clip.
    Delete { id: ClipId },
    /// Delete recorded history.
    Clear {
        /// Keep pinned clips.
        #[arg(long)]
        keep_pinned: bool,
    },
    /// Keep a clip through retention and clears.
    Pin { id: ClipId },
    /// Undo a pin.
    Unpin { id: ClipId },
    /// Stop recording copies.
    Pause,
    /// Resume recording copies.
    Resume,
}

#[derive(Subcommand, Debug)]
pub enum BindCommand {
    /// Show configured bindings, including any the platform refused.
    List,
    /// Re-read the config and re-register bindings.
    Reload,
}

#[derive(Subcommand, Debug)]
pub enum ConfigCommand {
    /// Print the loaded configuration as TOML.
    Show,
    /// Print the path the daemon loaded its configuration from.
    Path,
}
