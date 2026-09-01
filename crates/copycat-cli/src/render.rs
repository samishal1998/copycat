//! Turning daemon responses into something worth reading in a terminal.

use copycat_core::{ClipSummary, SessionSummary};
use copycat_protocol::{DoctorReport, ResultBody, StatusReport};

pub fn result(body: &ResultBody) -> String {
    match body {
        ResultBody::Done => "ok".to_string(),

        ResultBody::Pasted { clip_id, preview, bytes, skipped_non_text, injected, session } => {
            let mut lines = vec![format!(
                "pasted {} {}",
                match clip_id {
                    Some(id) => format!("#{id}"),
                    // A group aggregate is transient and has no id (R13).
                    None => "(group)".to_string(),
                },
                quote(preview)
            )];
            lines.push(format!("  {}", bytes_and_extras(*bytes, *skipped_non_text)));
            if !injected {
                // Not a warning: on a platform with no injection this is the
                // normal path, and the user needs to know to press paste.
                lines.push("  on the clipboard; press paste yourself (no injection here)".into());
            }
            if let Some(session) = session {
                lines.push(format!("  {}", session_line(session)));
            }
            lines.join("\n")
        }

        ResultBody::SessionStarted(started) => {
            let mut lines = vec![session_line(&started.session)];
            if let Some(replaced) = &started.replaced {
                lines.push(format!(
                    "  replaced the active {} session",
                    replaced.mode.as_str()
                ));
            }
            lines.join("\n")
        }

        ResultBody::Session { session: Some(session) } => session_line(session),
        ResultBody::Session { session: None } => "no active session".to_string(),

        ResultBody::Clips { clips, truncated } => {
            if clips.is_empty() {
                return "no clips".to_string();
            }
            let mut lines: Vec<String> = clips.iter().map(clip_line).collect();
            if *truncated {
                lines.push(
                    "  (search stopped at the scan limit; narrow the query or raise history.search_scan_limit)"
                        .into(),
                );
            }
            lines.join("\n")
        }

        ResultBody::Clip { clip, text } => {
            let mut lines = vec![clip_line(clip)];
            match text {
                Some(text) => {
                    lines.push(String::new());
                    lines.push(text.clone());
                }
                None => lines.push("  (no text representation)".into()),
            }
            lines.join("\n")
        }

        ResultBody::Removed { count } => format!("removed {count} clip{}", plural(*count)),

        ResultBody::Bindings { leader, sequences, hotkeys, rejected } => {
            let mut lines = Vec::new();
            lines.push(match leader {
                Some(trigger) => format!("leader  {trigger}"),
                None => "leader  disabled".to_string(),
            });
            for binding in sequences {
                lines.push(format!(
                    "  {:<10} {}{}",
                    binding.trigger,
                    binding.action,
                    render_args(&binding.args)
                ));
            }
            if !hotkeys.is_empty() {
                lines.push("hotkeys".to_string());
                for binding in hotkeys {
                    lines.push(format!(
                        "  {:<18} {}{}",
                        binding.trigger,
                        binding.action,
                        render_args(&binding.args)
                    ));
                }
            }
            if !rejected.is_empty() {
                lines.push("not active".to_string());
                for binding in rejected {
                    lines.push(format!("  {:<18} {}", binding.trigger, binding.reason));
                }
            }
            lines.join("\n")
        }

        ResultBody::Config { path, toml } => format!("# {path}\n{toml}"),
        ResultBody::Status(report) => status(report),
        ResultBody::Doctor(report) => doctor(report),
    }
}

fn bytes_and_extras(bytes: usize, skipped: usize) -> String {
    let mut parts = vec![format!("{bytes} bytes")];
    if skipped > 0 {
        parts.push(format!("{skipped} entr{} skipped for having no text", if skipped == 1 { "y" } else { "ies" }));
    }
    parts.join(", ")
}

pub fn session_line(session: &SessionSummary) -> String {
    let position = if session.size == 0 {
        "empty".to_string()
    } else {
        format!("{}/{} consumed", session.cursor.min(session.size), session.size)
    };
    format!(
        "{} [{}] {}, {} remaining, duplicates {:?}",
        session.mode.as_str(),
        match session.state {
            copycat_core::SessionState::Capturing => "capturing",
            copycat_core::SessionState::Ready => "ready",
        },
        position,
        session.remaining,
        session.duplicate_policy,
    )
}

pub fn clip_line(clip: &ClipSummary) -> String {
    let mut marks = String::new();
    if clip.pinned {
        marks.push_str(" pinned");
    }
    if clip.duplicate_run > 1 {
        marks.push_str(&format!(" x{}", clip.duplicate_run));
    }
    format!(
        "{:>5}  {:<9} {}{}",
        format!("#{}", clip.id),
        age(clip.captured_at),
        quote(&clip.preview),
        marks
    )
}

