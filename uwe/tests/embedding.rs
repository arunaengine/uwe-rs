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
async fn runs_lowered_documents() {
    let root = tempfile::tempdir().unwrap();
    let mut tasks = Vec::new();
    for lowered in [false, true] {
        let mut request = request(root.path());
        request.settings.lowered = lowered;
        let run = request.run().await.unwrap();
        assert!(run.execution.error.is_none(), "{:?}", run.execution.error);
        tasks.push(run.execution.graph.nodes[0].task.clone());
    }
    // The compiled task runs as the native one does, in the output folder of CWL.
    for task in &tasks {
        assert_eq!(task.workdir.as_deref(), Some("/var/spool/cwl"));
    }
    assert_eq!(tasks[0].command, tasks[1].command);
    let mut request = request(root.path());
    request.settings.lowered = true;
    request.settings.inputs = Some(json!({"count": "three"}));
    let Err(refused) = request.run().await else {
        panic!("a lowered run refuses an input of the wrong type");
    };
    assert!(refused.message.contains("not valid"), "{}", refused.message);
}

/// An embedded lowered Snakemake run takes its values as config, which the compiled document
/// holds, not as inputs of its graph.
#[tokio::test]
async fn runs_lowered_snakefiles() {
    let root = tempfile::tempdir().unwrap();
    let snakefile = "rule all:\n    input: \"a.txt\"\n\nrule a:\n    output: \"a.txt\"\n    shell: \"echo {config[sample_sheet]} > {output}\"\n";
    let target = root.path().join("Snakefile");
    std::fs::write(&target, snakefile).unwrap();
    let mut request = request(root.path());
    request.settings.target = target;
    request.settings.lowered = true;
    request.settings.inputs = Some(json!({"sample_sheet": "samples.tsv"}));
    // A Snakemake config has an advisory schema, natively as compiled.
    request.policy = uwe::application::Policy::Relaxed;
    let run = request.run().await.unwrap();
    assert!(run.execution.error.is_none(), "{:?}", run.execution.error);
    let script = run.execution.graph.nodes[0].task.script.clone();
    assert!(script.unwrap_or_default().contains("samples.tsv"));
    // A resume compiles the workflow again into the same document, with the same values.
    let mut resumed = RunRequest::new(request_settings(&run, root.path()));
    resumed.policy = uwe::application::Policy::Relaxed;
    let again = resumed.run().await.expect("resumes the compiled run");
    assert!(
        again.execution.error.is_none(),
        "{:?}",
        again.execution.error
    );
}

/// The settings of a resume of the compiled Snakefile run `run` in `root`.
fn request_settings(run: &uwe::Run, root: &std::path::Path) -> Settings {
    let mut settings = request(root).settings;
    settings.target = root.join("Snakefile");
    settings.lowered = true;
    settings.inputs = Some(json!({"sample_sheet": "samples.tsv"}));
    settings.resume = Some(Resume::Run(run.id.clone()));
    settings
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
    request.settings.no_cache_steps = vec!["make".into()];
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
        ["-no-cache-steps", "make"],
    ] {
        assert!(recorded.arguments.windows(2).any(|words| words == pair));
    }
    assert!(recorded.arguments.iter().any(|word| word == "-forceall"));
}

