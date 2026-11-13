//! nf-labrobotics in a real uwe run: the environment of the `labrobotics` executor's tasks, as
//! the plugin's executor sets it, and `labPlate`.
// Copyright (c) 2026 The uwe Contributors
// SPDX-License-Identifier: MIT or Apache-2.0

use std::cell::RefCell;
use std::collections::BTreeMap;
use std::rc::Rc;

use nextflow_evaluator::interpreter::Host;
use nextflow_runtime::driver::{Nextflow, Settings};
use workflow_graph::{Attempt, Engine, Executor, Launch, Outcome, Running};

/// The environment of each task, by its name.
type Recorded = Rc<RefCell<Vec<(String, BTreeMap<String, String>)>>>;

/// Keeps the environment of each task and succeeds without running it.
struct Recorder(Recorded);

impl Executor for Recorder {
    fn run(&mut self, attempt: Attempt) -> Running {
        let task = &attempt.task;
        self.0
            .borrow_mut()
            .push((task.name.clone(), task.environment.clone()));
        Running::ended(Ok(Outcome {
            id: task.id,
            exit_code: 0,
            ..Outcome::default()
        }))
    }
}

struct Printed(Rc<RefCell<String>>);

impl Host for Printed {
    fn print(&mut self, text: &str) {
        self.0.borrow_mut().push_str(text);
    }
}

#[test]
fn sets_task_environment() {
    let directory = tempfile::tempdir().expect("a temporary directory");
    let folder = directory.path();
    std::fs::write(
        folder.join("provenance.json"),
        r#"{"plate": "P1", "wells": ["A1"]}"#,
    )
    .expect("writes");
    std::fs::write(
        folder.join("result.json"),
        r#"{"wells": ["A2"], "ok": true}"#,
    )
    .expect("writes");
    let script = format!(
        "include {{ labPlate }} from 'plugin/nf-labrobotics'\n\
         process WET {{\n  label 'process_wet'\n  hints 'labrobotics/capability': 'liquid-handling'\n  \
         queue 'bench1'\n  script: 'labctl run'\n}}\nprocess DRY {{\n  script: 'true'\n}}\n\
         workflow {{\n  println labPlate(file('{0}/provenance.json'), file('{0}/result.json'))\n  \
         WET()\n  DRY()\n}}\n",
        folder.display()
    );
    std::fs::write(folder.join("main.nf"), script).expect("writes the script");
    let config = "plugins { id 'nf-labrobotics@0.1.0' }\n\
                  labrobotics { adapter = 'http://127.0.0.1:8000'; poll = '1500ms'; grace = '5m' }\n\
                  process { withLabel: 'process_wet' { executor = 'labrobotics' } }\n";
    std::fs::write(folder.join("nextflow.config"), config).expect("writes the config");
    let launch = Launch {
        target: folder.to_owned(),
        scratch: folder.join("scratch"),
        ..Launch::default()
    };
    let printed = Rc::new(RefCell::new(String::new()));
    let settings = Settings {
        host: Box::new(Printed(printed.clone())),
        run_name: "wet-run".to_owned(),
        ..Settings::default()
    };
    let mut driver = Nextflow::new(&launch, settings).expect("a driver");
    let recorded = Rc::new(RefCell::new(Vec::new()));
    let mut executor = Recorder(recorded.clone());
    let execution = Engine::new(&mut executor).run("main.nf", &mut driver);
    let printed = printed.borrow().clone();
    assert!(
        execution.error.is_none(),
        "{:?}\n{printed}",
        execution.error
    );
    assert!(
        printed.contains("[plate:P1, wells:[A2], ok:true]"),
        "{printed}"
    );
    let recorded = recorded.borrow();
    let environment = |name: &str| {
        let found = recorded.iter().find(|(task, _)| task == name);
        found
            .map(|(_, environment)| environment.clone())
            .unwrap_or_default()
    };
    let wet = environment("WET");
    let variable = |name: &str| wet.get(name).map(String::as_str);
    assert_eq!(variable("NF_LAB_ADAPTER"), Some("http://127.0.0.1:8000"));
    assert_eq!(variable("NF_LAB_POLL"), Some("1.5"));
    assert_eq!(variable("NF_LAB_GRACE"), Some("300"));
    assert_eq!(variable("NF_LAB_CAPABILITY"), Some("liquid-handling"));
    assert_eq!(variable("NF_LAB_QUEUE"), Some("bench1"));
    assert_eq!(variable("NF_LAB_SESSION"), Some("wet-run"));
    let dry = environment("DRY");
    assert!(
        !dry.keys().any(|name| name.starts_with("NF_LAB_")),
        "{dry:?}"
    );
}
