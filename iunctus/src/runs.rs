//! The commands that read the run database: `iunctus runs` lists the runs,
//! `iunctus export <run>` writes the Workflow Run RO-Crate of a run, and
//! `iunctus why <run> <task>` tells why a task ran instead of taking an earlier result, and
//! `iunctus origin <file>` tells where a file came from.
// Copyright (c) 2026 The iunctus Contributors
// SPDX-License-Identifier: MIT or Apache-2.0

use std::fmt::Write as _;
use std::path::{Path, PathBuf};

use run_database::{Database, DatabaseError, Run};
use workflow_graph::Language;

use crate::arguments::Arguments;
use crate::{Failure, cache_directory, detect, failure};

/// How many runs `iunctus runs` lists.
const LISTED: u32 = 50;

/// The run database of the command: `-database`, else `runs.sqlite` in the cache directory,
/// next to the default store.
pub fn database_path(arguments: &Arguments) -> PathBuf {
    (arguments.database.clone()).unwrap_or_else(|| cache_directory().join(run_database::FILE_NAME))
}

pub fn open(arguments: &Arguments) -> Result<Database, Failure> {
    let path = database_path(arguments);
    Database::open(&path).map_err(|error| {
        let why = match error {
            DatabaseError::Sqlite(error) => error.to_string(),
            DatabaseError::Io(error) => error.to_string(),
            error => error.to_string(),
        };
        Failure::setup(format!(
            "iunctus: {}: cannot open the run database: {why}; name another file with -database",
            path.display()
        ))
    })
}

/// `iunctus runs`: the latest runs, one line each.
pub fn list(arguments: &Arguments) -> Result<(), Failure> {
    let runs = open(arguments)?
        .runs(LISTED)
        .map_err(|error| failure(error.to_string()))?;
    print!("{}", table(&runs));
    Ok(())
}

/// The runs as a table with a header line.
fn table(runs: &[Run]) -> String {
    let mut text = format!(
        "{:<20} {:<19} {:<9} {:<9} {:>6} {:>6}  WORKFLOW\n",
        "RUN", "STARTED", "STATE", "EXECUTOR", "TASKS", "FAILED"
    );
    for run in runs {
        let tasks: u64 = run.tasks.values().sum();
        let failed = run.tasks.get("failed").copied().unwrap_or_default();
        let executor = if run.simulated {
            "simulated"
        } else {
            run.executor.as_str()
        };
        let replay = match (&run.replays, run.replays_task) {
            (Some(earlier), Some(task)) => format!(" (replays task {task} of run {earlier})"),
            _ => String::new(),
        };
        let _ = writeln!(
            text,
            "{:<20} {:<19} {:<9} {:<9} {:>6} {:>6}  {}{replay}",
            run.id,
            local_time(run.started),
            run.state,
            executor,
            tasks,
            failed,
            run.workflow
        );
    }
    text
}

/// A time of the database in the local time zone, to the second.
pub fn local_time(milliseconds: i64) -> String {
    jiff::Timestamp::from_millisecond(milliseconds)
        .map(|time| {
            time.to_zoned(jiff::tz::TimeZone::system())
                .strftime("%Y-%m-%d %H:%M:%S")
                .to_string()
        })
        .unwrap_or_default()
}

/// `iunctus export <run>`: writes the RO-Crate of the run to `-output`, by default to
/// `<run>-crate` in the current directory.
pub fn export(arguments: &Arguments) -> Result<(), Failure> {
    let Some(id) = arguments.target.as_ref().map(|id| id.to_string_lossy()) else {
        return Err(Failure::usage(
            "iunctus: name the run to export; `iunctus runs` lists them",
        ));
    };
    let database = open(arguments)?;
    let Some(run) = database
        .run(&id)
        .map_err(|error| failure(error.to_string()))?
    else {
        return Err(failure(format!(
            "iunctus: the run database has no run `{id}`; `iunctus runs` lists the runs"
        )));
    };
    let workflow = main_file(&run, &database)?;
    let output = (arguments.output.clone()).unwrap_or_else(|| PathBuf::from(format!("{id}-crate")));
    let options = run_crate::Options {
        workflow,
        license: arguments.license.clone(),
        sources: kept_sources(arguments, &database, &run)?,
    };
    let summary = run_crate::export(&database, &id, &output, &options)
        .map_err(|error| failure(error.to_string()))?;
    let kind = if run.simulated {
        "simulated run"
    } else {
        "run"
    };
    println!(
        "wrote the RO-Crate of the {kind} {id} to {}: {} tasks, {} files ({} bytes)",
        output.display(),
        summary.tasks,
        summary.files,
        summary.bytes
    );
    Ok(())
}