#[tokio::test]
async fn uses_chosen_id() {
    let root = tempfile::tempdir().unwrap();
    let mut first = request(root.path());
    first.settings.run_id = Some("01JBHOSTRUN_7".into());
    let run = first.run().await.unwrap();
    assert!(run.execution.error.is_none(), "{:?}", run.execution.error);
    assert_eq!(run.id, "01JBHOSTRUN_7");
    assert!(
        cache_folder()
            .join("scratch/simulator/01JBHOSTRUN_7")
            .is_dir()
    );
    let database = run_database::Database::open(&root.path().join("runs.sqlite")).unwrap();
    assert!(database.run("01JBHOSTRUN_7").unwrap().is_some());

    let mut again = request(root.path());
    again.settings.run_id = Some("01JBHOSTRUN_7".into());
    let failure = again.run().await.err().unwrap();
    assert_eq!(failure.code, 2, "{}", failure.message);
    assert!(failure.message.contains("already in the run database"));
    assert_eq!(database.runs(10).unwrap().len(), 1);

    for invalid in [
        "",
        "../escape",
        "a/b",
        "-dash",
        "tab\there",
        &"a".repeat(65),
    ] {
        let mut request = request(root.path());
        request.settings.run_id = Some(invalid.into());
        let failure = request.run().await.err().unwrap();
        assert!(failure.message.contains("is not allowed"), "{invalid:?}");
    }
    assert_eq!(database.runs(10).unwrap().len(), 1);
}

#[tokio::test]
async fn refuses_zero_time() {
    let root = tempfile::tempdir().unwrap();
    let mut zero = request(root.path());
    zero.settings.default_time = Some(std::time::Duration::ZERO);
    let failure = zero.run().await.err().unwrap();
    assert!(
        failure.message.contains("at least one second"),
        "{failure:?}"
    );
    let mut short = request(root.path());
    short.settings.default_time = Some(std::time::Duration::from_millis(1));
    let run = short.run().await.unwrap();
    assert!(run.execution.error.is_none(), "{:?}", run.execution.error);
}

#[tokio::test]
async fn reports_task_states() {
    use uwe::run::{Event, TaskState};
    let root = tempfile::tempdir().unwrap();
    let heard = std::sync::Arc::new(std::sync::Mutex::new(Vec::new()));
    let mut request = request(root.path());
    let sink = heard.clone();
    request.settings.listener = Some(std::sync::Arc::new(move |event: Event| {
        sink.lock().unwrap().push(event);
    }));
    let run = request.run().await.unwrap();
    assert!(run.execution.error.is_none(), "{:?}", run.execution.error);
    let heard = heard.lock().unwrap().clone();
    assert!(matches!(&heard[0], Event::Started { run: id, .. } if *id == run.id));
    assert!(
        matches!(heard.last().unwrap(), Event::Ended { run: id, error: None, .. } if *id == run.id)
    );
    let tasks: Vec<_> = heard[1..heard.len() - 1]
        .iter()
        .map(|event| match event {
            Event::Task(task) => task,
            other => panic!("only task events come between start and end: {other:?}"),
        })
        .collect();
    let states: Vec<_> = tasks.iter().map(|task| task.state).collect();
    assert_eq!(
        states,
        [
            TaskState::Requested,
            TaskState::Started,
            TaskState::Finished
        ]
    );
    let node = &run.execution.graph.nodes[0].task;
    for task in &tasks {
        assert_eq!((task.run.as_str(), task.task), (run.id.as_str(), node.id.0));
        assert_eq!(task.name, node.name);
    }
    assert_eq!(tasks[0].attempt, None);
    assert_eq!(tasks[1].attempt, Some(1));
    let finished = tasks[2];
    assert_eq!((finished.attempt, finished.exit_code), (Some(1), Some(0)));
    assert!(!finished.cached && finished.error.is_none());
    assert!(finished.started.is_some() && finished.ended >= finished.started);
    let json = serde_json::to_value(&heard[3]).unwrap();
    assert_eq!(
        (json["kind"].as_str(), json["state"].as_str()),
        (Some("task"), Some("finished"))
    );
}

