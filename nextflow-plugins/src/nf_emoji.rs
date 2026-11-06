//! nf-emoji 0.2.0: an emoji progress bar and summary on standard error, and emoji operators.
// Copyright (c) 2026 The uwe Contributors
// SPDX-License-Identifier: MIT or Apache-2.0

use std::io::Write;

use nextflow_config::Config;
use nextflow_evaluator::value::Value;

use crate::catalogue::{Kind, Plugin, State};
use crate::observer::{Event, Observer};

// The functions and operators are Nextflow script; the plugin's own greeting texts are not
// shipped, a greeting text of the config is printed.
pub(crate) const PLUGIN: Plugin = Plugin {
    functions: &["emojiView", "emojiDump", "printEmoji"],
    script: Some(include_str!("nf_emoji.nf")),
    scopes: &["emoji"],
    observer: Some(activate),
    ..Plugin::new("nf-emoji", Kind::Observer, State::Implemented)
};

/// The emoji of a theme: filled, empty, completed, cached, failed, summary, error.
type Theme = [&'static str; 7];

const THEMES: [(&str, Theme); 12] = [
    (
        "default",
        ["🟩", "⬜", "✅", "♻\u{fe0f}", "❌", "🏁", "☠\u{fe0f}"],
    ),
    ("space", ["🚀", "🌑", "🛸", "🌟", "💥", "🌌", "☄\u{fe0f}"]),
    ("ocean", ["🌊", "⬜", "🐟", "🐚", "🦈", "🏖\u{fe0f}", "🌀"]),
    (
        "lab",
        ["🧪", "⬜", "🔬", "📋", "☣\u{fe0f}", "🧬", "⚠\u{fe0f}"],
    ),
    ("food", ["🍕", "⬜", "🍰", "🥫", "🔥", "🍽\u{fe0f}", "🤮"]),
    (
        "pirate",
        [
            "🏴\u{200d}☠\u{fe0f}",
            "🏳\u{fe0f}",
            "💰",
            "🗺\u{fe0f}",
            "🦜",
            "⚓",
            "☠\u{fe0f}",
        ],
    ),
    ("animal", ["🐎", "⬜", "🦊", "🐢", "🦂", "🦁", "🐛"]),
    ("nf-core", ["🍏", "⬜", "🍏", "🌿", "🍎", "🌳", "🍎"]),
    ("spring", ["🌷", "⬜", "🌸", "🌱", "🥀", "🌻", "🌧\u{fe0f}"]),
    (
        "summer",
        [
            "☀\u{fe0f}",
            "⬜",
            "🏖\u{fe0f}",
            "🧊",
            "🌪\u{fe0f}",
            "🌴",
            "⛈\u{fe0f}",
        ],
    ),
    ("fall", ["🍂", "⬜", "🎃", "🍄", "💨", "🍁", "🌫\u{fe0f}"]),
    (
        "winter",
        ["❄\u{fe0f}", "⬜", "⛄", "🧣", "🥶", "🎄", "🌨\u{fe0f}"],
    ),
];

fn activate(config: &Config) -> Result<Option<Box<dyn Observer>>, String> {
    let value = |key: &str| {
        config
            .get(&["emoji", key])
            .filter(|it| !matches!(it, Value::Null))
    };
    let flag = |key: &str, default: bool| value(key).map_or(default, |it| it.truth());
    let name = value("theme").map_or_else(|| "default".into(), |it| it.to_string());
    let greeting = match value("greeting") {
        Some(Value::Boolean(_)) | None => None,
        Some(text) => Some(text.to_string()),
    };
    if flag("confetti", false) {
        tracing::warn!("nf-emoji: uwe runs no outside program, so emoji.confetti is ignored");
    }
    let observer = Emoji {
        theme: theme(&name, month()),
        bar: flag("progressBar", true),
        summary: flag("summary", true),
        greeting,
        counts: Counts::default(),
        out: std::io::stderr(),
    };
    Ok(Some(Box::new(observer)))
}

/// The theme of a name: `seasonal` by the month; an unknown name warns and gives `default`.
fn theme(name: &str, month: i8) -> Theme {
    let name = match (name, month) {
        ("seasonal", 3..=5) => "spring",
        ("seasonal", 6..=8) => "summer",
        ("seasonal", 9..=11) => "fall",
        ("seasonal", _) => "winter",
        (name, _) => name,
    };
    match THEMES.iter().find(|(known, _)| *known == name) {
        Some((_, theme)) => *theme,
        None => {
            let names: Vec<&str> = THEMES.iter().map(|(known, _)| *known).collect();
            tracing::warn!(
                "Unknown emoji theme '{name}', using default. Available: [{}]",
                names.join(", ")
            );
            THEMES[0].1
        }
    }
}

fn month() -> i8 {
    jiff::Zoned::now().month()
}

#[derive(Default)]
struct Counts {
    submitted: u64,
    done: u64,
    succeeded: u64,
    failed: u64,
    cached: u64,
}

/// Draws the bar and the summary on its output, standard error in a run.
struct Emoji<W: Write> {
    theme: Theme,
    bar: bool,
    summary: bool,
    greeting: Option<String>,
    counts: Counts,
    out: W,
}

impl<W: Write> Observer for Emoji<W> {
    fn notify(&mut self, event: &Event) {
        let counts = &mut self.counts;
        match event {
            Event::RunStarted(_) => {
                if let Some(greeting) = &self.greeting {
                    let _ = write!(self.out, "\n{greeting}\n\n");
                }
            }
            Event::TaskSubmitted(_) => counts.submitted += 1,
            Event::TaskCached(_) => {
                counts.cached += 1;
                counts.done += 1;
            }
            Event::TaskCompleted(task) | Event::TaskFailed(task) => {
                match task.record["exit"].as_i64() == Some(0) {
                    true => counts.succeeded += 1,
                    false => counts.failed += 1,
                }
                counts.done += 1;
            }
            Event::RunEnded(end) => {
                if !end.success {
                    let _ = writeln!(self.out, "{}", self.theme[6]);
                }
                if self.summary {
                    let [_, _, completed, cached, failed, summary, _] = self.theme;
                    let counts = &self.counts;
                    let _ = write!(
                        self.out,
                        "\n{summary} Pipeline complete!\n{completed} {} succeeded | {failed} {} \
                         failed | {cached} {} cached\n",
                        counts.succeeded, counts.failed, counts.cached
                    );
                }
                let _ = self.out.flush();
                return;
            }
            Event::TaskStarted(_) | Event::FilePublished { .. } => return,
        }
        // The bar shows at the start and after each task that ended.
        if self.bar && !matches!(event, Event::TaskSubmitted(_)) {
            let line = self.line();
            let _ = write!(self.out, "{line}");
            let _ = self.out.flush();
        }
    }
}

impl<W: Write> Emoji<W> {
    /// `\r`, ten emoji of progress, the share done and the counts, and three spaces.
    fn line(&self) -> String {
        let Counts {
            submitted, done, ..
        } = self.counts;
        let share = match submitted {
            0 => 0,
            _ => (done as f64 / submitted as f64 * 100.0).round() as u64,
        };
        let filled = ((share as f64 / 10.0).round() as usize).min(10);
        format!(
            "\r{}{} {share}% ({done}/{submitted})   ",
            self.theme[0].repeat(filled),
            self.theme[1].repeat(10 - filled)
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::observer::{Counts as Ended, End, Task};

    fn task(exit: i64) -> Task {
        let record = serde_json::json!({ "exit": exit });
        Task {
            record: record.as_object().cloned().unwrap_or_default(),
            process_id: None,
            hash: String::new(),
            executor: "local".into(),
            inputs: Vec::new(),
            outputs: Vec::new(),
            values: Vec::new(),
            declared: Vec::new(),
            stdout: String::new(),
            stderr: String::new(),
        }
    }

    #[test]
    fn draws_recorded_bar() {
        let mut emoji = Emoji {
            theme: theme("ocean", 11),
            bar: true,
            summary: true,
            greeting: None,
            counts: Counts::default(),
            out: Vec::new(),
        };
        emoji.notify(&Event::TaskSubmitted(task(0)));
        emoji.notify(&Event::TaskSubmitted(task(0)));
        emoji.notify(&Event::TaskCompleted(task(0)));
        emoji.notify(&Event::TaskCompleted(task(0)));
        emoji.notify(&Event::RunEnded(End {
            success: true,
            error: None,
            counts: Ended::default(),
            task: None,
            outputs: Vec::new(),
            end: 0,
        }));
        // The end of the recorded standard error, after the plugin's greeting and first bar.
        let expected = "\r🌊🌊🌊🌊🌊⬜⬜⬜⬜⬜ 50% (1/2)   \r🌊🌊🌊🌊🌊🌊🌊🌊🌊🌊 100% (2/2)   \n\
                        🏖\u{fe0f} Pipeline complete!\n🐟 2 succeeded | 🦈 0 failed | 🐚 0 cached\n";
        assert_eq!(String::from_utf8_lossy(&emoji.out), expected);
        assert_eq!(theme("seasonal", 11)[0], "🍂");
        assert_eq!(theme("cubist", 1)[0], "🟩");
    }
}
