//! Checks structured description, admission and execution through native drivers.
// Copyright (c) 2026 The uwe Contributors
// SPDX-License-Identifier: MIT or Apache-2.0

use std::cell::Cell;
use std::rc::Rc;
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};

use input_schema::Severity;
use serde_json::json;
use uwe::application::{self, ApplicationError, Description, Policy};
use workflow_graph::{
    Attempt, CancellationToken, Driver, DriverError, Executor, Inputs, Language, Outcome,
    RunOptions, Running, Simulator, Task,
};

struct FakeDriver {
    starts: Rc<Cell<usize>>,
    inputs: Option<Inputs>,
}

impl Driver for FakeDriver {
    fn language(&self) -> Language {
        Language::Cwl
    }

    fn start(&mut self) -> Result<Vec<Task>, DriverError> {
        self.starts.set(self.starts.get() + 1);
        Ok(Vec::new())
    }

    fn complete(&mut self, _outcome: Outcome) -> Result<Vec<Task>, DriverError> {
        Ok(Vec::new())
    }

    fn finish(&mut self) -> Result<(), DriverError> {
        Ok(())
    }

    fn inputs(&mut self) -> Result<Option<Inputs>, DriverError> {
        Ok(self.inputs.clone())
    }
}

struct CountingExecutor(Arc<AtomicUsize>);

impl Executor for CountingExecutor {
    fn run(&mut self, attempt: Attempt) -> Running {
        self.0.fetch_add(1, Ordering::SeqCst);
        Running::ended(Ok(Outcome {
            id: attempt.task.id,
            ..Outcome::default()
        }))
    }
}

fn inputs(values: serde_json::Value) -> Inputs {
    Inputs {
        schema: json!({
            "type": "object",
            "required": ["count"],
            "properties": {"count": {"type": "integer"}}
        }),
        values,
        ..Inputs::default()
    }
}

#[test]
fn admission_starts_nothing() {
    let starts = Rc::new(Cell::new(0));
    let mut driver = FakeDriver {
        starts: starts.clone(),
        inputs: Some(inputs(json!({"count": "3"}))),
    };
    let description = application::describe(&mut driver).expect("describes inputs");
    assert_eq!(starts.get(), 0);
    assert!(description.outputs.is_none());
    let admission =
        application::validate(description, false, Policy::Relaxed).expect("validates inputs");
    assert_eq!(starts.get(), 0);
    assert_eq!(admission.inputs.expect("inputs").values["count"], json!(3));

    let missing = Description {
        language: Language::Cwl,
        inputs: None,
        outputs: None,
    };
    assert!(matches!(
        application::validate(missing, false, Policy::Strict),
        Err(ApplicationError::MissingSchema { .. })
    ));
    let mut advisory = inputs(json!({"count": 3}));
    advisory.advisory = true;
    assert!(matches!(
        application::validate(
            Description {
                language: Language::Cwl,
                inputs: Some(advisory),
                outputs: None,
            },
            false,
            Policy::Strict,
        ),
        Err(ApplicationError::AdvisorySchema { .. })
    ));
}

#[test]
fn strict_admission_validates() {
    let mut required = inputs(json!({}));
    required.assumes_values = true;
    let folder = tempfile::tempdir().expect("a temporary directory");
    let local = Inputs {
        schema: json!({
            "type": "object",
            "properties": {
                "file": {"type": "string", "format": "file-path", "exists": true}
            }
        }),
        values: json!({"file": "missing.txt"}),
        base: folder.path().to_owned(),
        ..Inputs::default()
    };

    for (inputs, code) in [(required, "schema.required"), (local, "schema.exists")] {
        let description = Description {
            language: Language::Cwl,
            inputs: Some(inputs),
            outputs: None,
        };
        assert!(
            application::validate(description.clone(), true, Policy::Relaxed).is_ok(),
            "relaxed simulation keeps {code} as a warning"
        );
        let Err(ApplicationError::InvalidInputs { admission, .. }) =
            application::validate(description, true, Policy::Strict)
        else {
            panic!("strict admission must reject {code}")
        };
        let diagnostic = &admission.validation.expect("validation").diagnostics[0];
        assert_eq!(diagnostic.code, code);
        assert_eq!(diagnostic.severity, Severity::Error);
    }
}

#[test]
fn rejects_before_execution() {
    let starts = Rc::new(Cell::new(0));
    let mut driver = FakeDriver {
        starts: starts.clone(),
        inputs: Some(inputs(json!({"count": "many"}))),
    };
    let calls = Arc::new(AtomicUsize::new(0));
    let mut executor = CountingExecutor(calls.clone());
    let result = application::execute(
        "invalid",
        &mut driver,
        &mut executor,
        RunOptions::default(),
        CancellationToken::new(),
        false,
        Policy::Relaxed,
    );

    assert!(matches!(
        result,
        Err(ApplicationError::InvalidInputs { .. })
    ));
    assert_eq!(starts.get(), 0);
    assert_eq!(calls.load(Ordering::SeqCst), 0);
}

/// The check reads `--count 3` as the integer of the schema, but the script gets the text, as
/// in Nextflow 26.04.6, where `params.count + 1` is '31'.
#[test]
fn keeps_launch_texts() {
    let folder = tempfile::tempdir().expect("a temporary directory");
    let pipeline = r#"
process COUNT {
    output:
    path 'count.txt'
    script:
    "echo ${params.count + 1} > count.txt"
}
workflow { COUNT() }
"#;
    std::fs::write(folder.path().join("main.nf"), pipeline).expect("writes the pipeline");
    std::fs::write(
        folder.path().join("nextflow_schema.json"),
        r#"{"type":"object","properties":{"count":{"type":"integer"}}}"#,
    )
    .expect("writes the schema");
    let launch = workflow_graph::Launch {
        target: folder.path().to_owned(),
        params: vec![("count".into(), "3".into())],
        simulated: true,
        ..workflow_graph::Launch::default()
    };
    let mut driver = nextflow_runtime::driver(&launch).expect("a driver");
    let mut executor = Simulator::new(folder.path().join("simulator"));
    let executed = application::execute(
        "main.nf",
        driver.as_mut(),
        &mut executor,
        RunOptions::default(),
        CancellationToken::new(),
        true,
        Policy::Relaxed,
    )
    .expect("executes");

    assert_eq!(
        executed.admission.inputs.expect("inputs").values["count"],
        json!(3)
    );
    let script = executed.execution.graph.nodes[0]
        .task
        .script
        .as_deref()
        .unwrap_or_default();
    assert!(script.contains("echo 31 > count.txt"), "{script}");
}
