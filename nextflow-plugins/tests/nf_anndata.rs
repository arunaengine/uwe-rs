//! nf-anndata against the plugin 0.5.3 with Nextflow 26.04.6: the probe script of
//! `fixtures/nf_anndata` on two `.h5ad` files of the plugin's own tests.
// Copyright (c) 2026 The uwe Contributors
// SPDX-License-Identifier: MIT or Apache-2.0

use std::path::{Path, PathBuf};

use nextflow_evaluator::interpreter::{Error, Host};
use nextflow_evaluator::value::Value;
use nextflow_runtime::{Loader, Options, Run};

struct Console {
    lines: Vec<String>,
    launch: String,
}

impl Host for Console {
    fn print(&mut self, text: &str) {
        self.lines.extend(text.lines().map(str::to_owned));
    }

    fn launch_dir(&self) -> String {
        self.launch.clone()
    }
}

fn fixtures() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/nf_anndata")
}

/// Runs a script with nf-anndata turned on and `params` set, and gives the printed lines.
fn run(script: &Path, params: &str) -> (Vec<String>, Result<(), Error>) {
    let launch = tempfile::tempdir().expect("a folder");
    let mut console = Console {
        lines: Vec::new(),
        launch: launch.path().to_string_lossy().into_owned(),
    };
    let file = launch.path().join("probe.config");
    let text = format!("plugins {{\n    id 'nf-anndata@0.5.3'\n}}\nparams {{\n{params}\n}}\n");
    std::fs::write(&file, text).expect("writes");
    let project = script
        .parent()
        .expect("a folder")
        .to_string_lossy()
        .into_owned();
    let settings = nextflow_config::Options {
        profiles: None,
        params: Value::map([]),
        project_dir: project.clone(),
        launch_dir: console.launch.clone(),
        work_dir: launch.path().join("work").to_string_lossy().into_owned(),
    };
    let config = nextflow_config::load(&[file], &settings, &mut console).expect("loads");
    let options = Options {
        config,
        project_dir: project,
        script_file: script.to_string_lossy().into_owned(),
        ..Options::default()
    };
    let module = Loader::default().load(script).expect("loads");
    let result = Run::start(&module, &options, &mut console).and_then(|(mut run, requests)| {
        assert!(requests.is_empty(), "the probe runs no tasks");
        run.finish(&mut console)
    });
    (console.lines, result)
}

#[test]
fn matches_recorded_probe() {
    let base = fixtures().to_string_lossy().into_owned();
    let files = ["dtypes_nullable.h5ad", "index_categorical.h5ad"]
        .map(|name| format!("{base}/{name}"))
        .join(",");
    let params = format!("    base = '{base}'\n    files = '{files}'");
    let (lines, result) = run(&fixtures().join("probe.nf"), &params);
    result.expect("the probe runs");
    // The recording named the files without the folder of the plugin's tests.
    let lines: Vec<String> = (lines.iter())
        .map(|line| line.replace(&format!("=== {base}/"), "=== "))
        .collect();
    let recorded = std::fs::read_to_string(fixtures().join("expected.out")).expect("reads");
    // uwe's name sets are `LinkedHashSet`s, where the plugin gives a `LinkedHashMap` key set.
    let expected: Vec<String> = (recorded.lines())
        .map(|line| line.replace("LinkedKeySet", "LinkedHashSet"))
        .collect();
    assert_eq!(lines, expected);
}

#[test]
fn reports_store_errors() {
    let folder = tempfile::tempdir().expect("a folder");
    let base = folder.path().to_string_lossy().into_owned();
    std::fs::write(folder.path().join("not_hdf5.h5ad"), "hello").expect("writes");
    std::fs::write(folder.path().join("wrong.txt"), "x").expect("writes");
    let script = folder.path().join("errors.nf");
    let body = "include { anndata } from 'plugin/nf-anndata'\n\nworkflow {\n    \
        ['not_hdf5.h5ad', 'missing.h5ad', 'wrong.txt'].each { c ->\n        \
        try { anndata(file(\"${params.base}/${c}\")) }\n        \
        catch (Exception e) {\n            def r = e.cause ?: e\n            \
        println \"${c} !! ${e.getClass().name} / ${r.getClass().name}: ${r.message.replace(params.base, '<inputs>')}\"\n        \
        }\n    }\n}\n";
    std::fs::write(&script, body).expect("writes");
    let (lines, result) = run(&script, &format!("    base = '{base}'"));
    result.expect("the script runs");
    let wrapper = "java.lang.reflect.InvocationTargetException";
    assert_eq!(
        lines,
        [
            format!(
                "not_hdf5.h5ad !! {wrapper} / io.jhdf.exceptions.HdfException: No valid HDF5 signature found"
            ),
            format!(
                "missing.h5ad !! {wrapper} / io.jhdf.exceptions.HdfException: Failed to open file '<inputs>/missing.h5ad' . Is it a HDF5 file?"
            ),
            format!(
                "wrong.txt !! {wrapper} / java.lang.IllegalArgumentException: Unsupported AnnData storage format (expected .h5ad file or .zarr directory): <inputs>/wrong.txt"
            ),
        ]
    );
}
