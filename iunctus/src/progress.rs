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

/// How many results the summary names one by one in a folder; more are counted.
const NAMED_RESULTS: usize = 6;

/// The display of a run: the board of its steps.
pub struct Progress {
    board: Arc<Mutex<Board>>,
}

impl Progress {
    /// Prints the line of the run.
    pub fn start(header: Header<'_>) -> Progress {
        let board = Arc::new(Mutex::new(Board::new(header.executor)));
        println!("{}", header.line());
        Progress { board }
    }

    /// Ends the display: the steps with their counts, then the summary, which names the
    /// store, where the results went and the commands that show more of the run.
    pub fn finish(self, run: &str, store: Option<&str>) {
        let board = self.board.lock().unwrap_or_else(PoisonError::into_inner);
        let folder = std::env::current_dir().unwrap_or_default();
        for line in board
            .lines(false)
            .into_iter()
            .chain(board.summary(&folder, run, store))
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
                self.names.insert(task.id, task.name.clone());
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
            Event::Marked { .. } => {}
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
                "executor >  {} ({} tasks, {} running, {})",
                self.executor,
                self.attempts,
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

    /// What the run did, where its results went, and the commands that show more of it.
    /// `folder` is the folder of the run, from which results are named by relative paths.
    pub fn summary(&self, folder: &Path, run: &str, store: Option<&str>) -> Vec<String> {
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
                "{how} {}: {asked} tasks, {ran} ran, {cached} from the cache, {failed} failed",
                clock(took.as_secs())
            ),
        ];
        if !self.results.is_empty() {
            lines.push("Results:".to_owned());
            lines.extend(results(&self.results, folder).map(|line| format!("  {line}")));
        }
        if let Some(store) = store {
            lines.push(format!("Files of every task: {store}"));
        }
        lines.push(format!(
            "More: `iunctus runs`, `iunctus page {run}`, `iunctus time {run}`"
        ));
        lines
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
