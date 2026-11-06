//! The cleanup of nf-gc frees the run's own files once no task can read them, while later tasks
//! still read theirs; task results and resumed runs stay intact.
// Copyright (c) 2026 The uwe Contributors
// SPDX-License-Identifier: MIT or Apache-2.0

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::rc::Rc;
use std::sync::{Arc, Mutex};

use nextflow_evaluator::exception::Exception;
use nextflow_evaluator::interpreter::{Error, Host};
use nextflow_evaluator::value::PathValue;
use nextflow_runtime::driver::{Nextflow, Settings, run_folder};
use workflow_graph::{
    Attempt, Engine, Executor, Files, Launch, LocalFiles, Outcome, OutputFile, Running,
};

// EARLY reads `early.txt` and ends before SECOND starts, which reads `all.txt` after FIRST, the
// other reader of `all.txt`, ended.
const PIPELINE: &str = r#"
process EARLY {
    input:
    path f
    output:
    path 'early.out'
    script:
    "cat ${f} > early.out"
}

process FIRST {
    input:
    path f
    output:
    path 'first.out'
    script:
    "cat ${f} > first.out"
}

process SECOND {
    input:
    path f
    tuple path(g), path(h)
    output:
    path 'second.out'
    script:
    "cat ${f} ${g} ${h} > second.out"
}

workflow {
    early = channel.of('x').collectFile(name: 'early.txt')
    all = channel.of('a', 'b').collectFile(name: 'all.txt', newLine: true)
    SECOND(all, FIRST(all).combine(EARLY(early)))
}
"#;

/// Keeps operator files in the folder of the run, as uwe's own host does.
struct Folder(PathBuf);

impl Host for Folder {
    fn print(&mut self, _: &str) {}

    fn operator_directory(&mut self) -> Result<PathValue, Error> {
        let made = std::fs::create_dir_all(&self.0).and_then(|()| {
            tempfile::Builder::new()
                .prefix("operators-")
                .tempdir_in(&self.0)
        });
        let made = made.map_err(|error| Error::Thrown {
            exception: Rc::new(Exception::new("IOException", error.to_string())),
            span: Default::default(),
        })?;
        Ok(PathValue::parse(&made.keep().to_string_lossy(), "/"))
    }
}

/// What each task saw of its inputs when it ran: each name with whether its file was there,
/// and which files of the run's folder were there.
#[derive(Default)]
struct Seen {
    inputs: BTreeMap<String, Vec<(String, bool)>>,
    folder: BTreeMap<String, Vec<String>>,
}

/// Runs each task as a local executor would: reads its inputs and writes its output into a
/// folder of its own. On resume, it answers every task from those results instead.
struct Reading {
    tasks: PathBuf,
    run: PathBuf,
    seen: Arc<Mutex<Seen>>,
    resumed: bool,
}

impl Executor for Reading {
    fn run(&mut self, attempt: Attempt) -> Running {
        let task = &attempt.task;
        let output = format!("{}.out", task.step.to_lowercase());
        let location = self.tasks.join(&output);
        let mut seen = self.seen.lock().expect("not poisoned");
        // The task's script is an input with content, like `.command.sh`.
        let readable: Vec<_> = (task.inputs.iter())
            .filter(|input| input.content.is_none())
            .collect();
        let inputs = (readable.iter())
            .map(|input| (input.path.clone(), Path::new(&input.source).exists()))
            .collect();
        seen.inputs.insert(task.step.clone(), inputs);
        seen.folder.insert(task.step.clone(), files(&self.run));
        if !self.resumed {
            let text: String = (readable.iter())
                .map(|input| std::fs::read_to_string(&input.source).unwrap_or_default())
                .collect();
            std::fs::create_dir_all(&self.tasks).expect("makes the folder");
            std::fs::write(&location, text).expect("writes");
        }
        Running::ended(Ok(Outcome {
            id: task.id,
            cached: self.resumed,
            files: vec![OutputFile {
                path: output,
                location: location.to_string_lossy().into_owned(),
                ..OutputFile::default()
            }],
            ..Outcome::default()
        }))
    }

