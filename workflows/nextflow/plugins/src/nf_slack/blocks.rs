//! The Block Kit blocks of nf-slack's messages: start, success, failure, progress and the
//! messages of scripts.
// Copyright (c) 2026 The uwe Contributors
// SPDX-License-Identifier: MIT or Apache-2.0

use jiff::Timestamp;
use jiff::tz::TimeZone;

use super::settings::{Message, Notice};
use crate::observer::{End, Start};
use crate::report_format::{Json, now};
use crate::run_metadata::{Ending, trace_text};

/// What the messages show of a run.
pub(super) struct Facts {
    pub(super) run_name: String,
    pub(super) work_dir: String,
    pub(super) command_line: String,
    /// The run's duration as Nextflow prints it; `None` while it runs.
    pub(super) duration: Option<String>,
    /// The counts of the tasks, like `Completed: 2`.
    pub(super) tasks: String,
    pub(super) failed_process: Option<String>,
    pub(super) error: Option<String>,
    /// The time of the message, like `Nov 6, 2026 at 12:52 PM`.
    pub(super) time: String,
}

impl Facts {
    /// What the messages show of a run, at its end when `end` is given.
    pub(super) fn of(start: Option<&Start>, end: Option<&End>) -> Facts {
        let text = |read: fn(&Start) -> &String| start.map(read).cloned().unwrap_or_default();
        let begun = start.map_or(0, |start| start.start);
        let ending = end.map(Ending::of);
        Facts {
            run_name: text(|start| &start.run_name),
            work_dir: text(|start| &start.work_dir),
            command_line: text(|start| &start.command_line),
            duration: (end.map(|end| end.end - begun))
                .map(nextflow_evaluator::units::duration_text),
            tasks: end.map_or(String::new(), |end| {
                tasks_text(end.counts.succeeded, end.counts.cached, end.counts.failed)
            }),
            failed_process: (end.and_then(|end| end.task.as_ref()))
                .map(|task| trace_text(task, "process").to_owned()),
            error: ending.and_then(|ending| ending.error_message),
            time: footer_time(now()),
        }
    }
}

#[derive(Clone, Copy, PartialEq)]
pub(super) enum Kind {
    Start,
    Success,
    Failure,
}

/// The task counts of a progress update.
pub(super) struct Counts {
    pub(super) submitted: u64,
    pub(super) completed: u64,
    pub(super) cached: u64,
    pub(super) failed: u64,
}

/// The blocks of a run's message; `footer` is false for the start message of a progress
/// update, which shows no time.
pub(super) fn notice(kind: Kind, notice: &Notice, facts: &Facts, footer: bool) -> Vec<Json> {
    let mut blocks = match &notice.message {
        Message::Text(text) => standard(kind, notice, facts, text),
        Message::Custom(custom) => {
            let text = custom.text.clone().unwrap_or_else(|| {
                let text = match kind {
                    Kind::Start => "*Pipeline started*",
                    Kind::Success => "*Pipeline completed successfully*",
                    Kind::Failure => "*Pipeline failed*",
                };
                text.to_owned()
            });
            let listed = |name: &str| custom.fields.iter().any(|field| field == name);
            let mut blocks = vec![header(&text)];
            let mut shown = shown_fields(kind, facts, &listed, true);
            shown.extend((custom.extra.iter()).map(|(title, value)| field(title, value)));
            if !shown.is_empty() {
                blocks.extend([divider(), section_fields(shown)]);
            }
            if listed("commandLine") {
                blocks.push(command(&facts.command_line));
            }
            if listed("workDir") && kind == Kind::Start {
                blocks.push(work_dir(&facts.work_dir));
            }
            if listed("errorMessage") && kind == Kind::Failure {
                blocks.push(error(facts));
            }
            blocks
        }
    };
    if footer && notice.footer {
        blocks.extend([divider(), time(&facts.time)]);
    }
    blocks
}

/// The blocks of a message given as text: its header, the fields that `includeFields` shows
/// and the sections of its kind.
fn standard(kind: Kind, notice: &Notice, facts: &Facts, text: &str) -> Vec<Json> {
    let mut blocks = vec![header(text)];
    let usage = notice.resource_usage || !notice.fields.is_empty();
    let shown = shown_fields(kind, facts, &|name| notice.shows(name), usage);
    let command_line =
        notice.shows("commandLine") && notice.command_line && !facts.command_line.is_empty();
    match kind {
        Kind::Start => {
            if notice.shows("runName") {
                let name = field("Run Name", &facts.run_name);
                blocks.extend([divider(), section_fields(vec![name])]);
            }
            if notice.shows("workDir") {
                blocks.push(work_dir(&facts.work_dir));
            }
            if command_line {
                blocks.push(command(&facts.command_line));
            }
        }
        Kind::Success => {
            if !shown.is_empty() {
                blocks.extend([divider(), section_fields(shown)]);
            }
        }
        Kind::Failure => {
            if !shown.is_empty() {
                blocks.extend([divider(), section_fields(shown)]);
            }
            if notice.shows("errorMessage") {
                blocks.extend([divider(), error(facts)]);
            }
            if command_line {
                blocks.push(command(&facts.command_line));
            }
        }
    }
    blocks
}

