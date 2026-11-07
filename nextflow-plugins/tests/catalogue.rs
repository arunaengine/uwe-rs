//! The plugin catalogue: every registry plugin with its state, the warnings for the plugins
//! of a config that a run skips, and the environment that observers read.
// Copyright (c) 2026 The uwe Contributors
// SPDX-License-Identifier: MIT or Apache-2.0

use std::collections::BTreeSet;

use nextflow_config::Config;
use nextflow_evaluator::interpreter::Host;
use nextflow_evaluator::value::Value;
use nextflow_plugins::catalogue::{self, Kind, State};

struct Quiet;

impl Host for Quiet {
    fn print(&mut self, _: &str) {}
}

/// A config that loads this text.
fn loaded(text: &str) -> Config {
    let folder = tempfile::tempdir().expect("a temporary directory");
    let file = folder.path().join("nextflow.config");
    std::fs::write(&file, text).expect("writes");
    let options = nextflow_config::Options {
        profiles: None,
        params: Value::map([]),
        project_dir: "/project".into(),
        launch_dir: "/launch".into(),
        work_dir: "/work".into(),
    };
    nextflow_config::load(&[file], &options, &mut Quiet).expect("loads")
}

#[test]
fn warns_skipped_plugins() {
    let config = loaded(
        "plugins {\n    id 'nf-schema@2.1.0'\n    id 'nf-unknown@1.0'\n    id 'nf-aspera'\n    \
         id 'nf-agent-pi@0.1.0'\n    id 'nf-hello'\n    id 'nf-agent-pi'\n}\n",
    );
    assert_eq!(
        catalogue::configured(&config),
        [
            "nf-agent-pi",
            "nf-aspera",
            "nf-hello",
            "nf-schema",
            "nf-unknown"
        ]
    );
    assert_eq!(
        catalogue::warnings(&config),
        [
            "The plugin `nf-agent-pi` is planned in uwe but not implemented yet (deferred until \
             the Nextflow agent feature leaves preview; needs a container talking gRPC); its \
             features are skipped",
            "The plugin `nf-aspera` cannot be implemented in uwe (it needs IBM's closed FASP \
             protocol); its features are skipped",
            "The plugin `nf-hello` is not supported by uwe (no release since 2024-03-06); its \
             features are skipped",
            "The plugin `nf-unknown` is not known to uwe; its features are skipped",
        ]
    );
    let observers = catalogue::observers(&config, std::path::Path::new("/launch"), &|name| {
        std::env::var(name).ok()
    })
    .expect("no plugin config fails");
    assert!(observers.is_empty());
    assert!(catalogue::warnings(&loaded("params.x = 1\n")).is_empty());
}

#[test]
fn lists_registry_plugins() {
    let ids: BTreeSet<&str> = catalogue::PLUGINS.iter().map(|plugin| plugin.id).collect();
    assert_eq!(ids.len(), catalogue::PLUGINS.len(), "each plugin once");
    assert_eq!(ids.len(), 111, "the plugins of the registry");
    let schema = catalogue::find("nf-schema").expect("nf-schema");
    assert_eq!(
        (schema.kind, schema.state),
        (Kind::Functions, State::Implemented)
    );
    let utils = catalogue::find("nf-core-utils").expect("nf-core-utils");
    assert_eq!(utils.state, State::Implemented);
    let capabilities = nextflow_plugins::capabilities();
    let aspera = &capabilities["nf-aspera"];
    assert_eq!(aspera["kind"], "file system");
    assert_eq!(aspera["state"], "not implementable");
    assert_eq!(aspera["reason"], "it needs IBM's closed FASP protocol");
    let core = &capabilities["nf-core-utils"];
    assert_eq!(core["supported"].as_array().map(Vec::len), Some(40));
    assert_eq!(core["unavailable"], serde_json::json!([]));
    assert_eq!(capabilities["nf-agent"]["state"], "partial");
    assert_eq!(capabilities["nf-agent-pi"]["state"], "planned");
    let planned: Vec<&str> = (catalogue::PLUGINS.iter())
        .filter(|plugin| matches!(plugin.state, State::Planned(_)))
        .map(|plugin| plugin.id)
        .collect();
    assert_eq!(
        planned,
        ["nf-agent-pi"],
        "every other plugin has a decided state"
    );
    assert_eq!(capabilities["nf-pgcache"]["state"], "not implementable");
    assert_eq!(capabilities["nf-iac"]["state"], "out of scope");
    assert_eq!(
        capabilities["nf-nim"]["reason"],
        "it ignores the task's inputs and always sends the same downloaded file"
    );
}

/// Set in the child process of `observers_read_run`, which has a token of its own.
const CHILD: &str = "NF_PLUGINS_ENVIRONMENT_CHILD";

#[test]
fn observers_read_run() {
    if std::env::var_os(CHILD).is_none() {
        let output = std::process::Command::new(std::env::current_exe().expect("the test"))
            .args(["--exact", "observers_read_run", "--test-threads=1"])
            .env(CHILD, "1")
            .env("TOWER_ACCESS_TOKEN", "process-token")
            .output()
            .expect("runs the child");
        let text = String::from_utf8_lossy(&output.stdout);
        assert!(output.status.success(), "{text}");
        return;
    }
    let config = loaded("tower { enabled = true; endpoint = 'http://127.0.0.1:9' }\n");
    let launch = std::path::Path::new("/launch");
    let given = |name: &str| (name == "TOWER_ACCESS_TOKEN").then(|| "run-token".to_owned());
    let observers = catalogue::observers(&config, launch, &given).expect("the token of the run");
    assert_eq!(observers.len(), 1);
    let error = catalogue::observers(&config, launch, &|_| None).err();
    let error = error.expect("no token without a value of the run");
    assert!(
        error.starts_with("Missing Seqera Platform access token"),
        "{error}"
    );
}