fn status(report: &StatusReport) -> String {
    let mut lines = Vec::new();
    lines.push(format!(
        "daemon      {} (protocol {}, up {})",
        report.daemon_version,
        report.protocol_version,
        duration(report.uptime_ms)
    ));
    lines.push(format!(
        "history     {} of {} hot{}",
        report.core.hot_items,
        report.core.hot_capacity,
        if report.core.paused { ", capture PAUSED" } else { "" }
    ));

    // The two values below are allowed to differ, and saying so plainly is the
    // point (R15): after a paste, the clipboard holds what Copycat wrote while
    // offset 0 is still the last thing the user actually copied.
    lines.push(format!(
        "offset 0    {}",
        report.core.latest.as_ref().map_or("(nothing)".into(), |c| quote(&c.preview))
    ));
    lines.push(format!(
        "clipboard   {}",
        report.os_clipboard.as_deref().map_or("(unreadable)".into(), quote)
    ));

    lines.push(match &report.core.session {
        Some(session) => format!("session     {}", session_line(session)),
        None => "session     none".to_string(),
    });
    lines.push(format!("backend     {}", report.clipboard_backend));
    lines.push(format!("persistence {}", report.persistence));
    lines.push(format!("key storage {}", report.key_storage));
    lines.push(format!("socket      {}", report.socket_path));
    lines.join("\n")
}

fn doctor(report: &DoctorReport) -> String {
    let mut lines = Vec::new();
    lines.push(format!("copycat {} on {}", report.daemon_version, report.platform));
    lines.push(format!("display  {} — {}", report.display_server, report.platform_support));
    lines.push(String::new());

    let width = report.checks.iter().map(|c| c.name.len()).max().unwrap_or(0);
    for check in &report.checks {
        lines.push(format!(
            "{:<12} {:<width$}  {}",
            check.status.glyph(),
            check.name,
            check.detail,
            width = width
        ));
    }

    if !report.healthy() {
        lines.push(String::new());
        lines.push("Something above is unavailable. Those are capability limits, not crashes:".into());
        lines.push("the daemon keeps running and everything else still works.".into());
    }
    lines.join("\n")
}

fn render_args(args: &serde_json::Value) -> String {
    match args {
        serde_json::Value::Null => String::new(),
        serde_json::Value::Object(map) if map.is_empty() => String::new(),
        other => format!("  {other}"),
    }
}

fn quote(text: &str) -> String {
    if text.is_empty() { "(empty)".to_string() } else { format!("\"{text}\"") }
}

fn plural(count: usize) -> &'static str {
    if count == 1 { "" } else { "s" }
}

fn duration(ms: u64) -> String {
    let seconds = ms / 1000;
    match seconds {
        0..=59 => format!("{seconds}s"),
        60..=3599 => format!("{}m", seconds / 60),
        3600..=86399 => format!("{}h", seconds / 3600),
        _ => format!("{}d", seconds / 86400),
    }
}

/// Relative time, because "3m ago" answers the question a timestamp does not.
fn age(captured_at_ms: i64) -> String {
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis() as i64)
        .unwrap_or(0);
    let delta = now.saturating_sub(captured_at_ms).max(0);
    format!("{} ago", duration(delta as u64))
}

#[cfg(test)]
mod tests {
    use super::*;
    use copycat_core::{ClipId, ContentHash};

    fn clip() -> ClipSummary {
        ClipSummary {
            id: ClipId(7),
            captured_at: 0,
            content_hash: ContentHash([0; 32]),
            media_types: vec!["text/plain".into()],
            byte_len: 5,
            preview: "hello".into(),
            pinned: true,
            duplicate_run: 3,
        }
    }

    #[test]
    fn a_clip_line_shows_pins_and_duplicate_runs() {
        let line = clip_line(&clip());
        assert!(line.contains("#7"));
        assert!(line.contains("\"hello\""));
        assert!(line.contains("pinned"));
        assert!(line.contains("x3"));
    }

    #[test]
    fn a_paste_without_injection_tells_the_user_to_press_paste() {
        let body = ResultBody::Pasted {
            clip_id: Some(ClipId(1)),
            preview: "value".into(),
            bytes: 5,
            skipped_non_text: 0,
            injected: false,
            session: None,
        };
        assert!(result(&body).contains("press paste yourself"));
    }

    #[test]
    fn skipped_group_entries_are_reported_not_hidden() {
        let body = ResultBody::Pasted {
            clip_id: None,
            preview: "a\nb".into(),
            bytes: 3,
            skipped_non_text: 2,
            injected: true,
            session: None,
        };
        let rendered = result(&body);
        assert!(rendered.contains("(group)"));
        assert!(rendered.contains("2 entries skipped"));
    }

    #[test]
    fn an_empty_listing_says_so_rather_than_printing_nothing() {
        let body = ResultBody::Clips { clips: vec![], truncated: false };
        assert_eq!(result(&body), "no clips");
    }

    #[test]
    fn a_truncated_search_says_it_stopped_looking() {
        let body = ResultBody::Clips { clips: vec![clip()], truncated: true };
        assert!(result(&body).contains("scan limit"));
    }

    #[test]
    fn durations_read_naturally() {
        assert_eq!(duration(5_000), "5s");
        assert_eq!(duration(120_000), "2m");
        assert_eq!(duration(7_200_000), "2h");
        assert_eq!(duration(172_800_000), "2d");
    }
}