/// Copies of the files of the workflow and of the command line of a run, by digest, as the
/// store of a real run keeps them; none for a simulated run, whose crate takes the files as they
/// are now when they did not change.
fn kept_sources(
    arguments: &Arguments,
    database: &Database,
    run: &Run,
) -> Result<std::collections::HashMap<String, PathBuf>, Failure> {
    let setup = crate::setup::Setup::load(arguments)?;
    let Some(store) = crate::repeat::kept_by(&setup, run)? else {
        return Ok(Default::default());
    };
    let sources = database.sources(&run.id).map_err(crate::changes::broken)?;
    let folder = cache_directory().join("exports").join(&run.id);
    let runtime = crate::changes::runtime()?;
    let mut kept = std::collections::HashMap::new();
    let mut digests: std::collections::BTreeSet<String> =
        sources.into_iter().map(|source| source.digest).collect();
    for task in database.tasks(&run.id).map_err(crate::changes::broken)? {
        if let Some(task) = database
            .task(&run.id, task.id)
            .map_err(crate::changes::broken)?
        {
            digests.extend(
                task.inputs
                    .into_iter()
                    .chain(task.outputs)
                    .filter_map(|file| file.digest),
            );
        }
    }
    for digest in digests {
        if let Some(copy) = runtime.block_on(crate::repeat::kept_copy(&store, &digest, &folder)) {
            kept.insert(digest, copy);
        }
    }
    Ok(kept)
}

/// `iunctus why <run> <task>`: what changed since the same task ran before.
pub fn why(arguments: &Arguments) -> Result<(), Failure> {
    let run = arguments.target.as_ref().map(|run| run.to_string_lossy());
    let task = arguments
        .targets
        .first()
        .and_then(|task| task.parse::<u64>().ok());
    let (Some(run), Some(task)) = (run, task) else {
        return Err(Failure::usage(
            "iunctus: name a run and the number of one of its tasks, like `iunctus why \
             1a0f1cc4c10-3a2487 4`; `iunctus runs` lists the runs",
        ));
    };
    let database = open(arguments)?;
    let broken = |error: run_database::DatabaseError| failure(error.to_string());
    let Some(row) = database.task_row(&run, task).map_err(broken)? else {
        return Err(failure(format!("iunctus: run {run} has no task {task}")));
    };
    match row.reused.as_deref() {
        Some("cache") => {
            println!("task {task} did not run: the cache of the store had its result");
            return Ok(());
        }
        Some(earlier) => {
            println!("task {task} did not run: its result came from run {earlier}");
            return Ok(());
        }
        None => {}
    }
    let why = database.why(&run, task).map_err(broken)?;
    match why {
        None => println!("no earlier run of the workflow has this task"),
        Some(why) => {
            println!("compared with task {} of run {}:", why.task, why.run);
            for change in why.changes {
                println!("- {change}");
            }
        }
    }
    Ok(())
}

