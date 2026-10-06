//! Replays a recorded task with pinned inputs and compares its result.
// Copyright (c) 2026 The iunctus Contributors
// SPDX-License-Identifier: MIT or Apache-2.0

use std::fmt::Write as _;
use std::path::Path;

use content_store::{Store, layout};
use run_database::{Database, Log, RunStart, TaskDetail};
use task_model::Digest;
use workflow_graph::{Driver, DriverError, Engine, Language, Outcome, Task};

use super::duration;
use crate::arguments::Arguments;
use crate::setup::Setup;
use crate::{Failure, failure, runs};

/// How many of the last lines of each standard output and error the text shows; the page shows
/// them whole.
const LOG_LINES: usize = 20;

pub fn command(arguments: &Arguments) -> Result<(), Failure> {
    let run = arguments.target.as_ref().map(|run| run.to_string_lossy());
    let task = (arguments.targets.first()).and_then(|task| task.parse::<u64>().ok());
    let (Some(run), Some(task)) = (run, task) else {
        return Err(Failure::usage(
            "iunctus: name a run and the number of one of its tasks, like `iunctus replay \
             1a0f1cc4c10-3a2487 4`; `iunctus runs` lists the runs",
        ));
    };
    let broken = |error: run_database::DatabaseError| failure(error.to_string());
    let database = runs::open(arguments)?;
    let original = super::run(&database, &run)?;
    if original.simulated {
        return Err(failure(format!(
            "iunctus: run {run} was simulated, so its files are made up and its tasks cannot \
             be replayed; replay a task of a run with a real executor"
        )));
    }
    let Some(detail) = database.task(&run, task).map_err(broken)? else {
        return Err(failure(format!("iunctus: run {run} has no task {task}")));
    };
    if detail.description.is_no_op() {
        return Err(failure(format!(
            "iunctus: task {task} of run {run} does nothing: it has neither a command nor an image"
        )));
    }
    // A replay runs for real, by default on this machine, with the store of the run.
    let mut arguments = arguments.clone();
    let executor = arguments
        .executor
        .get_or_insert_with(|| "local".into())
        .clone();
    if executor == "simulator" {
        return Err(Failure::usage(
            "iunctus: a replay runs the task for real; use -executor local, tes, slurm or \
             kubernetes",
        ));
    }
    if arguments.store.is_none() {
        arguments.store = original.store.clone();
    }
    let setup = Setup::load(&arguments)?;
    let store = setup.store(&arguments)?;
    let (replayed, from_store) = replayed_task(&database, &detail, &store)?;
    let language: Language = serde_json::from_value(original.language.clone().into())
        .map_err(|_| failure(format!("iunctus: run {run} has an unknown language")))?;
    let id = crate::run_id();
    let mut executor_of_run = crate::executor(
        &arguments,
        &setup,
        &id,
        true,
        false,
        &crate::repeat::Pinned::default(),
        // A replay asks for what the task asked for in its run.
        &crate::resources::Measures::default(),
    )?;
    let start = RunStart {
        id: id.clone(),
        resumes: None,
        workflow: original.workflow.clone(),
        language,
        revision: original.revision.clone(),
        arguments: std::env::args().skip(1).collect(),
        parameters: original.parameters.clone(),
        executor: executor.clone(),
        simulated: false,
        store: Some(setup.store_location(&arguments)),
        program: Some(format!("iunctus {}", env!("CARGO_PKG_VERSION"))),
        launch: (std::env::current_dir().ok()).map(|folder| folder.to_string_lossy().into_owned()),
        workflow_folder: original.workflow_folder.clone(),
        repeats: None,
    };
    let mut recorder = runs::open(&arguments)?
        .record(&start)
        .map_err(|error| failure(format!("iunctus: {error}")))?;
    recorder.replays(&run, task);
    let telemetry = setup
        .telemetry
        .as_ref()
        .and_then(|telemetry| telemetry.start(&arguments, &mut recorder));
    eprintln!(
        "iunctus: replaying task {task} {} of run {run} as run {id} on {executor}",
        replayed.name
    );
    let mut driver = Replay {
        language,
        task: Some(replayed),
        outcome: None,
    };
    let interrupt = crate::stop_on_interrupt();
    let execution = Engine::new(executor_of_run.as_mut())
        .retries(arguments.machine_retries.unwrap_or(2))
        .retry_delay(std::time::Duration::from_millis(
            arguments.retry_delay_ms.unwrap_or(1_000),
        ))
        .observer(&mut recorder)
        .cancel(interrupt.clone())
        .run(&original.workflow, &mut driver);
    interrupt.cancel();
    crate::close(recorder);
    if let Some(telemetry) = telemetry {
        telemetry.finish();
    }
    if driver.outcome.is_some() {
        print!("{}", report(&database, &run, &id, task, &from_store)?);
    }
    execution.into_result().map(|_| ()).map_err(Failure::from)
}