#[tokio::test]
async fn pins_repository_revision() {
    let root = tempfile::tempdir().unwrap();
    let cache = root.path().join("cache");
    let commits = [
        ("0123456789abcdef0123456789abcdef01234567", "FIRST"),
        ("89abcdef0123456789abcdef0123456789abcdef", "SECOND"),
    ];
    // A cached checkout of a full commit is used without asking the server.
    for (commit, step) in commits {
        let folder = cache.join("pipelines/github.com/owner/demo").join(commit);
        std::fs::create_dir_all(&folder).unwrap();
        std::fs::write(folder.join(".uwe-revision"), commit).unwrap();
        std::fs::write(
            folder.join("main.nf"),
            format!("process {step} {{\n  script:\n  \"echo\"\n}}\nworkflow {{ {step}() }}\n"),
        )
        .unwrap();
    }
    let settings = |target: &str, revision: &str| {
        let mut settings = Settings::new(target);
        settings.revision = Some(revision.into());
        settings.executor = Some("simulator".into());
        settings.database = Some(root.path().join("runs.sqlite"));
        settings.cache_folder = Some(cache.clone());
        settings.setup = Some(Setup::default());
        settings
    };
    let database = |run: &str| {
        let database = run_database::Database::open(&root.path().join("runs.sqlite")).unwrap();
        database.run(run).unwrap().unwrap().revision
    };
    for (commit, step) in commits {
        let mut request = RunRequest::new(settings("owner/demo", commit));
        request.policy = uwe::application::Policy::Relaxed;
        let run = request.run().await.unwrap();
        assert!(run.execution.error.is_none(), "{:?}", run.execution.error);
        assert_eq!(run.execution.graph.nodes[0].task.step, step);
        assert_eq!(database(&run.id).as_deref(), Some(commit));
    }

    let local = RunRequest::new(settings(root.path().to_str().unwrap(), commits[0].0));
    let failure = local.run().await.err().unwrap();
    assert!(
        failure
            .message
            .contains("-r and -latest require a repository")
    );
    let empty = RunRequest::new(settings("owner/demo", ""));
    let failure = empty.run().await.err().unwrap();
    assert!(failure.message.contains("needs a tag, branch or commit"));
    let config = root.path().join("uwe.toml");
    std::fs::write(
        &config,
        format!(
            "[workflows.demo]\nsource='owner/demo'\nrevision='{}'\n",
            commits[0].0
        ),
    )
    .unwrap();
    let mut preset = RunRequest::new(settings("", commits[0].0));
    preset.settings.preset = Some("demo".into());
    preset.settings.config_file = Some(config);
    let failure = preset.run().await.err().unwrap();
    assert!(failure.message.contains("a preset supplies its source"));
}

/// The words that an embedded run must not write to the standard output of its host.
const SAID: &str = "said-by-the-workflow";

/// One run, in a child process of this test, of a Nextflow workflow that prints and of a CWL
/// workflow without steps whose output object holds a string; the parent reads its stdout.
#[test]
fn keeps_stdout_free() {
    if let Ok(language) = std::env::var("UWE_EMBEDDED_LANGUAGE") {
        let root = tempfile::tempdir().unwrap();
        let print = std::env::var("UWE_EMBEDDED_PRINT").is_ok();
        let run = tokio::runtime::Runtime::new()
            .unwrap()
            .block_on(printing(root.path(), &language, print).run())
            .unwrap();
        assert!(run.execution.error.is_none(), "{:?}", run.execution.error);
        return;
    }
    for language in ["nextflow", "cwl"] {
        for print in [false, true] {
            let mut command = std::process::Command::new(std::env::current_exe().unwrap());
            command.args(["--exact", "keeps_stdout_free", "--nocapture"]);
            command.env("UWE_EMBEDDED_LANGUAGE", language);
            if print {
                command.env("UWE_EMBEDDED_PRINT", "1");
            }
            let output = command.output().unwrap();
            let stdout = String::from_utf8_lossy(&output.stdout);
            let stderr = String::from_utf8_lossy(&output.stderr);
            assert!(output.status.success(), "{language}: {stdout}\n{stderr}");
            assert_eq!(stdout.contains(SAID), print, "{language}: {stdout}");
        }
    }
}

/// A secret value of a run that its Nextflow workflow prints.
const PRINTED: &str = "sentinel-printed-secret";