/// The fields of a kind of message that `shows` names: the run, its duration and status, the
/// failing process of a failure and the tasks of a success.
fn shown_fields(kind: Kind, facts: &Facts, shows: &dyn Fn(&str) -> bool, usage: bool) -> Vec<Json> {
    let mut fields = Vec::new();
    if shows("runName") {
        fields.push(field("Run Name", &facts.run_name));
    }
    if let Some(duration) = facts.duration.as_ref().filter(|_| shows("duration")) {
        fields.push(field("Duration", duration));
    }
    if shows("status") {
        let status = match kind {
            Kind::Start => "🚀 Running",
            Kind::Success => "✅ Success",
            Kind::Failure => "❌ Failed",
        };
        fields.push(field("Status", status));
    }
    if let Some(process) =
        (facts.failed_process.as_ref()).filter(|_| kind == Kind::Failure && shows("failedProcess"))
    {
        fields.push(field("Failed Process", &format!("`{process}`")));
    }
    if kind == Kind::Success && shows("tasks") && usage && !facts.tasks.is_empty() {
        fields.push(field("Tasks", &facts.tasks));
    }
    fields
}

/// The start message without its time, then the task counts and the time since the start.
pub(super) fn progress(start: Vec<Json>, counts: &Counts, elapsed: u64) -> Vec<Json> {
    let mut fields = vec![
        field("Tasks Submitted", &counts.submitted.to_string()),
        field("Tasks Completed", &counts.completed.to_string()),
        field("Tasks Cached", &counts.cached.to_string()),
    ];
    if counts.failed > 0 {
        fields.push(field("Tasks Failed", &counts.failed.to_string()));
    }
    fields.push(field("Elapsed", &elapsed_text(elapsed)));
    let mut blocks = start;
    blocks.extend([divider(), header("📊 *Progress*"), section_fields(fields)]);
    blocks
}

/// `slackMessage(text)`.
pub(super) fn text_message(text: &str) -> Vec<Json> {
    vec![header(text)]
}

/// `slackMessage(message: ..., fields: [[title: ..., value: ...]])`.
pub(super) fn rich_message(text: &str, fields: &[(String, String)]) -> Vec<Json> {
    let mut blocks = vec![header(text)];
    if !fields.is_empty() {
        let fields = fields.iter().map(|(title, value)| field(title, value));
        blocks.extend([divider(), section_fields(fields.collect())]);
    }
    blocks
}

/// The tasks of a run, like `Cached: 1, Completed: 2`, without the counts that are zero.
pub(super) fn tasks_text(succeeded: u64, cached: u64, failed: u64) -> String {
    let parts = [
        ("Cached", cached),
        ("Completed", succeeded),
        ("Failed", failed),
    ];
    let parts: Vec<String> = (parts.iter())
        .filter(|(_, count)| *count > 0)
        .map(|(name, count)| format!("{name}: {count}"))
        .collect();
    parts.join(", ")
}

/// Whole hours, minutes and seconds, like `1h 2m 3s`, `2m 3s` or `3s`.
fn elapsed_text(seconds: u64) -> String {
    let (hours, minutes, seconds) = (seconds / 3600, seconds / 60 % 60, seconds % 60);
    match (hours, minutes) {
        (0, 0) => format!("{seconds}s"),
        (0, _) => format!("{minutes}m {seconds}s"),
        _ => format!("{hours}h {minutes}m {seconds}s"),
    }
}

fn mrkdwn(text: &str) -> Json {
    Json::object([("type", Json::from("mrkdwn")), ("text", Json::from(text))])
}

pub(super) fn header(text: &str) -> Json {
    Json::object([("type", Json::from("section")), ("text", mrkdwn(text))])
}

fn divider() -> Json {
    Json::object([("type", Json::from("divider"))])
}

fn field(title: &str, value: &str) -> Json {
    mrkdwn(&format!("*{title}*\n{value}"))
}

fn section_fields(fields: Vec<Json>) -> Json {
    Json::object([
        ("type", Json::from("section")),
        ("fields", Json::List(fields)),
    ])
}

fn work_dir(folder: &str) -> Json {
    header(&format!("*Work Directory*\n`{folder}`"))
}

fn command(line: &str) -> Json {
    header(&format!("*Command Line*\n```{line}```"))
}

/// The run's error message, at most 2000 characters of it.
fn error(facts: &Facts) -> Json {
    let message = facts.error.as_deref().unwrap_or("Unknown error");
    let mut shown: String = message.chars().take(2000).collect();
    if message.chars().count() > 2000 {
        shown.push_str("...");
    }
    header(&format!("*Error Message*\n```{shown}```"))
}

fn time(text: &str) -> Json {
    Json::object([
        ("type", Json::from("context")),
        ("elements", Json::list([mrkdwn(text)])),
    ])
}

/// The time of a message in the time zone of this machine, like `Nov 6, 2026 at 12:52 PM`.
fn footer_time(millis: i64) -> String {
    match Timestamp::from_millisecond(millis) {
        Ok(time) => (time.to_zoned(TimeZone::system()))
            .strftime("%b %-d, %Y at %-I:%M %p")
            .to_string(),
        Err(_) => String::new(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn writes_counts() {
        assert_eq!(tasks_text(2, 0, 0), "Completed: 2");
        assert_eq!(tasks_text(2, 1, 3), "Cached: 1, Completed: 2, Failed: 3");
        assert_eq!(elapsed_text(3), "3s");
        assert_eq!(elapsed_text(125), "2m 5s");
        assert_eq!(elapsed_text(3725), "1h 2m 5s");
    }

    #[test]
    fn writes_footer_time() {
        let text = footer_time(1_793_965_920_000);
        assert!(text.starts_with("Nov 6, 2026 at "), "{text}");
        assert!(text.ends_with(" PM") || text.ends_with(" AM"), "{text}");
    }
}
