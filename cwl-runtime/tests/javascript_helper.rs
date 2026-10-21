//! Real helper isolation. Run with the packaged helper image and Docker or Podman.
// Copyright (c) 2026 The uwe Contributors
// SPDX-License-Identifier: MIT or Apache-2.0

use std::time::{Duration, Instant};

use cwl_parser::model::Expression;
use cwl_runtime::{Evaluator, Value, Variables};

fn evaluate(evaluator: &Evaluator, body: &str) -> Result<Value, String> {
    evaluator.evaluate(
        &Expression::parse(body, false).expect("expression"),
        Variables {
            inputs: &Value::Null,
            this: &Value::Null,
            runtime: &Value::Null,
        },
    )
}

#[test]
#[ignore = "requires the packaged iunctus-cwl-js image and bounded Docker or Podman"]
fn preserves_worker_fallback() {
    let evaluator = Evaluator::with_javascript(Vec::new());
    let value =
        evaluate(&evaluator, "${leaked = 1; return {b: 2, a: 1, n: 1e21};}").expect("value");
    let Value::Map(map) = value else {
        panic!("map")
    };
    assert_eq!(
        map.keys().map(String::as_str).collect::<Vec<_>>(),
        ["b", "a", "n"]
    );
    assert_eq!(map["n"], Value::Float(1e21));
    assert_eq!(
        evaluate(&evaluator, "${return typeof leaked;}"),
        Ok(Value::string("undefined"))
    );
    assert_eq!(
        evaluate(&evaluator, "${return undefined;}"),
        Ok(Value::Null)
    );
    // As in cwltool, `${...}` is code, and only code under the requirement.
    let error = evaluate(&Evaluator::default(), "${HOME}").expect_err("no requirement");
    assert!(error.contains("InlineJavascriptRequirement"), "{error}");
}

#[tokio::test]
#[ignore = "requires the packaged iunctus-cwl-js image and bounded Docker or Podman"]
async fn supports_async_callers() {
    tokio::spawn(async {
        let evaluator = Evaluator::with_javascript(Vec::new());
        assert_eq!(
            evaluate(&evaluator, "${return 42;}"),
            Ok(Value::Integer(42))
        );
        drop(evaluator);
    })
    .await
    .expect("helper operations and cleanup must not nest Tokio runtimes");
}

#[test]
#[ignore = "requires the packaged iunctus-cwl-js image and bounded Docker or Podman"]
fn bounds_native_work() {
    for body in [
        "${function f(n) {return n ? f(n-1) + f(n-1) : 1;} return f(55);}".to_owned(),
        "${return 'x'.repeat(300000000);}".to_owned(),
        format!(
            "${{{}return 1;}}",
            (0..900_000)
                .map(|index| format!("var v{index}=1;"))
                .collect::<String>()
        ),
    ] {
        let evaluator = Evaluator::with_javascript(Vec::new());
        assert_eq!(evaluate(&evaluator, "${return 1;}"), Ok(Value::Integer(1)));
        let started = Instant::now();
        let error = evaluate(&evaluator, &body).expect_err("bounded computation must fail");
        assert!(
            error.contains("CWL JavaScript execution failure:"),
            "{error}"
        );
        assert!(started.elapsed() < Duration::from_secs(35), "{error}");
    }
}

#[test]
#[ignore = "requires the packaged iunctus-cwl-js image and bounded Docker or Podman"]
fn rejects_unbounded_results() {
    for body in [
        "${return 'x'.repeat(20 << 20);}",
        "${var x=null; for(var i=0;i<80;i++) x=[x]; return x;}",
    ] {
        let evaluator = Evaluator::with_javascript(Vec::new());
        assert_eq!(evaluate(&evaluator, "${return 1;}"), Ok(Value::Integer(1)));
        let error = evaluate(&evaluator, body).expect_err("output limit");
        assert!(
            error.contains("CWL JavaScript execution failure:"),
            "{error}"
        );
        assert!(
            error.contains("frame")
                || error.contains("nesting")
                || error.contains("container ended"),
            "{error}"
        );
    }
}

