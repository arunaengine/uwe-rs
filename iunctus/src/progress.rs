//! What `iunctus run` shows of a real run, as `nextflow run` shows its log: a line that names
//! the run, then a line per step with its counts, and at the end how the run went and where
//! its results went. In a terminal, the lines of the steps are drawn again in place as the run
//! goes (`terminal.rs`); elsewhere, like in a log file, each task that ends prints its own line,
//! like `[done] SAY (1)`, and the summary comes at the end. The same for every language.
// Copyright (c) 2026 The iunctus Contributors
// SPDX-License-Identifier: MIT or Apache-2.0

use std::collections::HashMap;
use std::path::Path;
use std::sync::{Arc, Mutex, PoisonError};
use std::time::{Duration, Instant};

use workflow_graph::{Event, Language, Observer, TaskId};

use crate::terminal::Live;

/// How many results the summary names one by one in a folder; more are counted.
const NAMED_RESULTS: usize = 6;

/// The display of a run: the board of its steps, drawn live in a terminal.
pub struct Progress {
    board: Arc<Mutex<Board>>,
    live: Option<Live>,
}

impl Progress {
    /// Prints the line of the run, and in a terminal starts drawing the steps live.
    pub fn start(header: Header<'_>) -> Progress {
        let board = Arc::new(Mutex::new(Board::new(header.executor)));
        let line = header.line();
        let live = Live::start(board.clone());
        match &live {
            Some(live) => live.print(&format!("\x1b[1m{line}\x1b[0m")),
            None => println!("{line}"),
        }
        Progress { board, live }
    }

    /// Ends the display: the steps with their counts (in a terminal, their last drawing stays),
    /// then the summary, which names the store, where the results went, the report of the run
    /// and the commands that show more of the run.
    pub fn finish(self, run: &str, store: Option<&str>, report: Option<&Path>) {
        let steps = match self.live {
            Some(live) => {
                live.finish();
                Vec::new()
            }
            None => (self.board.lock().unwrap_or_else(PoisonError::into_inner)).lines(false),
        };
        let board = self.board.lock().unwrap_or_else(PoisonError::into_inner);
        let folder = std::env::current_dir().unwrap_or_default();
        for line in steps
            .into_iter()
            .chain(board.summary(&folder, run, store, report))
        {
            println!("{line}");
        }
    }

    /// Hears the events of the run, before `inner` does.
    pub fn observer<'a>(&'a self, inner: &'a mut dyn Observer) -> Watching<'a> {
        Watching {
            board: &self.board,
            inner,
        }
    }
}

/// What the line of a run names.
pub struct Header<'a> {
    pub target: &'a Path,
    pub language: Language,
    pub executor: &'a str,
    pub run: &'a str,
}

impl Header<'_> {
    fn line(&self) -> String {
        let language = match self.language {
            Language::Nextflow => "Nextflow",
            Language::Snakemake => "Snakemake",
            Language::Cwl => "CWL",
            Language::Argo => "Argo",
        };
        format!(
            "iunctus {}: running {} ({language}) on {}, run {}",
            env!("CARGO_PKG_VERSION"),
            self.target.display(),
            self.executor,
            self.run
        )
    }
}

/// An observer that notes each event on the board, then passes it on.
pub struct Watching<'a> {
    board: &'a Mutex<Board>,
    inner: &'a mut dyn Observer,
}

impl Observer for Watching<'_> {
    fn event(&mut self, event: Event<'_>) {
        (self.board.lock().unwrap_or_else(PoisonError::into_inner)).note(&event);
        self.inner.event(event);
    }
}

/// The steps of a run, in the order of their first task, and where its results went.
pub struct Board {
    executor: String,
    started: Instant,
    took: Option<Duration>,
    steps: Vec<Step>,
    step_of: HashMap<TaskId, usize>,
    names: HashMap<TaskId, String>,
    running: usize,
    attempts: usize,
    failed: bool,
    results: Vec<String>,
}

/// The tasks of one step: how many the run asked for, and how they ended.
#[derive(Default)]
struct Step {
    name: String,
    asked: usize,
    done: usize,
    cached: usize,
    failed: usize,
    /// The task that last started or ended, whose name the line shows.
    last: Option<TaskId>,
}