/// `iunctus origin <file>`: the tasks that made a file, back to the data that no task made. The
/// file is a path, or a digest like `sha256:<hex>`.
pub fn origin(arguments: &Arguments) -> Result<(), Failure> {
    let Some(file) = &arguments.target else {
        return Err(Failure::usage(
            "iunctus: name a file that a run made, or its digest as sha256:<hex>",
        ));
    };
    let text = file.to_string_lossy();
    let file = if text.starts_with("sha256:") {
        text.into_owned()
    } else {
        let absolute = std::path::absolute(file).unwrap_or_else(|_| file.clone());
        absolute.to_string_lossy().into_owned()
    };
    let chain = open(arguments)?
        .origin(&file)
        .map_err(|error| failure(error.to_string()))?;
    if chain.is_empty() {
        println!("no run made {file}");
        return Ok(());
    }
    println!("{file} was made by:");
    for step in chain {
        let indent = "  ".repeat(step.depth);
        let image = step.image.map(|image| format!(", image {image}"));
        println!(
            "{indent}task {} {} of run {}{}",
            step.task,
            step.name,
            step.run,
            image.unwrap_or_default()
        );
        for data in step.data {
            let source = match data.location.as_str() {
                "" => "the workflow's own text".to_owned(),
                location => location.to_owned(),
            };
            let size = data.size.map(|size| format!(", {size} bytes"));
            println!(
                "{indent}  reads {} from {source}{}, which no task made",
                data.path,
                size.unwrap_or_default()
            );
        }
    }
    Ok(())
}