/// One run, in a child process of this test, of a Nextflow workflow that prints a secret value
/// to a print sink: the sink gets the line with `[secret]`, and the process prints nothing.
#[test]
fn prints_to_sink() {
    if std::env::var_os("UWE_EMBEDDED_SINK").is_some() {
        let root = tempfile::tempdir().unwrap();
        let mut request = printing(root.path(), "nextflow", false);
        let target = &request.settings.target;
        std::fs::write(
            target,
            "workflow {\n  println \"token ${env('LAB_TOKEN')}\"\n  \
            log.info 'logged'\n  System.out.println('said')\n}\n",
        )
        .unwrap();
        let config = target.with_file_name("nextflow.config");
        let line = "System.out.println(\"config ${System.getenv('LAB_TOKEN')}\")\n";
        std::fs::write(config, line).unwrap();
        request
            .settings
            .secrets
            .insert("LAB_TOKEN", PRINTED)
            .unwrap();
        let printed = std::sync::Arc::new(std::sync::Mutex::new(String::new()));
        let sink = printed.clone();
        request.settings.print_sink = Some(uwe::run::PrintSink::new(move |text: &str| {
            sink.lock().unwrap().push_str(text);
        }));
        let runtime = tokio::runtime::Runtime::new().unwrap();
        let run = runtime.block_on(request.run()).unwrap();
        assert!(run.execution.error.is_none(), "{:?}", run.execution.error);
        let printed = printed.lock().unwrap();
        assert!(printed.contains("token [secret]\n"), "{printed}");
        assert!(printed.contains("config [secret]\n"), "{printed}");
        assert!(
            printed.contains("logged\n") && printed.contains("said\n"),
            "{printed}"
        );
        assert!(!printed.contains(PRINTED));
        return;
    }
    let mut command = std::process::Command::new(std::env::current_exe().unwrap());
    command.args(["--exact", "prints_to_sink", "--nocapture"]);
    let output = command.env("UWE_EMBEDDED_SINK", "1").output().unwrap();
    let stdout = String::from_utf8_lossy(&output.stdout);
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(output.status.success(), "{stdout}\n{stderr}");
    for text in [&stdout, &stderr] {
        assert!(
            !text.contains(PRINTED) && !text.contains("token ") && !text.contains("config "),
            "{text}"
        );
    }
}

/// A TES server that answers every request with an empty list, enough for a run without tasks.
fn empty_server() -> String {
    use std::io::{Read, Write};
    let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let address = format!("http://{}", listener.local_addr().unwrap());
    std::thread::spawn(move || {
        for mut stream in listener.incoming().map_while(Result::ok) {
            let mut request = [0; 4096];
            let _ = stream.read(&mut request);
            let answer = r#"{"tasks":[]}"#;
            let reply = format!(
                "HTTP/1.1 200 OK\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{answer}",
                answer.len()
            );
            let _ = stream.write_all(reply.as_bytes());
        }
    });
    address
}

/// A run that would print: a Nextflow workflow in the simulator, and a real CWL run on a TES
/// server that runs nothing, since the workflow has no step.
fn printing(root: &std::path::Path, language: &str, print: bool) -> RunRequest {
    let (file, text, inputs, executor) = match language {
        "nextflow" => (
            "main.nf",
            format!("workflow {{\n  println '{SAID}'\n  System.out.println('{SAID}')\n}}\n"),
            None,
            "simulator",
        ),
        _ => (
            "flow.cwl",
            "cwlVersion: v1.2\nclass: Workflow\ninputs:\n  said: string\noutputs:\n  out:\n    type: string\n    outputSource: said\nsteps: []\n".into(),
            Some(json!({"said": SAID})),
            "tes",
        ),
    };
    std::fs::write(root.join(file), text).unwrap();
    let mut settings = Settings::new(root.join(file));
    settings.executor = Some(executor.into());
    settings.database = Some(root.join("runs.sqlite"));
    settings.store = Some(root.join("store").to_string_lossy().into_owned());
    settings.output_dir = Some(root.join("outputs"));
    settings.cache = false;
    settings.cache_folder = Some(root.join("cache"));
    let mut setup = Setup::default();
    setup.executors.tes_server = Some(empty_server());
    settings.setup = Some(setup);
    settings.inputs = inputs;
    settings.print = print;
    let mut request = RunRequest::new(settings);
    request.policy = uwe::application::Policy::Relaxed;
    request
}