impl Board {
    fn new(executor: &str) -> Board {
        Board {
            executor: executor.to_owned(),
            started: Instant::now(),
            took: None,
            steps: Vec::new(),
            step_of: HashMap::new(),
            names: HashMap::new(),
            running: 0,
            attempts: 0,
            failed: false,
            results: Vec::new(),
        }
    }

    fn note(&mut self, event: &Event<'_>) {
        match event {
            Event::Requested { task, .. } => {
                let index = match self.steps.iter().position(|step| step.name == task.step) {
                    Some(index) => index,
                    None => {
                        self.steps.push(Step {
                            name: task.step.clone(),
                            ..Step::default()
                        });
                        self.steps.len() - 1
                    }
                };
                self.steps[index].asked += 1;
                self.step_of.insert(task.id, index);
                self.names.insert(task.id, one_line(&task.name));
            }
            Event::Started { task, .. } => {
                self.running += 1;
                self.attempts += 1;
                if let Some(step) = self.step_mut(*task) {
                    step.last = Some(*task);
                }
            }
            Event::Finished {
                outcome, retried, ..
            } => {
                self.running = self.running.saturating_sub(1);
                if *retried {
                    return;
                }
                let Some(step) = self.step_mut(outcome.id) else {
                    return;
                };
                step.last = Some(outcome.id);
                match (outcome.cached, outcome.exit_code) {
                    (true, _) => {
                        step.done += 1;
                        step.cached += 1;
                    }
                    (false, 0) => step.done += 1,
                    (false, _) => step.failed += 1,
                }
            }
            Event::Delivered { delivery } => {
                if !self.results.contains(&delivery.target) {
                    self.results.push(delivery.target.clone());
                }
            }
            Event::Ended { error } => {
                self.took = Some(self.started.elapsed());
                self.failed = error.is_some();
                self.running = 0;
            }
            Event::Marked { .. } | Event::Validated { .. } => {}
        }
    }

    fn step_mut(&mut self, task: TaskId) -> Option<&mut Step> {
        let index = *self.step_of.get(&task)?;
        self.steps.get_mut(index)
    }

    /// The lines of the steps, as the terminal shows them: the line of the executor, then one
    /// line per step, like `[3] SAY (3) | 2 of 3, cached: 1`, with a check mark once the run
    /// ended well and a cross for a step with failed tasks. Without `color`, plain text.
    pub fn lines(&self, color: bool) -> Vec<String> {
        let paint = |code: &str, text: &str| match color {
            true => format!("\x1b[{code}m{text}\x1b[0m"),
            false => text.to_owned(),
        };
        let seconds = self
            .took
            .unwrap_or_else(|| self.started.elapsed())
            .as_secs();
        let mut lines = vec![paint(
            "2",
            &format!(
                "executor >  {} ({}, {} running, {})",
                self.executor,
                tasks(self.attempts),
                self.running,
                clock(seconds)
            ),
        )];
        let label = |step: &Step| {
            (step.last.and_then(|task| self.names.get(&task)))
                .cloned()
                .unwrap_or_else(|| step.name.clone())
        };
        let id = |step: &Step| step.last.map_or("-".to_owned(), |task| task.to_string());
        let id_width = self.steps.iter().map(|step| id(step).len()).max();
        let label_width = self
            .steps
            .iter()
            .map(|step| label(step).chars().count())
            .max();
        for step in &self.steps {
            let mut counts = format!("{} of {}", step.done, step.asked);
            if step.cached > 0 {
                counts.push_str(&format!(", cached: {}", step.cached));
            }
            if step.failed > 0 {
                counts.push_str(&format!(", failed: {}", step.failed));
            }
            let mark = match (step.failed > 0, self.took.is_some() && !self.failed) {
                (true, _) => format!(" {}", paint("31", "\u{2718}")),
                (false, true) if step.done == step.asked => format!(" {}", paint("32", "\u{2714}")),
                _ => String::new(),
            };
            lines.push(format!(
                "[{}] {:<label_width$} | {counts}{mark}",
                paint(
                    "34",
                    &format!("{:>id_width$}", id(step), id_width = id_width.unwrap_or(1))
                ),
                label(step),
                label_width = label_width.unwrap_or(0),
            ));
        }
        lines
    }

