//! `iunctus repeat <run>`: runs a past run again, exactly: with the files of its workflow and of
//! its command line as the run had them, its images at the digests it ran at, and its remote
//! files as it read them, from its store. It first prints what changed since the run, and at
//! its end compares the outputs of each step with those of the run by digest.
// Copyright (c) 2026 The iunctus Contributors
// SPDX-License-Identifier: MIT or Apache-2.0

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::time::{Duration, UNIX_EPOCH};

use content_store::{Store, layout};
use run_database::{Database, Run, Source, SourceKind};
use storage::Fetched;
use task_model::Digest;

use crate::arguments::Arguments;
use crate::changes::{self, broken, runtime};
use crate::setup::Setup;
use crate::{Failure, Ran, cache_directory, execute, failure, finish, runs};

/// What a repeat of a run pins in the new run; nothing for other runs.
#[derive(Clone, Debug, Default)]
pub struct Pinned {
    /// The run that the new run repeats.
    pub repeats: Option<String>,
    /// Images by the names that tasks give them, pinned to the digests that the run ran at.
    pub images: HashMap<String, String>,
    /// Remote files that the new run reads from copies of what the run read.
    pub files: Vec<Fetched>,
}

/// `iunctus repeat <run>`: what changed since the run, the run again with what it pinned, and
/// the steps whose outputs differ from those of the run.
pub fn command(arguments: &Arguments) -> Result<(), Failure> {
    let database = runs::open(arguments)?;
    let run = changes::named_run(&database, arguments, "repeat")?;
    if run.state == "running" {
        return Err(failure(format!(
            "iunctus: run {} is still running; repeat it when it has ended",
            run.id
        )));
    }
    let setup = Setup::load(arguments)?;
    let changes = changes::found(&database, run.clone(), &setup.storage, &changes::registry)?;
    eprint!("{}", changes::report(&changes));
    let store = kept_by(&setup, &run)?;
    let folder = restoration_folder(&cache_directory().join("repeats"), &run.id)?;
    let mut repeated = changes::recorded_arguments(&database, &run)?;
    chosen_here(&mut repeated, arguments);
    if changes.sources_changed() {
        restore(&database, &run, store.as_ref(), &folder, &mut repeated)?;
    }
    let pinned = Pinned {
        repeats: Some(run.id.clone()),
        images: images(&database, &run)?,
        files: remote_files(&database, &run, store.as_ref(), &folder)?,
    };
    // Relative paths that the workflow names start where the run started, as they did then.
    if let Some(launch) = run
        .launch
        .as_deref()
        .filter(|launch| Path::new(launch).is_dir())
    {
        std::env::set_current_dir(launch).map_err(|error| {
            Failure::setup(format!("iunctus: cannot work in {launch}: {error}"))
        })?;
    }
    let Ran { execution, id, .. } = execute(&repeated, false, &pinned)?;
    let differences = (runs::open(arguments)?.compare_steps(&run.id, &id)).map_err(broken)?;
    eprint!("{}", compared(&run, &id, &differences));
    finish(execution)
}

/// The options of `iunctus repeat` that choose where and how the run runs again: its executor
/// and store, the settings of the executor, the page, and the reproducibility check. The
/// workflow and its parameters stay those of the run.
pub(crate) fn chosen_here(repeated: &mut Arguments, given: &Arguments) {
    macro_rules! take {
        ($($field:ident),*) => {
            $(if given.$field.is_some() { repeated.$field = given.$field.clone(); })*
        };
    }
    take!(
        executor,
        store,
        config_file,
        tes_server,
        slurm_container,
        slurm_options,
        kubeconfig,
        kubernetes_namespace,
        default_image,
        max_pending,
        machine_retries,
        retry_delay_ms,
        order,
        database,
        address,
        port,
        check_reproducibility
    );
    repeated.page |= given.page;
    repeated.resume = None;
}

pub(crate) fn restoration_folder(root: &Path, run: &str) -> Result<PathBuf, Failure> {
    let parent = root.join(run);
    std::fs::create_dir_all(&parent).map_err(|error| Failure::setup(error.to_string()))?;
    let now = std::time::SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_nanos();
    for number in 0_u64.. {
        let folder = parent.join(format!("{}-{now}-{number}", std::process::id()));
        match std::fs::create_dir(&folder) {
            Ok(()) => return Ok(folder),
            Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => continue,
            Err(error) => return Err(Failure::setup(error.to_string())),
        }
    }
    unreachable!()
}

