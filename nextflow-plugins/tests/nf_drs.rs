//! nf-drs: the `drs` scope of a config reaches the DRS provider that reads `drs://` paths.
// Copyright (c) 2026 The uwe Contributors
// SPDX-License-Identifier: MIT or Apache-2.0

use nextflow_config::Config;
use nextflow_evaluator::interpreter::Host;
use nextflow_evaluator::value::Value;
use nextflow_plugins::catalogue;

struct Quiet;

impl Host for Quiet {
    fn print(&mut self, _: &str) {}
}

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

/// The scope is checked before any request, so no server is needed.
#[test]
fn scope_reaches_storage() {
    let config = loaded(
        "plugins { id 'nf-drs@0.1.0' }\ndrs {\n    authProvider = 'bearer'\n    \
         authSecret = 'UWE_TEST_UNSET_DRS_TOKEN'\n}\n",
    );
    let observers = catalogue::observers(&config).expect("an activated plugin");
    assert!(observers.is_empty());
    let storage = storage::Storage::default().blocking();
    let error = storage
        .stat("drs://127.0.0.1:9/object")
        .expect_err("no token");
    let error = error.to_string();
    assert!(error.contains("UWE_TEST_UNSET_DRS_TOKEN"), "{error}");
    assert!(error.contains("bearer token"), "{error}");
    let config = loaded("plugins { id 'nf-drs' }\ndrs.authProvider = 'kerberos'\n");
    catalogue::observers(&config).expect("an activated plugin");
    let storage = storage::Storage::default().blocking();
    let error = storage
        .stat("drs://127.0.0.1:9/object")
        .expect_err("an unknown login");
    assert!(error.to_string().contains("'kerberos'"), "{error}");
}
