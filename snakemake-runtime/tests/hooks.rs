//! Lifecycle hooks follow the Snakemake scheduling boundary and use native Python.
// Copyright (c) 2026 The uwe Contributors
// SPDX-License-Identifier: MIT or Apache-2.0

use workflow_graph::{
    Attempt, ExecuteError, Executor, Launch, Outcome, Running, Simulator, execute,
};

const HOOKS: &str = r#"
events = []
onstart:
    events.append('start')
    print('starting')
    logger.info('hook log')
    with open('events', 'a') as handle:
        handle.write('start\n')
    local_only = 1
onsuccess:
    assert events == ['start']
    assert 'local_only' not in globals()
    assert 'starting' in open(log).read()
    assert 'hook log' in open(log).read()
    with open('events', 'a') as handle:
        handle.write('success\n')
onerror:
    assert events == ['start']
    assert 'failed' in open(log).read() or 'exit code' in open(log).read()
    with open('events', 'a') as handle:
        handle.write('error\n')
rule make:
    shell: 'true'
"#;

struct Failing(bool);

impl Executor for Failing {
    fn run(&mut self, attempt: Attempt) -> Running {
        Running::ended(if self.0 {
            Err(ExecuteError::Executor {
                task: attempt.task.name.clone(),
                message: "executor failed".into(),
            })
        } else {
            Ok(Outcome {
                id: attempt.task.id,
                exit_code: 1,
                ..Outcome::default()
            })
        })
    }
}

#[test]
fn awaits_success_hooks() {
    let folder = tempfile::tempdir().expect("a folder");
    std::fs::write(folder.path().join("Snakefile"), HOOKS).expect("a Snakefile");
    let launch = Launch {
        target: folder.path().to_path_buf(),
        scratch: folder.path().join("scratch"),
        ..Launch::default()
    };
    let mut driver = snakemake_runtime::driver(&launch).expect("a driver");
    driver.inputs().expect("inputs");
    driver.outputs().expect("outputs");
    assert!(!folder.path().join("events").exists());
    let mut executor = Simulator::new(folder.path().join("simulator"));
    let execution = execute("hooks", driver.as_mut(), &mut executor);
    assert!(execution.error.is_none(), "{:?}", execution.error);
    assert_eq!(
        std::fs::read_to_string(folder.path().join("events")).unwrap(),
        "start\nsuccess\n"
    );
}

#[test]
fn registers_called_hooks() {
    let folder = tempfile::tempdir().expect("a folder");
    std::fs::write(
        folder.path().join("module.smk"),
        "onstart:\n    print('module start')\n    with open('events', 'a') as handle:\n        handle.write('module\\n')\nrule make:\n    shell: 'true'\n",
    ).expect("a module");
    std::fs::write(
        folder.path().join("hooks.smk"),
        "onstart:\n    test.onstart()\nonsuccess:\n    assert 'module start' in open(log).read()\n    with open('events', 'a') as handle:\n        handle.write('success\\n')\n",
    ).expect("included hooks");
    std::fs::write(
        folder.path().join("Snakefile"),
        "module test:\n    snakefile: 'module.smk'\nuse rule * from test\ninclude: 'hooks.smk'\n",
    )
    .expect("a Snakefile");
    let launch = Launch {
        target: folder.path().to_path_buf(),
        scratch: folder.path().join("scratch"),
        ..Launch::default()
    };
    let mut driver = snakemake_runtime::driver(&launch).expect("a driver");
    let mut executor = Simulator::new(folder.path().join("simulator"));
    let execution = execute("hooks", driver.as_mut(), &mut executor);
    assert!(execution.error.is_none(), "{:?}", execution.error);
    assert_eq!(
        std::fs::read_to_string(folder.path().join("events")).unwrap(),
        "module\nsuccess\n"
    );
}

#[test]
fn selects_error_hooks() {
    for machine in [false, true] {
        let folder = tempfile::tempdir().expect("a folder");
        std::fs::write(folder.path().join("Snakefile"), HOOKS).expect("a Snakefile");
        let launch = Launch {
            target: folder.path().to_path_buf(),
            scratch: folder.path().join("scratch"),
            ..Launch::default()
        };
        let mut driver = snakemake_runtime::driver(&launch).expect("a driver");
        let execution = execute("hooks", driver.as_mut(), &mut Failing(machine));
        assert!(execution.error.is_some());
        assert!(
            execution.failure_hook_error.is_none(),
            "{:?}",
            execution.failure_hook_error
        );
        assert_eq!(
            std::fs::read_to_string(folder.path().join("events")).unwrap(),
            "start\nerror\n"
        );
    }
}

