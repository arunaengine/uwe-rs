//! Replays a recorded task with pinned inputs and compares its result.
// Copyright (c) 2026 The uwe Contributors
// SPDX-License-Identifier: MIT or Apache-2.0

use std::fmt::Write as _;
use std::path::Path;

use content_store::Store;
use run_database::{Database, Log, RunStart, TaskDetail};
use task_model::Digest;
use workflow_graph::{Driver, DriverError, Engine, Language, Outcome, Task, Writable};

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
            "uwe: name a run and the number of one of its tasks, like `uwe replay \
             1a0f1cc4c10-3a2487 4`; `uwe runs` lists the runs",
        ));
    };
    let broken = |error: run_database::DatabaseError| failure(error.to_string());
    let database = runs::open(arguments)?;
    let original = super::run(&database, &run)?;
    if original.simulated {
        return Err(failure(format!(
            "uwe: run {run} was simulated, so its files are made up and its tasks cannot \
             be replayed; replay a task of a run with a real executor"
        )));
    }
    let Some(detail) = database.task(&run, task).map_err(broken)? else {
        return Err(failure(format!("uwe: run {run} has no task {task}")));
    };
    if detail.description.is_no_op() {
        return Err(failure(format!(
            "uwe: task {task} of run {run} does nothing: it has neither a command nor an image"
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
            "uwe: a replay runs the task for real; use -executor local, tes, slurm or \
             kubernetes",
        ));
    }
    if arguments.store.is_none() {
        arguments.store = original.store.clone();
    }
    let setup = Setup::load(&arguments)?;
    let _registry = setup.registry.enter().map_err(Failure::setup)?;
    let store = match crate::repeat::kept_by(&setup, &original)? {
        Some(store) => store,
        None => setup.store(&arguments)?,
    };
    let id = crate::run_id();
    let folder = crate::repeat::restoration_folder(&crate::cache_directory().join("replays"), &id)?;
    let (replayed, from_store) = replayed_task(&detail, &store, &folder)?;
    let language: Language = serde_json::from_value(original.language.clone().into())
        .map_err(|_| failure(format!("uwe: run {run} has an unknown language")))?;
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
        program: Some(format!("uwe {}", env!("CARGO_PKG_VERSION"))),
        launch: (std::env::current_dir().ok()).map(|folder| folder.to_string_lossy().into_owned()),
        workflow_folder: original.workflow_folder.clone(),
        repeats: None,
    };
    let mut recorder = runs::open(&arguments)?
        .record(&start)
        .map_err(|error| failure(format!("uwe: {error}")))?;
    recorder.replays(&run, task);
    let telemetry = setup
        .telemetry
        .as_ref()
        .and_then(|telemetry| telemetry.start(&arguments, &mut recorder));
    eprintln!(
        "uwe: replaying task {task} {} of run {run} as run {id} on {executor}",
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
        let files = executor_of_run.files();
        print!(
            "{}",
            report(&database, &run, &id, task, &from_store, files.as_deref())?
        );
    }
    execution.into_result().map(|_| ()).map_err(Failure::from)
}