/// The task as it ran, to run again alone: in the image it ran in, and each input where it is
/// now: at its place when the file is still there, else in the store by its digest. The paths
/// of the inputs that come from the store come with it.
fn replayed_task(
    database: &Database,
    detail: &TaskDetail,
    store: &Store,
) -> Result<(Task, Vec<String>), Failure> {
    let mut task = detail.description.clone();
    let mut from_store = Vec::new();
    task.after.clear();
    if let Some(image) = detail
        .attempts
        .last()
        .and_then(|attempt| attempt.image.clone())
    {
        task.image = Some(image);
    }
    for input in &mut task.inputs {
        let there = input.source.contains("://") || Path::new(&input.source).exists();
        if input.content.is_some() || there {
            continue;
        }
        let recorded = (detail.inputs.iter())
            .find(|file| file.path == input.path)
            .and_then(|file| file.digest.clone());
        let digest = match recorded {
            Some(digest) => Some(digest),
            None => {
                (database.digest_of(&input.source)).map_err(|error| failure(error.to_string()))?
            }
        };
        let gone = || {
            failure(format!(
                "iunctus: the input `{}` is gone: {} is no longer there, and the store has no \
                 copy of it by its digest",
                input.path, input.source
            ))
        };
        let digest: Digest = digest
            .and_then(|digest| digest.parse().ok())
            .ok_or_else(gone)?;
        // A directory is kept as the files under its place in the store, not by one digest.
        if input.directory {
            return Err(gone());
        }
        let location = layout::content(&digest);
        // A path when the object is on this machine (also in the shared layer of a store in
        // layers), else its address.
        let source = store.url(&location);
        if !source.contains("://") && !Path::new(&source).exists() {
            return Err(gone());
        }
        input.source = source;
        from_store.push(input.path.clone());
    }
    Ok((task, from_store))
}

/// How the replay compares with the task as it ran, and the end of the logs of both. The inputs
/// that came from the store by their digest have the same content by their digest, wherever
/// they are now.
fn report(
    database: &Database,
    run: &str,
    replay: &str,
    task: u64,
    from_store: &[String],
) -> Result<String, Failure> {
    let broken = |error: run_database::DatabaseError| failure(error.to_string());
    let (Some(before), Some(now)) = (
        database.task(run, task).map_err(broken)?,
        database.task(replay, task).map_err(broken)?,
    ) else {
        return Ok(String::new());
    };
    let comparison = database.compare(run, replay).map_err(broken)?;
    let pair = (comparison.iter())
        .flat_map(|comparison| &comparison.tasks)
        .find(|pair| pair.second.as_ref().is_some_and(|side| side.task == task));
    let mut text = format!(
        "task {task} {} of run {run}, replayed as run {replay}:\n",
        now.row.name
    );
    let code = |detail: &TaskDetail| {
        detail
            .row
            .exit_code
            .map_or_else(|| "none".to_owned(), |code| code.to_string())
    };
    let _ = writeln!(
        text,
        "  exit code: {} before, {} now",
        code(&before),
        code(&now)
    );
    let time = |detail: &TaskDetail| match (detail.row.started, detail.row.ended) {
        (Some(started), Some(ended)) => duration(ended - started),
        _ => "unknown".to_owned(),
    };
    let _ = writeln!(text, "  time: {} before, {} now", time(&before), time(&now));
    match before.row.reused.as_deref() {
        Some("cache") => text.push_str("  before, its result came from the cache\n"),
        Some(earlier) => {
            let _ = writeln!(text, "  before, its result came from run {earlier}");
        }
        None => {}
    }
    if let Some(image) = now
        .attempts
        .last()
        .and_then(|attempt| attempt.image.as_deref())
    {
        let _ = writeln!(text, "  image: {image}");
    }
    for path in from_store {
        let _ = writeln!(
            text,
            "  - the input `{path}` came from the store by its digest, as its file is gone"
        );
    }
    if let Some(pair) = pair {
        let moved = |line: &&String| {
            (from_store.iter())
                .any(|path| line.starts_with(&format!("the input `{path}` comes from ")))
        };
        for line in pair
            .changes
            .iter()
            .filter(|line| !moved(line))
            .chain(&pair.outputs)
        {
            let _ = writeln!(text, "  - {line}");
        }
        let compared = !before.outputs.is_empty() && !now.outputs.is_empty();
        if compared && pair.outputs.is_empty() {
            text.push_str("  every output has the same content as before\n");
        }
    }
    for (title, stream) in [("standard output", "stdout"), ("standard error", "stderr")] {
        let log = |run: &str, detail: &TaskDetail| -> Result<Option<Log>, Failure> {
            match detail.attempts.last() {
                Some(attempt) => database
                    .log(run, task, attempt.number, stream)
                    .map_err(broken),
                None => Ok(None),
            }
        };
        for (when, found) in [("before", log(run, &before)?), ("now", log(replay, &now)?)] {
            let _ = writeln!(text, "{title} {when}:");
            match found {
                None => text.push_str("  (empty)\n"),
                Some(log) => {
                    let lines: Vec<&str> = log.text.lines().collect();
                    let skipped = lines.len().saturating_sub(LOG_LINES);
                    if skipped > 0 || !log.whole {
                        text.push_str("  ... (the page shows the whole text)\n");
                    }
                    for line in &lines[skipped..] {
                        let _ = writeln!(text, "  {line}");
                    }
                }
            }
        }
    }
    Ok(text)
}

/// The driver of a replay: it asks for the one task, and fails the run when the task fails.
struct Replay {
    language: Language,
    task: Option<Task>,
    outcome: Option<Outcome>,
}

impl Driver for Replay {
    fn language(&self) -> Language {
        self.language
    }

    fn start(&mut self) -> Result<Vec<Task>, DriverError> {
        Ok(self.task.take().into_iter().collect())
    }

    fn complete(&mut self, outcome: Outcome) -> Result<Vec<Task>, DriverError> {
        self.outcome = Some(outcome);
        Ok(Vec::new())
    }

    fn finish(&mut self) -> Result<(), DriverError> {
        match &self.outcome {
            Some(outcome) if outcome.exit_code != 0 => Err(DriverError::new(format!(
                "the replayed task failed with exit code {}",
                outcome.exit_code
            ))),
            _ => Ok(()),
        }
    }
}