#[test]
#[ignore = "requires the packaged iunctus-cwl-js image and bounded Docker or Podman"]
fn cancellation_cleans_worker() {
    let evaluator = Evaluator::with_javascript(Vec::new());
    assert_eq!(evaluate(&evaluator, "${return 1;}"), Ok(Value::Integer(1)));
    let cancel = evaluator.settings.cancel.clone();
    let canceller = std::thread::spawn(move || {
        std::thread::sleep(Duration::from_millis(100));
        cancel.cancel();
    });
    let started = Instant::now();
    let error = evaluate(&evaluator, "${while(true){}} ").expect_err("cancellation");
    canceller.join().expect("canceller");
    assert!(
        error.contains("CWL JavaScript execution failure:"),
        "{error}"
    );
    assert!(started.elapsed() < Duration::from_secs(12), "{error}");
}

#[test]
#[ignore = "requires the packaged iunctus-cwl-js image and bounded Docker or Podman"]
fn cancels_driver_planning() {
    use workflow_graph::{
        Attempt, CancellationToken, Engine, Executor, Launch, Running, Simulator,
    };

    struct Cancelling {
        inner: Simulator,
        cancel: CancellationToken,
        trigger: Option<std::thread::JoinHandle<()>>,
        armed: Option<Instant>,
    }
    impl Executor for Cancelling {
        fn run(&mut self, attempt: Attempt) -> Running {
            let running = self.inner.run(attempt);
            self.armed = Some(Instant::now());
            let cancel = self.cancel.clone();
            self.trigger = Some(std::thread::spawn(move || {
                std::thread::sleep(Duration::from_millis(100));
                cancel.cancel();
            }));
            running
        }
    }
    let folder = tempfile::tempdir().expect("folder");
    let main = folder.path().join("main.cwl");
    std::fs::write(&main, "cwlVersion: v1.2\nclass: Workflow\nrequirements: {InlineJavascriptRequirement: {}}\n\
        inputs: []\noutputs: []\nsteps:\n  warm:\n    in: []\n    out: [ready]\n    run:\n      \
        class: CommandLineTool\n      baseCommand: [touch, ready]\n      arguments: ['${return 1;}']\n      \
        inputs: []\n      outputs: {ready: {type: File, outputBinding: {glob: ready}}}\n  \
        busy:\n    in: {ready: warm/ready}\n    out: []\n    run:\n      class: ExpressionTool\n      \
        inputs: {ready: File}\n      outputs: []\n      expression: '${/(a+)+$/.test(\"a\".repeat(40)+\"!\"); return {};}'\n").expect("workflow");
    let launch = Launch {
        target: main.clone(),
        scratch: folder.path().join("scratch"),
        simulated: true,
        ..Launch::default()
    };
    let mut driver = cwl_runtime::driver(&launch).expect("default driver");
    let cancel = CancellationToken::new();
    let mut executor = Cancelling {
        inner: Simulator::new(folder.path().join("simulator")),
        cancel: cancel.clone(),
        trigger: None,
        armed: None,
    };
    let execution = Engine::new(&mut executor)
        .cancel(cancel)
        .run("cwl", driver.as_mut());
    executor
        .trigger
        .take()
        .expect("helper warmed before cancellation")
        .join()
        .expect("canceller");
    let error = execution.error.expect("engine cancellation");
    assert!(
        matches!(&error, workflow_graph::ExecuteError::Cancelled),
        "{error}"
    );
    assert!(executor.cancel.is_cancelled());
    assert_eq!(execution.graph.nodes.len(), 1);
    assert!(
        executor.armed.expect("cancellation armed").elapsed() < Duration::from_secs(12),
        "{error}"
    );
}