/// The store that kept the files of the run: none for a simulated run.
pub fn kept_by(setup: &Setup, run: &Run) -> Result<Option<Store>, Failure> {
    let Some(location) = &run.store else {
        return Ok(None);
    };
    let at = Arguments {
        store: Some(location.clone()),
        ..Arguments::default()
    };
    setup.store(&at).map(Some)
}

/// Puts the files of the workflow and of the command line of the run, as the run had them,
/// into `folder`, and points the command line at them.
fn restore(
    database: &Database,
    run: &Run,
    store: Option<&Store>,
    folder: &Path,
    repeated: &mut Arguments,
) -> Result<(), Failure> {
    let Some(store) = store else {
        return Err(failure(format!(
            "iunctus: the files of the workflow changed since run {}, and a simulated run keeps \
             no copy of them; restore them, or run the workflow again with `iunctus run`",
            run.id
        )));
    };
    let sources = database.sources(&run.id).map_err(broken)?;
    let (Some(workflow_folder), false) = (&run.workflow_folder, sources.is_empty()) else {
        return Err(failure(format!(
            "iunctus: run {} recorded no files of its workflow, so its workflow cannot be \
             restored",
            run.id
        )));
    };
    let workflow = folder.join("workflow");
    let runtime = runtime()?;
    let mut named: HashMap<PathBuf, PathBuf> = HashMap::new();
    for (index, source) in sources.iter().enumerate() {
        let target = match source.kind {
            SourceKind::Workflow => workflow.join(&source.path),
            SourceKind::Named => {
                let name = Path::new(&source.path).file_name().unwrap_or_default();
                let target = folder.join("named").join(index.to_string()).join(name);
                named.insert(PathBuf::from(&source.path), target.clone());
                target
            }
        };
        runtime.block_on(copy_out(store, source, &target))?;
    }
    let inside = Path::new(&run.workflow)
        .strip_prefix(workflow_folder)
        .unwrap_or(Path::new(""));
    repeated.target = Some(workflow.join(inside));
    let moved = |path: &PathBuf| named.get(path).cloned().unwrap_or_else(|| path.clone());
    repeated.configs = repeated.configs.iter().map(moved).collect();
    repeated.inputs = repeated.inputs.as_ref().map(moved);
    eprintln!(
        "iunctus: the repeat runs the workflow as run {} had it, restored from the store into {}",
        run.id,
        workflow.display()
    );
    Ok(())
}

/// Writes a file that the store keeps by its digest to `target`.
async fn copy_out(store: &Store, source: &Source, target: &Path) -> Result<(), Failure> {
    let failed = |error: String| {
        failure(format!(
            "iunctus: cannot restore {} from the store: {error}",
            source.path
        ))
    };
    let digest: Digest = (source.digest.parse()).map_err(|_| failed("no valid digest".into()))?;
    let bytes = store
        .get_content(&digest)
        .await
        .map_err(|e| failed(e.to_string()))?;
    if let Some(parent) = target.parent() {
        std::fs::create_dir_all(parent).map_err(|error| failed(error.to_string()))?;
    }
    std::fs::write(target, &bytes).map_err(|error| failed(error.to_string()))
}

/// The images of the run that ran at a digest, by the names that tasks gave them.
fn images(database: &Database, run: &Run) -> Result<HashMap<String, String>, Failure> {
    let images = database.images(&run.id).map_err(broken)?;
    Ok((images.into_iter())
        .filter(|image| image.named.starts_with("conda-env:") || image.pinned())
        .map(|image| (image.named, image.ran))
        .collect())
}

/// The remote files of the run as copies of what it read, where its store keeps them. A file
/// that the store does not keep is read again from its address.
fn remote_files(
    database: &Database,
    run: &Run,
    store: Option<&Store>,
    folder: &Path,
) -> Result<Vec<Fetched>, Failure> {
    let files = database.remote_files(&run.id).map_err(broken)?;
    let runtime = runtime()?;
    let mut pinned = Vec::new();
    for file in files {
        let copy = match store {
            Some(store) => runtime.block_on(kept_copy(store, &file.digest, folder)),
            None => None,
        };
        let Some(copy) = copy else {
            tracing::warn!(
                "iunctus: the store does not keep {} as run {} read it; the repeat reads \
                 it again from its address",
                file.address,
                run.id
            );
            continue;
        };
        pinned.push(Fetched {
            address: file.address,
            copy,
            digest: file.digest,
            size: file.size,
            time: UNIX_EPOCH + Duration::from_millis(u64::try_from(file.fetched).unwrap_or(0)),
            version: file.version,
        });
    }
    Ok(pinned)
}

