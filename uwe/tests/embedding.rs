//! Complete runs from a Tokio host use the same recording and resume path as the CLI.
// Copyright (c) 2026 The uwe Contributors
// SPDX-License-Identifier: MIT or Apache-2.0

use serde_json::json;
use uwe::{CancellationToken, Resume, RunRequest, run::Settings, setup::Setup};

/// The cache folder of this test process, below the target folder and empty at its start,
/// so that no test writes into the cache folder of the user.
fn cache_folder() -> std::path::PathBuf {
    static EMPTIED: std::sync::Once = std::sync::Once::new();
    let folder = std::path::Path::new(env!("CARGO_TARGET_TMPDIR")).join("embedding-cache");
    EMPTIED.call_once(|| {
        let _ = std::fs::remove_dir_all(&folder);
    });
    folder
}

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
    request.settings.cache_folder = Some(cache_folder());
    request.settings.setup = Some(Setup::default());
    request.settings.inputs = Some(json!({"count": 3}));
    request
}

#[tokio::test]
async fn forwards_cwl_image() {
    let root = tempfile::tempdir().unwrap();
    for declared in [None, Some("declared:tag")] {
        let mut request = request(root.path());
        request.settings.default_image = Some("chosen:tag".into());
        if let Some(image) = declared {
            let target = &request.settings.target;
            let text = std::fs::read_to_string(target).unwrap().replace(
                "inputs:\n",
                &format!("requirements:\n  DockerRequirement:\n    dockerPull: {image}\ninputs:\n"),
            );
            std::fs::write(target, text).unwrap();
        }
        let run = request.run().await.unwrap();
        assert!(run.execution.error.is_none(), "{:?}", run.execution.error);
        assert_eq!(
            run.execution.graph.nodes[0].task.image.as_deref(),
            Some(declared.unwrap_or("chosen:tag"))
        );
    }
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
    assert!(failure.message.contains("uwe check-machine"));
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
    let scratch = cache_folder().join("scratch/simulator").join(&run.id);
    assert!(
        scratch.is_dir(),
        "the run uses the cache folder that it names"
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
async fn isolates_reused_storage() {
    let root = tempfile::tempdir().unwrap();
    std::fs::write(root.path().join("old-input"), "previous request").unwrap();
    let config = config_file::ConfigFile::parse(
        &format!(
            "[storage.mirror]\ntype='folder'\nprefixes='s3://bucket/'\npath='{}'",
            root.path().display()
        ),
        &root.path().join("config.toml"),
    )
    .unwrap();
    let shared = storage::Storage::from_config(&config).unwrap();
    shared
        .fetch("s3://bucket/old-input", &root.path().join("old-downloads"))
        .await
        .unwrap();
    let previous = shared.fetched();
    assert_eq!(previous.len(), 1);
    shared.pin(previous[0].clone());
    let mut described = request(root.path());
    described.settings.setup.as_mut().unwrap().storage = shared.clone();
    described.describe().await.unwrap();
    for _ in 0..2 {
        let mut submission = request(root.path());
        submission.settings.setup.as_mut().unwrap().storage = shared.clone();
        let run = submission.run().await.unwrap();
        assert!(run.execution.error.is_none());
        let database = run_database::Database::open(&root.path().join("runs.sqlite")).unwrap();
        assert!(database.remote_files(&run.id).unwrap().is_empty());
        assert_eq!(shared.fetched(), previous);
    }
}

#[tokio::test]
async fn validates_then_runs() {
    use uwe::application::Policy;
    use uwe::forms::{Form, ValidationResponse};

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
    request.policy = uwe::application::Policy::Relaxed;
    request.settings.executor = Some("simulator".into());
    request.settings.database = Some(root.path().join("runs.sqlite"));
    request.settings.until = vec!["out".into()];
    request.settings.omit_from = vec!["absent".into()];
    request.settings.forcerun = vec!["make".into()];
    request.settings.forceall = true;
    request.settings.cache_folder = Some(cache_folder());
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