#[test]
fn stops_failed_worker() {
    let folder = tempfile::tempdir().expect("a folder");
    std::fs::write(
        folder.path().join("Snakefile"),
        "onerror:\n    assert False\nshell('true')\nrule make:\n    shell: 'true'\n",
    )
    .expect("a Snakefile");
    let launch = Launch {
        target: folder.path().to_path_buf(),
        scratch: folder.path().join("scratch"),
        ..Launch::default()
    };
    let mut driver = snakemake_runtime::driver(&launch).expect("a driver");
    let execution = execute("hooks", driver.as_mut(), &mut Failing(true));
    assert!(
        execution
            .error
            .unwrap()
            .to_string()
            .contains("executor failed")
    );
    assert!(execution.failure_hook_error.is_none());
    assert_eq!(execution.graph.nodes.len(), 1);
}

#[test]
fn skips_unexecuted_hooks() {
    for input in ["", "    input: 'missing'\n"] {
        let folder = tempfile::tempdir().expect("a folder");
        std::fs::write(folder.path().join("out"), "existing").expect("an output");
        std::fs::write(
            folder.path().join("Snakefile"),
            format!("onstart:\n    assert False\nonsuccess:\n    assert False\nonerror:\n    assert False\nrule make:\n{input}    output: 'out'\n    shell: 'true'\n"),
        ).expect("a Snakefile");
        let launch = Launch {
            target: folder.path().to_path_buf(),
            scratch: folder.path().join("scratch"),
            ..Launch::default()
        };
        let mut driver = snakemake_runtime::driver(&launch).expect("a driver");
        let mut executor = Simulator::new(folder.path().join("simulator"));
        let execution = execute("hooks", driver.as_mut(), &mut executor);
        assert_eq!(execution.error.is_some(), !input.is_empty());
        assert!(execution.failure_hook_error.is_none());
        assert!(execution.graph.nodes.is_empty());
    }
}

#[test]
fn runs_hook_commands() {
    for hook in ["onstart", "onsuccess", "onerror"] {
        let folder = tempfile::tempdir().expect("a folder");
        std::fs::write(
            folder.path().join("Snakefile"),
            format!(
                "{hook}:\n    shell('mail user@example.test')\nrule make:\n    shell: 'true'\n"
            ),
        )
        .expect("a Snakefile");
        let launch = Launch {
            target: folder.path().to_path_buf(),
            scratch: folder.path().join("scratch"),
            simulated: true,
            ..Launch::default()
        };
        let mut driver = snakemake_runtime::driver(&launch).expect("a driver");
        let mut executor = Simulator::new(folder.path().join("simulator"));
        let execution = if hook == "onerror" {
            execute("hooks", driver.as_mut(), &mut Failing(false))
        } else {
            execute("hooks", driver.as_mut(), &mut executor)
        };
        if hook == "onerror" {
            assert!(execution.error.is_some());
            assert!(
                execution.failure_hook_error.is_none(),
                "{:?}",
                execution.failure_hook_error
            );
        } else {
            assert!(execution.error.is_none(), "{:?}", execution.error);
        }
    }
}

#[test]
fn diagnoses_missing_engine() {
    for shell in [false, true] {
        let folder = tempfile::tempdir().unwrap();
        let hook = if shell {
            "shell('true')"
        } else {
            "print('native hook')"
        };
        std::fs::write(
            folder.path().join("Snakefile"),
            format!("onstart:\n    {hook}\nrule make:\n    shell: 'true'\n"),
        )
        .unwrap();
        let launch = Launch {
            target: folder.path().into(),
            scratch: folder.path().join("scratch"),
            ..Launch::default()
        };
        let settings = snakemake_runtime::Settings {
            python: Some(snakemake_runtime::PythonContainers {
                engine: folder
                    .path()
                    .join("missing.sock")
                    .to_string_lossy()
                    .into_owned(),
                image: None,
                cache: folder.path().join("cache"),
                show_output: false,
            }),
            ..Default::default()
        };
        let mut driver = snakemake_runtime::driver_with(&launch, settings).unwrap();
        let mut executor = Simulator::new(folder.path().join("made"));
        let execution = execute("hooks", driver.as_mut(), &mut executor);
        if shell {
            let error = execution.error.unwrap();
            assert!(
                error
                    .to_string()
                    .contains("requires a local Docker or Podman engine"),
                "{error}"
            );
        } else {
            assert!(execution.error.is_none(), "{:?}", execution.error);
        }
    }
}