/// A pipeline whose process `MAKE` sits in a subworkflow and passes its file to `USE`; `SIDE`
/// depends on neither.
const STEPS: &str = r#"
process MAKE {
    output:
    path('made.txt')
    script:
    'echo made > made.txt'
}

process USE {
    input:
    path(made)
    output:
    path('used.txt')
    script:
    "cat ${made} > used.txt"
}

process SIDE {
    output:
    path('side.txt')
    script:
    'echo side > side.txt'
}

workflow PREPARE {
    main:
    MAKE()
    emit:
    made = MAKE.out
}

workflow {
    USE(PREPARE().made)
    SIDE()
}
"#;

#[tokio::test]
async fn reruns_uncached_steps() {
    let root = tempfile::tempdir().unwrap();
    let target = root.path().join("main.nf");
    std::fs::write(&target, STEPS).unwrap();
    let request = |steps: Vec<String>| {
        let mut request = RunRequest::new(Settings::new(&target));
        request.policy = uwe::application::Policy::Relaxed;
        request.settings.executor = Some("simulator".into());
        request.settings.database = Some(root.path().join("runs.sqlite"));
        request.settings.cache_folder = Some(cache_folder());
        request.settings.setup = Some(Setup::default());
        request.settings.resume = Some(Resume::Last);
        request.settings.no_cache_steps = steps;
        request
    };
    let never = || vec!["MAKE".to_owned()];
    let mut graphs = Vec::new();
    for steps in [never(), Vec::new(), never()] {
        let run = request(steps).run().await.unwrap();
        assert!(run.execution.error.is_none(), "{:?}", run.execution.error);
        graphs.push(run.execution.graph);
    }
    let node = |graph: &workflow_graph::ExecutionGraph, step: &str| {
        let found = graph.nodes.iter().find(|node| node.task.step == step);
        found
            .cloned()
            .unwrap_or_else(|| panic!("no task of {step}"))
    };
    // The first run left no result of `MAKE` to resume, as with Nextflow's `cache false`.
    let made = node(&graphs[1], "PREPARE:MAKE");
    assert!(!made.reused);
    assert_eq!(made.task.reuse, workflow_graph::Reuse::Allowed);
    assert!(node(&graphs[1], "SIDE").reused);
    let graph = &graphs[2];
    let (made, used, side) = (
        node(graph, "PREPARE:MAKE"),
        node(graph, "USE"),
        node(graph, "SIDE"),
    );
    assert!(!made.reused, "the named step runs again");
    assert_eq!(made.task.reuse, workflow_graph::Reuse::Disabled);
    assert!(side.reused, "the other steps come from the earlier run");
    assert_eq!(side.task.reuse, workflow_graph::Reuse::Allowed);
    // The file of the step that ran again reaches the step after it.
    assert!(graph.edges.iter().any(|edge| edge.from == made.task.id
        && edge.to == used.task.id
        && matches!(edge.kind, workflow_graph::EdgeKind::File { .. })));
}

