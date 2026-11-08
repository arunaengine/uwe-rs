//! Isolated embedded runs: workflow code reaches only the folders and variables of its run.
// Copyright (c) 2026 The uwe Contributors
// SPDX-License-Identifier: MIT or Apache-2.0

use std::path::Path;

use serde_json::json;
use uwe::RunRequest;
use uwe::hosted::Isolation;
use uwe::run::Settings;

/// A variable of the service process that no workflow may read.
const SERVICE: &str = "UWE_TEST_SERVICE_TOKEN";

/// The child process that runs a test has `SERVICE` set and reads no site or user config.
const CHILD: &str = "UWE_ISOLATION_CHILD";

/// Tries to read, delete and overwrite a file outside the run, and to read `SERVICE`.
const NEXTFLOW: &str = "workflow {\n  def seen = []\n  \
    try { seen << file(params.outside).text } catch (e) { seen << 'refused read' }\n  \
    try { seen << file(params.outside).countLines() } catch (e) { seen << 'refused count' }\n  \
    try { file(params.outside).delete(); seen << 'deleted' } catch (e) { seen << 'refused delete' }\n  \
    try { file(params.outside).text = 'changed'; seen << 'written' } catch (e) { seen << 'refused write' }\n  \
    seen << (env('UWE_TEST_SERVICE_TOKEN') ?: 'no variable')\n  \
    seen << (System.getenv('UWE_TEST_SERVICE_TOKEN') ?: 'no variable')\n  \
    seen << file(params.inside).text\n  \
    file(params.report).text = seen.join('\\n')\n}\n";

/// The same at the top level of a Snakefile.
const SNAKEFILE: &str = "import os\nseen = []\ntry:\n    seen.append(open(config['outside']).read())\n\
    except OSError:\n    seen.append('refused read')\ntry:\n    os.remove(config['outside'])\n    \
    seen.append('deleted')\nexcept OSError:\n    seen.append('refused delete')\n\
    seen.append(os.environ.get('UWE_TEST_SERVICE_TOKEN', 'no variable'))\n\
    seen.append(open(config['inside']).read())\nwith open(config['report'], 'w') as written:\n    \
    written.write('\\n'.join(seen))\n\nrule all:\n    input: []\n";

const NEXTFLOW_SEEN: &str =
    "refused read\nrefused count\nrefused delete\nrefused write\nno variable\nno variable\ninside";
const SNAKEMAKE_SEEN: &str = "refused read\nrefused delete\nno variable\ninside";

#[test]
fn confines_workflow_code() {
    if std::env::var_os(CHILD).is_none() {
        return in_child("confines_workflow_code");
    }
    let root = tempfile::tempdir().unwrap();
    let outside = root.path().join("outside/secret.txt");
    std::fs::create_dir_all(outside.parent().unwrap()).unwrap();
    for (name, script, seen) in [
        ("main.nf", NEXTFLOW, NEXTFLOW_SEEN),
        ("Snakefile", SNAKEFILE, SNAKEMAKE_SEEN),
    ] {
        std::fs::write(&outside, "server file").unwrap();
        let folder = root.path().join(name);
        let data = folder.join("data");
        std::fs::create_dir_all(&data).unwrap();
        std::fs::write(data.join("inside.txt"), "inside").unwrap();
        std::fs::write(folder.join(name), script).unwrap();
        let mut settings = settings(&folder, &folder.join(name));
        let path = |path: &Path| json!(path.to_string_lossy());
        settings.params.insert("outside".into(), path(&outside));
        settings
            .params
            .insert("inside".into(), path(&data.join("inside.txt")));
        settings
            .params
            .insert("report".into(), path(&data.join("report.txt")));
        settings.isolation = Some(Isolation {
            roots: vec![data.clone()],
            launch: Some(folder.join("launch")),
            ..Isolation::default()
        });
        let mut request = RunRequest::new(settings);
        request.policy = uwe::application::Policy::Relaxed;
        let runtime = tokio::runtime::Runtime::new().unwrap();
        let ran = runtime.block_on(request.run());
        let ran = ran.unwrap_or_else(|failure| panic!("{name}: {}", failure.message));
        assert!(
            ran.execution.error.is_none(),
            "{name}: {:?}",
            ran.execution.error
        );
        let report = std::fs::read_to_string(data.join("report.txt")).unwrap();
        assert_eq!(report, seen, "{name}");
        assert_eq!(std::fs::read_to_string(&outside).unwrap(), "server file");
    }
}

/// An isolated Nextflow run without a launch folder of the host launches in one of its own.
#[test]
fn launches_isolated_run() {
    if std::env::var_os(CHILD).is_none() {
        return in_child("launches_isolated_run");
    }
    let folder = tempfile::tempdir().unwrap();
    let workflow = folder.path().join("main.nf");
    std::fs::write(
        &workflow,
        "workflow {\n  file('marker.txt').text = 'here'\n}\n",
    )
    .unwrap();
    let mut settings = settings(folder.path(), &workflow);
    settings.isolation = Some(Isolation::default());
    let mut request = RunRequest::new(settings);
    request.policy = uwe::application::Policy::Relaxed;
    let runtime = tokio::runtime::Runtime::new().unwrap();
    let ran = runtime.block_on(request.run());
    let ran = ran.unwrap_or_else(|failure| panic!("{}", failure.message));
    assert!(ran.execution.error.is_none(), "{:?}", ran.execution.error);
    let launch = folder.path().join("cache/scratch/launch").join(&ran.id);
    assert_eq!(
        std::fs::read_to_string(launch.join("marker.txt")).unwrap(),
        "here"
    );
}

/// A run of `workflow` with its database, store, cache and outputs in `folder`.
fn settings(folder: &Path, workflow: &Path) -> Settings {
    let mut settings = Settings::new(workflow);
    settings.database = Some(folder.join("runs.sqlite"));
    settings.store = Some(folder.join("store").to_string_lossy().into_owned());
    settings.output_dir = Some(folder.join("outputs"));
    settings.cache = false;
    settings.cache_folder = Some(folder.join("cache"));
    settings
}

/// Runs one test of this file again in a child process with `SERVICE` set and without config
/// files.
fn in_child(test: &str) {
    let config = tempfile::tempdir().unwrap();
    let output = std::process::Command::new(std::env::current_exe().unwrap())
        .args(["--exact", test, "--nocapture", "--test-threads=1"])
        .env(CHILD, "1")
        .env("UWE_SITE_CONFIG", "")
        .env("XDG_CONFIG_HOME", config.path())
        .env(SERVICE, "service-value")
        .output()
        .unwrap();
    let text = [output.stdout, output.stderr].concat();
    let text = String::from_utf8_lossy(&text);
    assert!(output.status.success(), "{test} failed:\n{text}");
}
