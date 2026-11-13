//! nf-crukci-support's functions against the output of nf-crukci-support 1.1.3 with Nextflow
//! 26.04.6 for the probe pipeline `main.nf` (`out-functions.txt`, `out-small.txt`, `out-boom.txt`).
// Copyright (c) 2026 The uwe Contributors
// SPDX-License-Identifier: MIT or Apache-2.0

mod common;

/// A run of the probe pipeline with these params, with its error and what it printed. The
/// probe's twelve-second pause in `OOM` is left out: uwe does not scan task logs.
fn run(params: &str, name: &str) -> (Option<String>, String) {
    let directory = tempfile::tempdir().expect("a temporary directory");
    let folder = directory.path();
    let script = std::fs::read_to_string(common::fixtures("nf_crukci_support").join("main.nf"))
        .expect("reads the probe");
    std::fs::write(folder.join("main.nf"), script.replace("sleep 12; ", "")).expect("writes");
    let config = format!("plugins {{ id 'nf-crukci-support@1.1.3' }}\nparams {{ {params} }}\n");
    std::fs::write(folder.join("nextflow.config"), config).expect("writes the config");
    let (execution, printed) = common::run(folder, name);
    (execution.error.map(|error| error.to_string()), printed)
}

#[test]
fn runs_functions() {
    let (error, printed) = run("small = false; boom = false", "functions_run");
    assert_eq!(error, None, "{printed}");
    for line in [
        "sizeOf: 3 1 0 2",
        "makeCollection: [f.txt] [a, b] null",
        "safeName: [MySample_Run1_2026_] [Data_File.txt] [TabRemoved] [_n_code-_.txt] []",
        "heap=1.8 GB meta=128 MB misc=64 MB all=2 GB opts=-XX:MaxMetaspaceSize=128m \
         -Xms1856m -Xmx1856m mb=1856",
    ] {
        assert!(printed.contains(line), "{line}\n{printed}");
    }
}

#[test]
fn refuses_small_memory() {
    let (error, printed) = run("small = true; boom = false", "small_run");
    let error = error.expect("the run fails");
    let message = "Insufficent memory for this Java task to run. Task is allocated 200 MB; \
                   overheads are 192 MB; minimum size for Java heap is 16 MB. The task needs \
                   at least 208 MB allocated.";
    assert!(error.contains(message), "{error}\n{printed}");
}

#[test]
fn rethrows_exception() {
    let (error, _) = run("small = false; boom = true", "boom_run");
    let error = error.expect("the run fails");
    assert!(error.contains("demo failure"), "{error}");
}