/// Two embedded Nextflow runs at the same time write the same relative path, each into a
/// launch folder of its own, never into the working directory of the host. The runs go in a
/// child process whose working directory is a temporary folder.
#[test]
fn launches_own_folder() {
    if std::env::var_os("UWE_EMBEDDED_LAUNCH").is_none() {
        let folder = tempfile::tempdir().unwrap();
        let output = std::process::Command::new(std::env::current_exe().unwrap())
            .args(["--exact", "launches_own_folder", "--nocapture"])
            .env("UWE_EMBEDDED_LAUNCH", "1")
            .current_dir(folder.path())
            .output()
            .unwrap();
        let text = String::from_utf8_lossy(&[output.stdout, output.stderr].concat()).into_owned();
        assert!(output.status.success(), "the child failed:\n{text}");
        assert!(text.contains("1 passed"), "the child ran no test:\n{text}");
        assert!(!folder.path().join("marker.txt").exists());
        return;
    }
    let root = tempfile::tempdir().unwrap();
    std::fs::write(
        root.path().join("main.nf"),
        "workflow {\n  file('marker.txt').text = params.word\n}\n",
    )
    .unwrap();
    let request = |word: &str| {
        let mut settings = Settings::new(root.path().join("main.nf"));
        settings.executor = Some("simulator".into());
        settings.database = Some(root.path().join(format!("{word}.sqlite")));
        settings.cache = false;
        settings.cache_folder = Some(root.path().join("cache"));
        settings.setup = Some(Setup::default());
        settings.params.insert("word".into(), json!(word));
        let mut request = RunRequest::new(settings);
        request.policy = uwe::application::Policy::Relaxed;
        request
    };
    let runtime = tokio::runtime::Runtime::new().unwrap();
    let (first, second) =
        runtime.block_on(async { tokio::join!(request("a").run(), request("b").run()) });
    for (run, word) in [(first.unwrap(), "a"), (second.unwrap(), "b")] {
        assert!(run.execution.error.is_none(), "{:?}", run.execution.error);
        let marker = root
            .path()
            .join("cache/scratch/launch")
            .join(&run.id)
            .join("marker.txt");
        assert_eq!(std::fs::read_to_string(&marker).unwrap(), word);
    }
    assert!(!std::env::current_dir().unwrap().join("marker.txt").exists());
}

/// An embedded run with a cache folder writes nothing into the cache folder of the user,
/// which `XDG_CACHE_HOME` and `HOME` name, also not the journal of its JavaScript helper
/// container. The run goes in a child process with both set.
#[test]
fn keeps_user_cache() {
    if let Some(root) = std::env::var_os("UWE_EMBEDDED_CACHE") {
        let root = std::path::PathBuf::from(root);
        let target = root.join("tool.cwl");
        std::fs::write(
            &target,
            "cwlVersion: v1.2\nclass: CommandLineTool\nrequirements:\n  InlineJavascriptRequirement: {}\n\
             baseCommand: echo\ninputs:\n  count: int\narguments: [$(inputs.count + 1)]\noutputs: []\n",
        )
        .unwrap();
        let mut settings = Settings::new(target);
        settings.executor = Some("simulator".into());
        settings.database = Some(root.join("runs.sqlite"));
        settings.cache = false;
        settings.cache_folder = Some(root.join("cache"));
        settings.inputs = Some(json!({"count": 3}));
        let runtime = tokio::runtime::Runtime::new().unwrap();
        let run = runtime.block_on(RunRequest::new(settings).run()).unwrap();
        assert!(run.execution.error.is_none(), "{:?}", run.execution.error);
        return;
    }
    let root = tempfile::tempdir().unwrap();
    let user = root.path().join("user");
    std::fs::create_dir(&user).unwrap();
    let output = std::process::Command::new(std::env::current_exe().unwrap())
        .args(["--exact", "keeps_user_cache", "--nocapture"])
        .env("UWE_EMBEDDED_CACHE", root.path())
        // The helper image that `just test` builds, as the test binary makes none.
        .env(
            "UWE_CWL_JS_IMAGE",
            std::env::var("UWE_CWL_JS_IMAGE").unwrap_or("uwe-cwl-js:0.1.0".into()),
        )
        .env("XDG_CACHE_HOME", user.join(".cache"))
        .env("HOME", &user)
        .output()
        .unwrap();
    let text = String::from_utf8_lossy(&[output.stdout, output.stderr].concat()).into_owned();
    assert!(output.status.success(), "the child failed:\n{text}");
    assert!(text.contains("1 passed"), "the child ran no test:\n{text}");
    let written: Vec<_> = walk(&user);
    assert!(
        written.is_empty(),
        "written into the user cache: {written:?}"
    );
}