/// A local copy of content that the store keeps: the file of a local store itself, else a copy
/// in `folder`. `None` when the store does not have it.
pub async fn kept_copy(store: &Store, digest: &str, folder: &Path) -> Option<PathBuf> {
    let digest: Digest = digest.parse().ok()?;
    if !store.has_content(&digest).await.ok()? {
        return None;
    }
    let location = layout::content(&digest);
    if let Some(path) = store.local_file(&location).await.ok()? {
        return Some(path);
    }
    let copy = folder.join("remote").join(digest.hex());
    std::fs::create_dir_all(copy.parent()?).ok()?;
    store.materialize_content(&digest, &copy).await.ok()?;
    Some(copy)
}

/// Whether the repeat made the same outputs as the run, step by step.
fn compared(run: &Run, repeat: &str, differences: &[run_database::StepDifference]) -> String {
    if differences.is_empty() {
        return format!(
            "iunctus: repeat {repeat} made the same outputs as run {} in every step\n",
            run.id
        );
    }
    let mut text = format!(
        "iunctus: repeat {repeat} differs from run {} in {} of its steps:\n",
        run.id,
        differences.len()
    );
    for difference in differences {
        let outputs = match difference.outputs.as_slice() {
            [] => String::new(),
            paths => format!(": other bytes in {}", paths.join(", ")),
        };
        text.push_str(&format!("  {}{outputs}\n", difference.step));
    }
    text
}

#[cfg(test)]
mod tests {
    use super::*;
    use run_database::RunStart;
    use workflow_graph::{Language, Outcome, Task, TaskId};

    #[test]
    fn restorations_stay_independent() {
        let root = tempfile::tempdir().expect("a folder");
        let first = restoration_folder(root.path(), "original")
            .unwrap_or_else(|error| panic!("{}", error.message));
        std::fs::write(first.join("main.nf"), "original workflow").expect("writes");
        let second = restoration_folder(root.path(), "original")
            .unwrap_or_else(|error| panic!("{}", error.message));
        std::fs::write(second.join("main.nf"), "other workflow").expect("writes");
        assert_ne!(first, second);
        assert_eq!(
            std::fs::read_to_string(first.join("main.nf")).expect("reads"),
            "original workflow"
        );
    }

    #[test]
    fn preserves_conda_markers() {
        let root = tempfile::tempdir().expect("a folder");
        let database_path = root.path().join("runs.sqlite");
        let start = RunStart {
            id: "original".into(),
            resumes: None,
            workflow: "/work/Snakefile".into(),
            language: Language::Snakemake,
            revision: None,
            arguments: vec!["run".into()],
            parameters: Vec::new(),
            executor: "local".into(),
            simulated: false,
            store: Some("/store".into()),
            program: None,
            launch: Some("/work".into()),
            workflow_folder: Some("/work".into()),
            repeats: None,
        };
        let database = Database::open(&database_path).expect("opens");
        let mut recorder = database.record(&start).expect("records");
        let local = format!("sha256:{}", "a".repeat(64));
        for (id, named, ran) in [
            (1, "conda-env:/work/env.yaml", local.as_str()),
            (2, "registry/image:latest", "registry/image:latest"),
        ] {
            let task = Task {
                id: TaskId(id),
                image: Some(named.into()),
                command: vec!["true".into()],
                ..Task::default()
            };
            recorder.requested(&task, &[]);
            recorder.started(task.id, 1);
            recorder.finished(
                &Outcome {
                    id: task.id,
                    exit_code: 0,
                    image: Some(ran.into()),
                    accepted: Some(true),
                    ..Outcome::default()
                },
                1,
                false,
            );
        }
        recorder.close().expect("closes");
        let database = Database::open(&database_path).expect("opens again");
        let run = database.run("original").expect("reads").expect("a run");

        assert_eq!(
            images(&database, &run).unwrap_or_else(|error| panic!("{}", error.message)),
            [("conda-env:/work/env.yaml".into(), local),].into()
        );
    }
}