    /// What the run did, where its results went, its report, and the commands that show more
    /// of it. `folder` is the folder of the run, from which results and the report are named by
    /// relative paths.
    pub fn summary(
        &self,
        folder: &Path,
        run: &str,
        store: Option<&str>,
        report: Option<&Path>,
    ) -> Vec<String> {
        let sum = |count: fn(&Step) -> usize| self.steps.iter().map(count).sum::<usize>();
        let (asked, cached, failed) = (sum(|s| s.asked), sum(|s| s.cached), sum(|s| s.failed));
        let ran = sum(|s| s.done) - cached;
        let took = self.took.unwrap_or_else(|| self.started.elapsed());
        let how = match self.failed {
            true => "Failed after",
            false => "Done in",
        };
        let mut lines = vec![
            String::new(),
            format!(
                "{how} {}: {}, {ran} ran, {cached} from the cache, {failed} failed",
                clock(took.as_secs()),
                tasks(asked)
            ),
        ];
        if !self.results.is_empty() {
            lines.push("Results:".to_owned());
            lines.extend(results(&self.results, folder).map(|line| format!("  {line}")));
        }
        if let Some(store) = store {
            lines.push(format!("Files of every task: {store}"));
        }
        if let Some(report) = report {
            let shown = report.strip_prefix(folder).unwrap_or(report);
            lines.push(format!("Report: {}", shown.display()));
        }
        lines.push(format!(
            "More: `iunctus runs`, `iunctus page {run}`, `iunctus time {run}`"
        ));
        lines
    }
}

/// A name on one line: names of tasks may hold the text of a parameter, line breaks and all.
fn one_line(name: &str) -> String {
    let words: Vec<&str> = name.split_whitespace().collect();
    words.join(" ")
}

/// A number of tasks, like `1 task` or `3 tasks`.
fn tasks(count: usize) -> String {
    match count {
        1 => "1 task".to_owned(),
        count => format!("{count} tasks"),
    }
}

/// A time in seconds as a clock, like `42s`, `3m 05s` or `1h 02m`.
fn clock(seconds: u64) -> String {
    match seconds {
        0..60 => format!("{seconds}s"),
        60..3600 => format!("{}m {:02}s", seconds / 60, seconds % 60),
        _ => format!("{}h {:02}m", seconds / 3600, seconds / 60 % 60),
    }
}

/// The results, one line per folder in the order of their first result: the folder, and its
/// files by name, or how many there are when they are many. Paths in `base` are relative.
fn results<'a>(targets: &'a [String], base: &Path) -> impl Iterator<Item = String> + 'a {
    let mut folders: Vec<(String, Vec<String>)> = Vec::new();
    for target in targets {
        let shown = (Path::new(target).strip_prefix(base))
            .map_or(target.clone(), |relative| relative.display().to_string());
        let (folder, name) = match shown.rsplit_once('/') {
            Some((folder, name)) => (format!("{folder}/"), name.to_owned()),
            None => ("./".to_owned(), shown),
        };
        match folders.iter_mut().find(|(known, _)| *known == folder) {
            Some((_, names)) => names.push(name),
            None => folders.push((folder, vec![name])),
        }
    }
    folders.into_iter().map(|(folder, mut names)| {
        names.sort();
        match names.len() {
            count if count > NAMED_RESULTS => format!("{folder} ({count} files)"),
            _ => format!("{folder} {}", names.join(", ")),
        }
    })
}

#[cfg(test)]
mod tests {
    use std::path::Path;

    use workflow_graph::{Delivery, Event, ExecuteError, Outcome, Task, TaskId};

    use super::Board;

    fn task(id: u64, step: &str, name: &str) -> Task {
        Task {
            id: TaskId(id),
            step: step.to_owned(),
            name: name.to_owned(),
            ..Task::default()
        }
    }