/// The files below a folder.
fn walk(folder: &std::path::Path) -> Vec<std::path::PathBuf> {
    let mut files = Vec::new();
    for entry in std::fs::read_dir(folder).into_iter().flatten().flatten() {
        match entry.file_type().is_ok_and(|kind| kind.is_dir()) {
            true => files.extend(walk(&entry.path())),
            false => files.push(entry.path()),
        }
    }
    files
}

/// A chain as flowy runs it: a document of two native CWL blocks without graph outputs, the
/// first feeding the second. A real run on a TES server that runs nothing, as no block has a
/// step, delivers each block's output below the folder of its op, and records those files.
#[tokio::test]
async fn delivers_chain_blocks() {
    let root = tempfile::tempdir().unwrap();
    let root = root.path();
    let pass = "cwlVersion: v1.2\nclass: Workflow\ninputs:\n  text: File\n\
                outputs:\n  out:\n    type: File\n    outputSource: text\nsteps: []\n";
    std::fs::write(root.join("pass.cwl"), pass).unwrap();
    std::fs::write(root.join("a.txt"), "chained\n").unwrap();
    let file = json!({"file": {}});
    let block = |path: &str| {
        json!({
            "path": path, "kind": {"native": {"source": path, "language": "cwl"}},
            "inputs": [{"name": "text", "type": file}],
            "outputs": [{"name": "out", "type": file}]
        })
    };
    let source = |id: &str| json!({"id": id, "language": "cwl", "location": root.join("pass.cwl")});
    let document = json!({
        "wir_version": "0.1",
        "sources": [source("first"), source("second")],
        "graphs": [{
            "name": "main",
            "inputs": [{"name": "first.text", "type": file}],
            "ops": [block("first"), block("second")],
            "edges": [
                {"from": {"port": "first.text"}, "to": {"op": "first", "port": "text"}},
                {"from": {"op": "first", "port": "out"}, "to": {"op": "second", "port": "text"}}
            ]
        }],
        "entry": "main"
    });
    std::fs::write(root.join("chain.wir.json"), document.to_string()).unwrap();
    let mut settings = Settings::new(root.join("chain.wir.json"));
    settings.language = Some(uwe::run::Language::Wir);
    settings.executor = Some("tes".into());
    settings.database = Some(root.join("runs.sqlite"));
    settings.store = Some(root.join("store").to_string_lossy().into_owned());
    settings.output_dir = Some(root.join("outputs"));
    settings.cache = false;
    settings.cache_folder = Some(root.join("cache"));
    let mut setup = Setup::default();
    setup.executors.tes_server = Some(empty_server());
    settings.setup = Some(setup);
    let text = json!({"class": "File", "location": root.join("a.txt")});
    settings.inputs = Some(json!({"first.text": text}));
    settings.print = false;
    let mut request = RunRequest::new(settings);
    request.policy = uwe::application::Policy::Relaxed;
    let run = request.run().await.unwrap();
    assert!(run.execution.error.is_none(), "{:?}", run.execution.error);
    let outputs = root.join("outputs");
    for node in ["first", "second"] {
        let delivered = std::fs::read_to_string(outputs.join(node).join("a.txt"));
        assert_eq!(
            delivered.unwrap(),
            "chained\n",
            "a block delivers below its node"
        );
    }
    let database = run_database::Database::open(&root.join("runs.sqlite")).unwrap();
    let mut recorded: Vec<String> = (database.deliveries(&run.id).unwrap().into_iter())
        .map(|delivered| delivered.target)
        .collect();
    recorded.sort();
    let expected: Vec<String> = ["first/a.txt", "second/a.txt"]
        .iter()
        .map(|name| outputs.join(name).to_string_lossy().into_owned())
        .collect();
    assert_eq!(recorded, expected, "the run records the files that stay");
}
