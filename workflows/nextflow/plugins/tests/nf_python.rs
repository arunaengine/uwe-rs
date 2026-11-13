//! nf-python against the messages and results that nf-python 0.1.4 gave with Nextflow 26.04.6;
//! the calls that need Python run only when Docker has the Python image already.
// Copyright (c) 2026 The uwe Contributors
// SPDX-License-Identifier: MIT or Apache-2.0

mod common;

use std::path::Path;
use std::process::Command;

/// The Python image of the calls, which the tests never pull.
const IMAGE: &str = "ghcr.io/astral-sh/uv:0.9.30-python3.12-bookworm-slim";

/// Runs a workflow body in a new folder; the error of the run and what it printed.
fn run_body(body: &str, config: &str) -> (Option<String>, String) {
    let directory = tempfile::tempdir().expect("a temporary directory");
    let folder = directory.path();
    let script =
        format!("include {{ pyFunction }} from 'plugin/nf-python'\nworkflow {{\n{body}\n}}\n");
    std::fs::write(folder.join("main.nf"), script).expect("writes the script");
    let config = format!(
        "plugins {{ id 'nf-python@0.1.4' }}\nworkDir = '{}/work'\n{config}\n",
        folder.display()
    );
    std::fs::write(folder.join("nextflow.config"), config).expect("writes the config");
    let (execution, printed) = common::run(folder, "python");
    (execution.error.map(|error| error.to_string()), printed)
}

#[test]
fn fails_before_python() {
    let cases = [
        ("println pyFunction()", "", "Missing code argument"),
        (
            "println pyFunction('nf.output(a=1)', script: 'echo_kwargs.py')",
            "",
            "Cannot use both code and script options together",
        ),
        (
            "println pyFunction('nf.output(a=1)', _executable: 'python3', _conda_env: 'numpy')",
            "",
            "The 'executable' and 'conda_env' options cannot be used together",
        ),
        (
            "println pyFunction('nf.output(a=1)')",
            "nf_python { executable = 'python3'; conda_env = 'numpy' }",
            "The 'executable' and 'conda_env' options cannot be used together",
        ),
        (
            "println pyFunction('nf.output(a=1)', d: new Date(0))",
            "",
            "Cannot serialize type: class java.util.Date",
        ),
        (
            "println pyFunction('nf.output(a=1)')",
            "nf_python.executable = 3",
            "Expected a string, got: 3",
        ),
        (
            "println pyFunction('nf.output(a=1)', _conda_env: 'numpy')",
            "",
            "cannot use the conda environment numpy",
        ),
    ];
    for (body, config, expected) in cases {
        let (error, printed) = run_body(body, config);
        let error = error.unwrap_or_default();
        assert!(error.contains(expected), "{body}: {error}\n{printed}");
    }
}

/// Whether the container engine has the image, so that a call needs no download.
fn image_present() -> bool {
    let found = Command::new("docker")
        .args(["image", "inspect", "--format", "{{.Id}}", IMAGE])
        .output();
    found.is_ok_and(|output| output.status.success())
}

#[test]
fn runs_python_container() {
    if !image_present() {
        eprintln!("skipped: Docker or the image {IMAGE} is not on this machine");
        return;
    }
    let directory = tempfile::tempdir().expect("a temporary directory");
    let folder = directory.path();
    let fixtures = common::fixtures("nf_python");
    common::copy(&fixtures, folder, &["echo_kwargs.py"]);
    // The launch folder is the crate's, so the script and the work folder are absolute.
    let script = std::fs::read_to_string(fixtures.join("main.nf")).expect("the script");
    let script = script.replace(
        "'echo_kwargs.py'",
        &format!("'{}/echo_kwargs.py'", folder.display()),
    );
    std::fs::write(folder.join("main.nf"), script).expect("writes the script");
    let config = format!(
        "plugins {{ id 'nf-python@0.1.4' }}\nworkDir = '{}/work'\n\
         nf_python {{ executable = 'python3' }}\n",
        folder.display()
    );
    std::fs::write(folder.join("nextflow.config"), config).expect("writes the config");
    let (execution, printed) = common::run(folder, "python");
    assert!(
        execution.error.is_none(),
        "{:?}\n{printed}",
        execution.error
    );
    let recorded = std::fs::read_to_string(fixtures.join("main.out.txt")).expect("the record");
    for prefix in ["echo: ", "inline: ", "positional: ", "args-type: "] {
        let line = |text: &str| {
            text.lines()
                .find(|line| line.starts_with(prefix))
                .map(str::to_owned)
        };
        assert_eq!(line(&printed), line(&recorded), "{printed}");
    }
    let calls = Path::new(folder).join("work/.nf-python");
    assert!(calls.join("lib/nf_python/__init__.py").is_file());
    let (error, printed) = run_body(
        "println pyFunction('import sys; print(\"boom\", file=sys.stderr); sys.exit(3)')",
        "",
    );
    assert_eq!(
        error
            .as_deref()
            .map(|error| error.contains("Python script evaluation failed")),
        Some(true)
    );
    assert!(
        printed.contains("exited with exit code 3\nFirst 10 lines of stderr:\nboom\n"),
        "{printed}"
    );
    let (error, _) = run_body(
        "println pyFunction('nf.output(a=1)', _executable: 'nope-python')",
        "",
    );
    let error = error.unwrap_or_default();
    assert!(
        error.contains("Cannot run program \"nope-python\""),
        "{error}"
    );
}
