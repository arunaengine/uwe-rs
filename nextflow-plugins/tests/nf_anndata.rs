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

/// Writes a script that runs `body` with `anndata` included, in a new folder.
fn script(body: &str) -> (tempfile::TempDir, PathBuf) {
    let folder = tempfile::tempdir().expect("a folder");
    let path = folder.path().join("main.nf");
    let text =
        format!("include {{ anndata }} from 'plugin/nf-anndata'\n\nworkflow {{\n{body}\n}}\n");
    std::fs::write(&path, text).expect("writes");
    (folder, path)
}

#[test]
fn reads_zarr_stores() {
    let (_folder, path) = script(
        "    ['v2.zarr', 'v3.zarr'].each { s ->\n        \
        def ad = anndata(file(\"${params.base}/${s}\"))\n        \
        println \"${s} ${ad.n_obs} ${ad.n_vars} ${ad.obs.index.name} ${ad.obs.colnames as List} ${ad.var.colnames as List} ${ad.layers as List} ${ad.uns as List} ${ad.obs_names[299]}\"\n        \
        ['kind', 'flag', 'small', 'score', 'count'].each { c ->\n            \
        def d = ad.obs.get(c).data\n            \
        def shown = (d[0..5] + [d[19], d[299]]).collect { v -> \"${v}<${v == null ? 'null' : v.getClass().simpleName}>\" }\n            \
        println \"${c} ${d.size()} ${shown.join(' ')}\"\n        }\n    }",
    );
    let base = fixtures().to_string_lossy().into_owned();
    let (lines, result) = run(&path, &format!("    base = '{base}'"));
    result.expect("the script runs");
    let mut expected = Vec::new();
    for store in ["v2.zarr", "v3.zarr"] {
        expected.extend([
            format!("{store} 300 2 cell_id [kind, flag, small, score, count] [] [counts] [neighbors] c299"),
            "kind 300 null<null> A<String> B<String> null<null> A<String> B<String> A<String> B<String>".into(),
            "flag 300 true<Boolean> false<Boolean> true<Boolean> false<Boolean> true<Boolean> false<Boolean> false<Boolean> false<Boolean>".into(),
            // Unsigned bytes of Zarr stores are signed, as in the plugin: 133 is -123.
            "small 300 0<Integer> 7<Integer> 14<Integer> 21<Integer> 28<Integer> 35<Integer> -123<Integer> 45<Integer>".into(),
            "score 300 0.0<Double> 0.25<Double> 0.5<Double> 0.75<Double> 1.0<Double> 1.25<Double> 4.75<Double> 74.75<Double>".into(),
            "count 300 null<null> 10<Long> 20<Long> 30<Long> 40<Long> null<null> 190<Long> 2990<Long>".into(),
        ]);
    }
    assert_eq!(lines, expected);
}

#[test]
fn checks_zarr_groups() {
    let folder = tempfile::tempdir().expect("a folder");
    let group = |path: &str, attributes: &str| {
        let directory = folder.path().join(path);
        std::fs::create_dir_all(&directory).expect("creates");
        std::fs::write(directory.join(".zgroup"), "{\"zarr_format\": 2}").expect("writes");
        std::fs::write(directory.join(".zattrs"), attributes).expect("writes");
    };
    for name in ["layers", "obs", "var", "obsm", "varm", "obsp"] {
        group(&format!("missing.zarr/{name}"), "{}");
        group(&format!("plainstore/{name}"), "{}");
    }
    group("missing.zarr", "{}");
    group("missing.zarr/.varp", "{}");
    group("plainstore", "{}");
    group("plainstore/varp", "{}");
    group("plainstore/uns", "{}");
    group("plainstore/obs", "{\"_index\": \"nope\"}");
    let (_folder, path) = script(
        "    ['missing.zarr', 'plainstore'].each { s ->\n        \
        try { anndata(file(\"${params.base}/${s}\")) }\n        \
        catch (Exception e) { println \"${s}: ${e.cause.message.replace(params.base, '<inputs>')}\" }\n    }",
    );
    let base = folder.path().to_string_lossy().into_owned();
    let (lines, result) = run(&path, &format!("    base = '{base}'"));
    result.expect("the script runs");
    assert_eq!(
        lines,
        [
            "missing.zarr: Missing fields: [varp, uns]",
            "plainstore: Index 'nope' not found in group: file://<inputs>/plainstore/obs",
        ]
    );
}