/// The main file of the workflow of a run, which the crate holds a copy of.
fn main_file(run: &Run, database: &Database) -> Result<PathBuf, Failure> {
    let target = Path::new(&run.workflow);
    let language = match run.language.as_str() {
        "nextflow" => Language::Nextflow,
        "snakemake" => Language::Snakemake,
        "cwl" => Language::Cwl,
        _ => Language::Argo,
    };
    let paths: std::collections::BTreeSet<PathBuf> = match run.workflow_folder.as_deref() {
        Some(folder) => database
            .sources(&run.id)
            .map_err(crate::changes::broken)?
            .into_iter()
            .filter(|source| source.kind == run_database::SourceKind::Workflow)
            .map(|source| Path::new(folder).join(source.path))
            .collect(),
        None => Default::default(),
    };
    if paths.contains(target) || target.is_file() {
        return Ok(target.to_owned());
    }
    let names: &[&str] = match language {
        Language::Nextflow => &["main.nf"],
        Language::Snakemake => &[
            "Snakefile",
            "snakefile",
            "workflow/Snakefile",
            "workflow/snakefile",
        ],
        _ => &[],
    };
    if let Some(main) = names
        .iter()
        .map(|name| target.join(name))
        .find(|path| paths.contains(path) || path.is_file())
    {
        return Ok(main);
    }
    if let Some(live) = detect::main_file(target, language) {
        return Ok(live);
    }
    Err(failure(format!(
        "iunctus: the workflow {} of the run is gone and no recorded main file is identifiable, so the crate cannot hold it",
        target.display()
    )))
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeMap;

    use super::*;

    #[test]
    fn untracked_preferred_main_is_not_replaced_by_a_recorded_module() {
        let root = tempfile::tempdir().expect("a directory");
        let project = root.path().join("workflow");
        std::fs::create_dir_all(&project).expect("creates workflow");
        for args in [["init", "-q"].as_slice(), ["add", "a.nf"].as_slice()] {
            if args[0] == "add" {
                std::fs::write(project.join("a.nf"), "process MODULE {}\n")
                    .expect("writes tracked module");
                std::fs::write(project.join("main.nf"), "workflow {}\n")
                    .expect("writes untracked main");
            }
            assert!(
                std::process::Command::new("git")
                    .args(args)
                    .current_dir(&project)
                    .env("GIT_CONFIG_GLOBAL", "/dev/null")
                    .env("GIT_CONFIG_NOSYSTEM", "1")
                    .status()
                    .expect("prepares test index")
                    .success()
            );
        }
        assert_eq!(
            crate::tracked::tracked(&project.join(".git")).expect("reads index"),
            ["a.nf"]
        );
        let sources = [run_database::Source {
            kind: run_database::SourceKind::Workflow,
            path: "a.nf".into(),
            digest: task_model::Digest::sha256(b"process MODULE {}\n").to_string(),
            size: b"process MODULE {}\n".len() as u64,
        }];
        let path = root.path().join("runs.sqlite");
        let start = run_database::RunStart {
            id: "entrypoint".into(),
            resumes: None,
            workflow: project.to_string_lossy().into_owned(),
            language: Language::Nextflow,
            revision: None,
            arguments: Vec::new(),
            parameters: Vec::new(),
            executor: "simulator".into(),
            simulated: true,
            store: None,
            program: None,
            launch: None,
            workflow_folder: Some(project.to_string_lossy().into_owned()),
            repeats: None,
        };
        let mut recorder = Database::open(&path)
            .expect("opens")
            .record(&start)
            .expect("records");
        recorder.sources(&sources);
        recorder.ended(None);
        recorder.close().expect("closes");
        let database = Database::open(&path).expect("opens again");
        let run = database.run("entrypoint").expect("reads").expect("a run");
        assert_eq!(
            main_file(&run, &database)
                .map_err(|error| error.message)
                .expect("finds live preferred main"),
            project.join("main.nf")
        );
        let recorded_main = run_database::Source {
            kind: run_database::SourceKind::Workflow,
            path: "main.nf".into(),
            digest: task_model::Digest::sha256(b"workflow {}\n").to_string(),
            size: b"workflow {}\n".len() as u64,
        };
        let mut historical = start.clone();
        historical.id = "retained-main".into();
        let mut recorder = Database::open(&path)
            .expect("opens")
            .record(&historical)
            .expect("records retained main");
        recorder.sources(&[sources[0].clone(), recorded_main.clone()]);
        recorder.ended(None);
        recorder.close().expect("closes retained main");
        std::fs::remove_file(project.join("main.nf")).expect("deletes main, retains helper");
        let retained = root.path().join("retained-main.nf");
        std::fs::write(&retained, "workflow {}\n").expect("retains main bytes");
        let historical = database
            .run("retained-main")
            .expect("reads retained main")
            .expect("a run");
        let selected = main_file(&historical, &database)
            .map_err(|error| error.message)
            .expect("selects missing recorded main");
        assert_eq!(selected, project.join("main.nf"));
        let options = run_crate::Options {
            workflow: selected,
            sources: [(recorded_main.digest, retained)].into(),
            ..run_crate::Options::default()
        };
        let output = root.path().join("crate");
        run_crate::export(&database, "retained-main", &output, &options)
            .expect("exports retained main instead of live helper");
        assert_eq!(
            std::fs::read_to_string(output.join("main.nf")).expect("reads archive main"),
            "workflow {}\n"
        );
        std::fs::remove_dir_all(&project).expect("deletes checkout");
        assert!(
            main_file(&run, &database).is_err(),
            "a recorded module does not identify the deleted main"
        );
        let mut explicit = run.clone();
        explicit.workflow = project.join("a.nf").to_string_lossy().into_owned();
        assert_eq!(
            main_file(&explicit, &database)
                .map_err(|error| error.message)
                .expect("identifies recorded explicit file"),
            project.join("a.nf")
        );
    }

    #[test]
    fn runs_are_listed_one_per_line() {
        let run = Run {
            id: "19a8f3c2b10-1a2b".into(),
            lineage: "19a8f3c2b10-1a2b".into(),
            resumes: None,
            workflow: "/work/main.nf".into(),
            language: "nextflow".into(),
            revision: None,
            arguments: Vec::new(),
            parameters: Vec::new(),
            executor: "simulator".into(),
            simulated: true,
            store: None,
            process: 1,
            started: 0,
            ended: None,
            state: "succeeded".into(),
            error: None,
            replays: None,
            replays_task: None,
            tasks: BTreeMap::from([("failed".into(), 1), ("succeeded".into(), 4)]),
            program: None,
            launch: None,
            workflow_folder: None,
            repeats: None,
        };
        let text = table(&[run]);
        let lines: Vec<&str> = text.lines().collect();
        assert_eq!(lines.len(), 2);
        assert!(lines[0].starts_with("RUN "), "{text}");
        let words: Vec<&str> = lines[1].split_whitespace().collect();
        assert_eq!(words[0], "19a8f3c2b10-1a2b");
        assert_eq!(
            &words[3..],
            ["succeeded", "simulated", "5", "1", "/work/main.nf"]
        );
    }
}
