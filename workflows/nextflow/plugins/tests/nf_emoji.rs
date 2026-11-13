//! nf-emoji against the plugin 0.2.0 with Nextflow 26.04.6: a real run of the probe prints the
//! recorded lines of `printEmoji`, `emojiView` and `emojiDump`.
// Copyright (c) 2026 The uwe Contributors
// SPDX-License-Identifier: MIT or Apache-2.0

use std::cell::RefCell;
use std::path::{Path, PathBuf};
use std::rc::Rc;

use nextflow_evaluator::interpreter::Host;
use nextflow_runtime::driver::{Nextflow, Settings};
use workflow_graph::{Attempt, Engine, Executor, Launch, Outcome, Running};

/// Keeps what the run prints.
struct Printed(Rc<RefCell<String>>);

impl Host for Printed {
    fn print(&mut self, text: &str) {
        self.0.borrow_mut().push_str(text);
    }
}

fn fixtures() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/nf_emoji")
}

/// Prints what `echo hi <x>` of the task's script prints.
struct Echo;

impl Executor for Echo {
    fn run(&mut self, attempt: Attempt) -> Running {
        let script = attempt.task.script.clone().unwrap_or_default();
        let said = script
            .lines()
            .find_map(|line| line.trim().strip_prefix("echo "));
        Running::ended(Ok(Outcome {
            id: attempt.task.id,
            stdout: format!("{}\n", said.unwrap_or_default()),
            ..Outcome::default()
        }))
    }
}

#[test]
fn prints_recorded_lines() {
    let launch = tempfile::tempdir().expect("a folder");
    let folder = launch.path();
    std::fs::copy(fixtures().join("main.nf"), folder.join("main.nf")).expect("copies");
    let config = "plugins { id 'nf-emoji@0.2.0' }\nemoji { theme = 'ocean'; progressBar = false; \
                  summary = false }\n";
    std::fs::write(folder.join("nextflow.config"), config).expect("writes");
    let printed = Rc::new(RefCell::new(String::new()));
    let settings = Settings {
        host: Box::new(Printed(printed.clone())),
        ..Settings::default()
    };
    let run = Launch {
        target: folder.to_owned(),
        scratch: folder.join("scratch"),
        ..Launch::default()
    };
    let mut driver = Nextflow::new(&run, settings).expect("a driver");
    let execution = Engine::new(&mut Echo).run("main.nf", &mut driver);
    assert!(execution.error.is_none(), "{:?}", execution.error);
    // Nextflow's own lines, like `[PROCESS ...]`, are not the plugin's.
    let recorded = std::fs::read_to_string(fixtures().join("stdout.txt")).expect("reads");
    let ours = |text: &str| {
        let mut lines: Vec<String> = (text.lines())
            .filter(|line| !line.starts_with("[PROCESS") && !line.starts_with("[PIPELINE"))
            .filter(|line| !line.starts_with("[WORKDIR") && !line.starts_with("[SUCCESS"))
            .filter(|line| !line.starts_with("WARN:") && !line.is_empty())
            .map(str::to_owned)
            .collect();
        lines.sort();
        lines
    };
    assert_eq!(ours(&printed.borrow()), ours(&recorded));
    assert!(printed.borrow().starts_with("🧪\n"), "{}", printed.borrow());
}