    fn files(&self) -> Option<Arc<dyn Files>> {
        Some(Arc::new(LocalFiles))
    }
}

/// The names of the files below a folder, sorted.
fn files(folder: &Path) -> Vec<String> {
    let mut names = Vec::new();
    for entry in std::fs::read_dir(folder).into_iter().flatten().flatten() {
        match entry.path().is_dir() {
            true => names.extend(files(&entry.path())),
            false => names.push(entry.file_name().to_string_lossy().into_owned()),
        }
    }
    names.sort();
    names
}

/// Runs the pipeline in `launch` with this config and returns what the tasks saw, with the
/// files of the run's folder at the end of the run, before the driver goes.
fn run(launch: &Path, config: &str, resumed: bool) -> (Seen, Vec<String>) {
    std::fs::write(launch.join("main.nf"), PIPELINE).expect("writes");
    std::fs::write(launch.join("nextflow.config"), config).expect("writes");
    let scratch = launch.join("scratch");
    let folder = run_folder(&scratch, "uwe");
    let settings = Settings {
        host: Box::new(Folder(folder.clone())),
        ..Settings::default()
    };
    let target = Launch {
        target: launch.to_owned(),
        scratch,
        ..Launch::default()
    };
    let mut driver = Nextflow::new(&target, settings).expect("a driver");
    let seen = Arc::new(Mutex::new(Seen::default()));
    let mut executor = Reading {
        tasks: launch.join("tasks"),
        run: folder.clone(),
        seen: seen.clone(),
        resumed,
    };
    let execution = Engine::new(&mut executor).run("main.nf", &mut driver);
    assert!(execution.error.is_none(), "{:?}", execution.error);
    let left = files(&folder);
    drop(executor);
    let seen = Arc::try_unwrap(seen).ok().expect("one owner");
    (seen.into_inner().expect("not poisoned"), left)
}

#[test]
fn frees_unread_files() {
    let launch = tempfile::tempdir().expect("a folder");
    let launch = launch.path();
    let (seen, left) = run(launch, "plugins { id 'nf-gc@0.3.0' }\n", false);
    let present = |step: &str| seen.inputs[step].iter().all(|(_, there)| *there);
    assert!(present("EARLY") && present("FIRST"), "{:?}", seen.inputs);
    assert!(present("SECOND"), "all.txt stays while SECOND reads it");
    assert_eq!(
        seen.folder["SECOND"],
        ["all.txt"],
        "early.txt went with EARLY"
    );
    assert!(left.is_empty(), "{left:?}");
    let second = std::fs::read_to_string(launch.join("tasks/second.out")).expect("reads");
    assert_eq!(second, "a\nb\na\nb\nx", "SECOND read every input");
    for output in ["early.out", "first.out", "second.out"] {
        assert!(
            launch.join("tasks").join(output).is_file(),
            "{output} stays"
        );
    }
    // A resumed run takes the results of the first run, which the cleanup leaves alone.
    let (seen, left) = run(launch, "plugins { id 'nf-gc@0.3.0' }\n", true);
    assert!(seen.inputs["SECOND"].iter().all(|(_, there)| *there));
    assert!(left.is_empty(), "{left:?}");
    for output in ["early.out", "first.out", "second.out"] {
        assert!(
            launch.join("tasks").join(output).is_file(),
            "{output} stays"
        );
    }
}

#[test]
fn keeps_files_otherwise() {
    let launch = tempfile::tempdir().expect("a folder");
    let (seen, left) = run(launch.path(), "", false);
    assert_eq!(seen.folder["SECOND"], ["all.txt", "early.txt"]);
    assert_eq!(left, ["all.txt", "early.txt"]);
}