    fn ended(board: &mut Board, id: u64, exit_code: i32, cached: bool) {
        board.note(&Event::Started {
            task: TaskId(id),
            attempt: 1,
        });
        let outcome = Outcome {
            id: TaskId(id),
            exit_code,
            cached,
            ..Outcome::default()
        };
        board.note(&Event::Finished {
            outcome: &outcome,
            attempt: 1,
            retried: false,
        });
    }

    fn delivered(board: &mut Board, target: &str) {
        let delivery = Delivery {
            location: String::new(),
            target: target.to_owned(),
            mode: Default::default(),
        };
        board.note(&Event::Delivered {
            delivery: &delivery,
        });
    }

    #[test]
    fn summarizes_step_results() {
        let mut board = Board::new("slurm");
        for (id, name) in [(1, "SAY (1)"), (2, "SAY (2)"), (3, "SAY (3)")] {
            board.note(&Event::Requested {
                task: &task(id, "SAY", name),
                edges: &[],
            });
        }
        board.note(&Event::Requested {
            task: &task(4, "JOIN", "JOIN"),
            edges: &[],
        });
        ended(&mut board, 1, 0, true);
        ended(&mut board, 3, 0, false);
        let lines = board.lines(false);
        assert!(lines[0].starts_with("executor >  slurm (2 tasks, 0 running, "));
        assert_eq!(lines[1], "[3] SAY (3) | 2 of 3, cached: 1");
        assert_eq!(lines[2], "[-] JOIN    | 0 of 1");
        ended(&mut board, 2, 0, false);
        ended(&mut board, 4, 0, false);
        for target in ["a.txt", "b.txt", "all.txt"] {
            delivered(&mut board, &format!("/work/demo/results/{target}"));
        }
        delivered(&mut board, "s3://bucket/report.html");
        board.note(&Event::Ended { error: None });
        let lines = board.lines(false);
        assert_eq!(lines[1], "[2] SAY (2) | 3 of 3, cached: 1 \u{2714}");
        assert_eq!(lines[2], "[4] JOIN    | 1 of 1 \u{2714}");
        let summary = board.summary(
            Path::new("/work/demo"),
            "r1",
            Some("/vol/cache/store"),
            Some(Path::new("/work/demo/report.html")),
        );
        assert_eq!(
            summary[1],
            "Done in 0s: 4 tasks, 3 ran, 1 from the cache, 0 failed"
        );
        assert_eq!(
            summary[2..],
            [
                "Results:",
                "  results/ a.txt, all.txt, b.txt",
                "  s3://bucket/ report.html",
                "Files of every task: /vol/cache/store",
                "Report: report.html",
                "More: `iunctus runs`, `iunctus page r1`, `iunctus time r1`",
            ]
        );
    }

    #[test]
    fn failure_marks_progress() {
        let mut board = Board::new("local");
        board.note(&Event::Requested {
            task: &task(7, "align", "align (sample=A)"),
            edges: &[],
        });
        ended(&mut board, 7, 2, false);
        let error = ExecuteError::Cancelled;
        board.note(&Event::Ended {
            error: Some(&error),
        });
        assert_eq!(
            board.lines(false)[1],
            "[7] align (sample=A) | 0 of 1, failed: 1 \u{2718}"
        );
        let summary = board.summary(Path::new("/"), "r2", None, None);
        assert_eq!(
            summary[1],
            "Failed after 0s: 1 task, 0 ran, 0 from the cache, 1 failed"
        );
        assert_eq!(summary.len(), 3);
    }

    #[test]
    fn counts_directory_results() {
        let targets: Vec<String> = (0..8).map(|n| format!("/d/out/{n}.txt")).collect();
        let lines: Vec<String> = super::results(&targets, Path::new("/d")).collect();
        assert_eq!(lines, ["out/ (8 files)"]);
        assert_eq!(super::clock(185), "3m 05s");
        assert_eq!(
            super::one_line("shout(0:Hello!\ngreeted)"),
            "shout(0:Hello! greeted)"
        );
        assert_eq!(super::clock(3720), "1h 02m");
    }
}