/// Restores recorded input bytes before running a task alone in its recorded image.
fn replayed_task(
    detail: &TaskDetail,
    store: &Store,
    folder: &Path,
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
    let runtime = crate::changes::runtime()?;
    for (index, input) in task.inputs.iter_mut().enumerate() {
        if input.content.is_some() {
            continue;
        }
        let recorded = (detail.inputs.iter())
            .find(|file| file.path == input.path)
            .and_then(|file| file.digest.clone());
        let gone = || {
            failure(format!(
                "uwe: the recorded input `{}` cannot be restored or verified: {}",
                input.path, input.source
            ))
        };
        let digest: Digest = recorded
            .and_then(|digest| digest.parse().ok())
            .ok_or_else(gone)?;
        if matches!(input.writable, Writable::InPlace | Writable::Shared) {
            let place = Path::new(&input.source)
                .canonicalize()
                .map_err(|_| gone())?;
            if Digest::sha256(place.to_string_lossy().as_bytes()) != digest {
                return Err(gone());
            }
            continue;
        }
        let target = folder.join(index.to_string());
        std::fs::create_dir_all(&target).map_err(|_| gone())?;
        if input.directory {
            let copy = runtime
                .block_on(runs::kept_tree(store, &digest.to_string(), &target))
                .ok_or_else(gone)?;
            input.source = copy.to_string_lossy().into_owned();
        } else {
            let copy = target.join("input");
            let retained = runtime.block_on(store.materialize_content(&digest, &copy));
            if retained.is_err() {
                if input.source.contains("://") {
                    return Err(gone());
                }
                std::fs::copy(&input.source, &copy).map_err(|_| gone())?;
                let (actual, _) = runtime
                    .block_on(content_store::hash_file(&copy))
                    .map_err(|_| gone())?;
                if actual != digest {
                    let _ = std::fs::remove_file(&copy);
                    return Err(gone());
                }
            }
            use std::os::unix::fs::PermissionsExt;
            if input.executable
                || std::fs::metadata(&input.source)
                    .is_ok_and(|metadata| metadata.permissions().mode() & 0o111 != 0)
            {
                std::fs::set_permissions(&copy, std::fs::Permissions::from_mode(0o755))
                    .map_err(|_| gone())?;
            }
            input.source = copy.to_string_lossy().into_owned();
        }
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
    files: Option<&dyn workflow_graph::Files>,
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
            "  - the input `{path}` was restored and verified against its recorded digest"
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
                Some(mut log) => {
                    run_page::log_tail(
                        &mut log,
                        files.unwrap_or(&workflow_graph::LocalFiles),
                        run_database::LOG_LIMIT,
                    )
                    .map_err(|error| failure(format!("uwe: cannot read retained log: {error}")))?;
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

#[cfg(test)]
mod tests {
    use super::*;
    use workflow_graph::{Input, TaskId};

    #[test]
    fn displays_deferred_logs() {
        let root = tempfile::tempdir().unwrap();
        let path = root.path().join("runs.sqlite");
        let log = root.path().join("stdout");
        let bytes = b"retained stdout\n";
        std::fs::write(&log, bytes).unwrap();
        for id in ["original", "replayed"] {
            let start = RunStart {
                id: id.into(),
                resumes: None,
                workflow: "/work/main.nf".into(),
                language: Language::Nextflow,
                revision: None,
                arguments: vec![],
                parameters: vec![],
                executor: "local".into(),
                simulated: false,
                store: None,
                program: None,
                launch: None,
                workflow_folder: None,
                repeats: None,
            };
            let mut recorder = Database::open(&path).unwrap().record(&start).unwrap();
            let task = Task {
                id: TaskId(1),
                defer_logs: true,
                ..Task::default()
            };
            recorder.requested(&task, &[]);
            recorder.started(task.id, 1);
            recorder.finished(
                &Outcome {
                    id: task.id,
                    stdout_log: Some(workflow_graph::Stream {
                        location: log.to_string_lossy().into_owned(),
                        digest: Digest::sha256(bytes).to_string(),
                        size: bytes.len() as u64,
                    }),
                    ..Outcome::default()
                },
                1,
                false,
            );
            recorder.ended(None);
            recorder.close().unwrap();
        }
        let database = Database::open(&path).unwrap();
        let text = report(&database, "original", "replayed", 1, &[], None).unwrap();
        assert!(text.contains("standard output before:\n  retained stdout\n"));
        assert!(text.contains("standard output now:\n  retained stdout\n"));
    }

    #[test]
    fn restores_recorded_inputs() {
        let root = tempfile::tempdir().unwrap();
        let local = root.path().join("reference.fa");
        std::fs::write(&local, "original").unwrap();
        let store = Store::local(&root.path().join("store")).unwrap();
        let runtime = crate::changes::runtime().unwrap();
        let (digest, _) = runtime.block_on(store.put_file(&local)).unwrap();
        let path = root.path().join("runs.sqlite");
        let start = RunStart {
            id: "recorded".into(),
            resumes: None,
            workflow: "/work/main.nf".into(),
            language: Language::Nextflow,
            revision: None,
            arguments: vec![],
            parameters: vec![],
            executor: "local".into(),
            simulated: false,
            store: None,
            program: None,
            launch: None,
            workflow_folder: None,
            repeats: None,
        };
        let mut recorder = Database::open(&path).unwrap().record(&start).unwrap();
        let place = local.canonicalize().unwrap();
        let path_digest = Digest::sha256(place.to_string_lossy().as_bytes());
        for (id, source, writable, digest) in [
            (
                1,
                local.to_string_lossy().into_owned(),
                Writable::No,
                &digest,
            ),
            (
                2,
                "https://example.org/reference.fa".into(),
                Writable::No,
                &digest,
            ),
            (
                3,
                local.to_string_lossy().into_owned(),
                Writable::InPlace,
                &path_digest,
            ),
        ] {
            let task = Task {
                id: TaskId(id),
                inputs: vec![Input {
                    path: "reference.fa".into(),
                    source,
                    writable,
                    ..Input::default()
                }],
                ..Task::default()
            };
            recorder.requested(&task, &[]);
            recorder.started(task.id, 1);
            recorder.finished(
                &Outcome {
                    id: task.id,
                    inputs: [("reference.fa".into(), digest.to_string())].into(),
                    ..Outcome::default()
                },
                1,
                false,
            );
        }
        recorder.ended(None);
        recorder.close().unwrap();
        std::fs::write(&local, "changed").unwrap();
        let database = Database::open(&path).unwrap();
        for id in [1, 2] {
            let detail = database.task("recorded", id).unwrap().unwrap();
            let (task, _) =
                replayed_task(&detail, &store, &root.path().join(format!("replay-{id}"))).unwrap();
            assert_eq!(std::fs::read(&task.inputs[0].source).unwrap(), b"original");
            let empty = Store::local(&root.path().join("empty")).unwrap();
            assert!(
                replayed_task(&detail, &empty, &root.path().join(format!("missing-{id}"))).is_err()
            );
        }
        let detail = database.task("recorded", 3).unwrap().unwrap();
        let empty = Store::local(&root.path().join("empty")).unwrap();
        let folder = root.path().join("in-place");
        let (task, from_store) = replayed_task(&detail, &empty, &folder).unwrap();
        assert_eq!(task.inputs[0].source, local.to_string_lossy());
        assert_eq!(task.inputs[0].writable, Writable::InPlace);
        assert_eq!(std::fs::read(&task.inputs[0].source).unwrap(), b"changed");
        assert!(from_store.is_empty());
        assert!(!folder.exists());
        std::fs::remove_file(&local).unwrap();
        assert!(replayed_task(&detail, &empty, &folder).is_err());
        std::fs::write(&local, "original").unwrap();
        let detail = database.task("recorded", 1).unwrap().unwrap();
        let empty = Store::local(&root.path().join("empty")).unwrap();
        let (task, _) = replayed_task(&detail, &empty, &root.path().join("verified")).unwrap();
        std::fs::write(&local, "changed again").unwrap();
        assert_eq!(std::fs::read(&task.inputs[0].source).unwrap(), b"original");
    }
}
