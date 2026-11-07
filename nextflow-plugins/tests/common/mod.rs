//! What the report plugin tests share: a real uwe run of a pipeline folder, whose tasks run
//! their scripts with bash, each in a folder of its own.
// Copyright (c) 2026 The uwe Contributors
// SPDX-License-Identifier: MIT or Apache-2.0
#![allow(dead_code, reason = "each test file uses only a part of this module")]

use std::cell::RefCell;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::rc::Rc;
use std::sync::Arc;
use std::time::{Duration, SystemTime};

use nextflow_evaluator::interpreter::Host;
use nextflow_evaluator::interpreter::glob::matches;
use nextflow_runtime::driver::{Nextflow, Settings};
use workflow_graph::{
    Attempt, Engine, Execution, Executor, Files, Launch, LocalFiles, Outcome, OutputFile, Running,
    Task, Usage,
};

/// The fixture folder of a plugin.
pub fn fixtures(plugin: &str) -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("tests/fixtures")
        .join(plugin)
}

/// A run of the pipeline in `folder`, named `run_name`, with what it printed.
pub fn run(folder: &Path, run_name: &str) -> (Execution, String) {
    run_timed(folder, run_name, None)
}

/// A run whose tasks report the given run time and CPU time, so that figures derived from
/// them are fixed.
pub fn run_timed(folder: &Path, run_name: &str, timing: Option<Timing>) -> (Execution, String) {
    let launch = Launch {
        target: folder.to_owned(),
        scratch: folder.join("scratch"),
        ..Launch::default()
    };
    let printed = Rc::new(RefCell::new(String::new()));
    let settings = Settings {
        host: Box::new(Printed(printed.clone())),
        run_name: run_name.to_owned(),
        command_line: "uwe run main.nf".to_owned(),
        ..Settings::default()
    };
    let mut driver = Nextflow::new(&launch, settings).expect("a driver");
    let mut shell = Shell {
        root: folder.join("work"),
        timing,
    };
    let execution = Engine::new(&mut shell).run("main.nf", &mut driver);
    let printed = printed.borrow().clone();
    (execution, printed)
}

/// Keeps what the run prints.
struct Printed(Rc<RefCell<String>>);

impl Host for Printed {
    fn print(&mut self, text: &str) {
        self.0.borrow_mut().push_str(text);
    }
}

/// Runs the script of each attempt with bash in `<root>/<task>/<attempt>`, with its inputs
/// linked there, and gives the files that match its output patterns.
struct Shell {
    root: PathBuf,
    timing: Option<Timing>,
}

/// The run time and CPU time that every task of a run reports.
#[derive(Clone, Copy, Debug)]
pub struct Timing {
    pub realtime: Duration,
    pub cpu: Duration,
}

impl Executor for Shell {
    fn run(&mut self, attempt: Attempt) -> Running {
        Running::ended(Ok(self.execute(&attempt.task, attempt.number)))
    }

    fn files(&self) -> Option<Arc<dyn Files>> {
        Some(Arc::new(LocalFiles))
    }
}

impl Shell {
    fn execute(&self, task: &Task, attempt: u32) -> Outcome {
        let folder = self.root.join(format!("{}/{attempt}", task.id.0));
        std::fs::create_dir_all(&folder).expect("makes the task folder");
        for input in &task.inputs {
            let target = folder.join(&input.path);
            if let Some(parent) = target.parent() {
                std::fs::create_dir_all(parent).expect("makes the input folder");
            }
            match &input.content {
                Some(content) => std::fs::write(&target, content).expect("writes the input"),
                None => {
                    std::os::unix::fs::symlink(&input.source, &target).expect("links the input")
                }
            }
        }
        let script = format!(
            "#!/bin/bash -ue\n{}\n",
            task.script.as_deref().unwrap_or_default()
        );
        std::fs::write(folder.join(".command.sh"), script).expect("writes the script");
        let started = SystemTime::now();
        let output = Command::new("bash")
            .args(["-ue", ".command.sh"])
            .current_dir(&folder)
            .output()
            .expect("runs bash");
        let mut ended = SystemTime::now();
        let mut usage = None;
        if let Some(timing) = self.timing {
            ended = started + timing.realtime;
            usage = Some(Usage {
                cpu_usec: u64::try_from(timing.cpu.as_micros()).ok(),
                ..Usage::default()
            });
        }
        let mut files = Vec::new();
        let mut names: Vec<String> = std::fs::read_dir(&folder)
            .expect("lists the task folder")
            .filter_map(|entry| Some(entry.ok()?.file_name().to_string_lossy().into_owned()))
            .filter(|name| !name.starts_with(".command"))
            .collect();
        names.sort();
        for name in names {
            let staged = task.inputs.iter().any(|input| input.path == name);
            if !staged && task.outputs.iter().any(|pattern| matches(pattern, &name)) {
                let path = folder.join(&name);
                files.push(OutputFile {
                    path: name,
                    location: path.to_string_lossy().into_owned(),
                    directory: path.is_dir(),
                    size: std::fs::metadata(&path).ok().map(|metadata| metadata.len()),
                    digest: None,
                });
            }
        }
        Outcome {
            id: task.id,
            exit_code: output.status.code().unwrap_or(-1),
            files,
            stdout: String::from_utf8_lossy(&output.stdout).into_owned(),
            stderr: String::from_utf8_lossy(&output.stderr).into_owned(),
            started: Some(started),
            ended: Some(ended),
            usage,
            ..Outcome::default()
        }
    }
}

/// Copies the files of a fixture folder into `folder`.
pub fn copy(fixture: &Path, folder: &Path, names: &[&str]) {
    for name in names {
        std::fs::copy(fixture.join(name), folder.join(name)).expect("copies the fixture");
    }
}
