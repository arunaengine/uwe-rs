//! Complete runs from a Tokio host use the same recording and resume path as the CLI.
// Copyright (c) 2026 The iunctus Contributors
// SPDX-License-Identifier: MIT or Apache-2.0

use iunctus::{CancellationToken, Resume, RunRequest, run::Settings, setup::Setup};
use serde_json::json;

fn request(root: &std::path::Path) -> RunRequest {
    let target = root.join("tool.cwl");
    std::fs::write(&target,
        "cwlVersion: v1.2\nclass: CommandLineTool\nbaseCommand: echo\ninputs:\n  count: int\noutputs: []\n",
    ).unwrap();
    let mut request = RunRequest::new(Settings::new(target));
    request.settings.executor = Some("simulator".into());
    request.settings.database = Some(root.join("runs.sqlite"));
    request.settings.store = Some(root.join("store").to_string_lossy().into_owned());
    request.settings.cache = false;
    request.settings.setup = Some(Setup::default());
    request.settings.inputs = Some(json!({"count": 3}));
    request
}

#[tokio::test]
async fn defaults_to_local() {
    let root = tempfile::tempdir().unwrap();
    let mut request = request(root.path());
    request.settings.executor = Settings::default().executor;
    request.settings.setup.as_mut().unwrap().executors.engine = Some(
        root.path()
            .join("missing.sock")
            .to_string_lossy()
            .into_owned(),
    );
    let failure = request.run().await.err().unwrap();
    assert!(
        failure
            .message
            .contains("cannot reach a Docker or Podman engine")
    );
    assert!(failure.message.contains("-executor simulator"));
    assert!(failure.message.contains("iunctus check-machine"));
    let database = run_database::Database::open(&root.path().join("runs.sqlite")).unwrap();
    let runs = database.runs(1).unwrap();
    assert_eq!(runs[0].executor, "local");
    assert!(!runs[0].simulated);
}

#[tokio::test]
async fn records_async_runs() {
    let root = tempfile::tempdir().unwrap();
    let cancel = CancellationToken::new();
    let mut submission = request(root.path());
    submission.settings.cancel = cancel.clone();
    let run = submission.run().await.unwrap();
    assert!(run.execution.error.is_none(), "{:?}", run.execution.error);
    assert!(run.execution.graph.is_simulated());
    assert_eq!(run.execution.graph.nodes.len(), 1);
    assert!(
        !cancel.is_cancelled(),
        "completion must not cancel the host token"
    );

    let database = run_database::Database::open(&root.path().join("runs.sqlite")).unwrap();
    let recorded = database.run(&run.id).unwrap().unwrap();
    assert_ne!(recorded.state, "running");
    assert_eq!(recorded.arguments[0], "run");
    assert!(recorded.simulated);

    let mut resumed = request(root.path());
    resumed.settings.resume = Some(Resume::Run(run.id.clone()));
    resumed.settings.cancel = cancel;
    let resumed = resumed.run().await.unwrap();
    assert!(resumed.execution.error.is_none());
    assert_ne!(run.id, resumed.id);
    assert_eq!(
        database.run(&resumed.id).unwrap().unwrap().resumes,
        Some(run.id)
    );
}

#[tokio::test]
async fn shares_configured_driver() {
    let root = tempfile::tempdir().unwrap();
    let described = request(root.path()).describe().await.unwrap();
    assert_eq!(described.inputs.unwrap().values, json!({"count": 3}));
    let mut invalid = request(root.path());
    invalid.settings.inputs = Some(json!({"count": "3"}));
    assert!(invalid.run().await.is_err());
}

#[tokio::test]
async fn validates_then_runs() {
    use iunctus::application::Policy;
    use iunctus::forms::{Form, ValidationResponse};

    let root = tempfile::tempdir().unwrap();
    let form = Form::new(
        request(root.path()).describe().await.unwrap(),
        Policy::Strict,
    )
    .unwrap();
    let ValidationResponse::Checked(checked) = form.validate_response(json!({"count": 7})) else {
        panic!("the driver provides a form");
    };
    assert!(checked.accepted);
    let mut request = request(root.path());
    request.settings.inputs = Some(checked.values.clone());
    let run = request.run().await.unwrap();
    assert!(run.execution.error.is_none(), "{:?}", run.execution.error);
    assert_eq!(run.execution.graph.nodes.len(), 1);
    let database = run_database::Database::open(&root.path().join("runs.sqlite")).unwrap();
    assert_eq!(
        database.run_inputs(&run.id).unwrap().unwrap().values,
        checked.values
    );
}

#[tokio::test]
async fn params_keep_types() {
    let root = tempfile::tempdir().unwrap();
    let mut valid = request(root.path());
    valid.settings.params.insert("count".into(), json!(7));
    assert_eq!(
        valid.describe().await.unwrap().inputs.unwrap().values,
        json!({"count": 7})
    );
    let mut invalid = request(root.path());
    invalid.settings.params.insert("count".into(), json!("7"));
    assert!(invalid.run().await.is_err());
}

#[tokio::test]
async fn forwards_host_cancellation() {
    let root = tempfile::tempdir().unwrap();
    let cancel = CancellationToken::new();
    cancel.cancel();
    let mut request = request(root.path());
    request.settings.cancel = cancel;
    let run = request.run().await.unwrap();
    assert!(matches!(
        run.execution.error,
        Some(workflow_graph::ExecuteError::Cancelled)
    ));
}

#[tokio::test]
async fn concurrent_receipts_differ() {
    let root = tempfile::tempdir().unwrap();
    let first = request(root.path());
    let second = request(root.path());
    let (first, second) = tokio::join!(first.run(), second.run(),);
    let (first, second) = (first.unwrap(), second.unwrap());
    assert!(first.execution.error.is_none());
    assert!(second.execution.error.is_none());
    assert_ne!(first.id, second.id);
    let database = run_database::Database::open(&root.path().join("runs.sqlite")).unwrap();
    assert_eq!(database.runs(10).unwrap().len(), 2);
}

#[tokio::test]
async fn records_target_controls() {
    let root = tempfile::tempdir().unwrap();
    std::fs::write(
        root.path().join("Snakefile"),
        "rule make:\n    output: 'out'\n    shell: 'touch {output}'\n",
    )
    .unwrap();
    let mut request = RunRequest::new(Settings::new(root.path()));
    request.policy = iunctus::application::Policy::Relaxed;
    request.settings.executor = Some("simulator".into());
    request.settings.database = Some(root.path().join("runs.sqlite"));
    request.settings.until = vec!["out".into()];
    request.settings.omit_from = vec!["absent".into()];
    request.settings.forcerun = vec!["make".into()];
    request.settings.forceall = true;
    request.settings.setup = Some(Setup::default());
    let run = request.run().await.unwrap();
    assert!(run.execution.error.is_none(), "{:?}", run.execution.error);
    let database = run_database::Database::open(&root.path().join("runs.sqlite")).unwrap();
    let recorded = database.run(&run.id).unwrap().unwrap();
    for pair in [
        ["-until", "out"],
        ["-omit-from", "absent"],
        ["-forcerun", "make"],
    ] {
        assert!(recorded.arguments.windows(2).any(|words| words == pair));
    }
    assert!(recorded.arguments.iter().any(|word| word == "-forceall"));
}
