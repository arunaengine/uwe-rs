//! Names of inputs, outputs, logs and params that Snakemake's named lists keep for themselves,
//! like `items`, fail as they do in Snakemake 9.27.0, whose `snakemake -n` says:
//! `AttributeError in file "Snakefile", line 1: invalid name for input, output, wildcard,
//! params or log: items is reserved for internal use`.
// Copyright (c) 2026 The iunctus Contributors
// SPDX-License-Identifier: MIT or Apache-2.0

use workflow_graph::{Launch, Simulator, execute};

/// The error of a simulated run of a Snakefile, if it has one.
fn error(snakefile: &str) -> Option<String> {
    let folder = tempfile::tempdir().expect("a temporary directory");
    std::fs::write(folder.path().join("Snakefile"), snakefile).expect("writes");
    let launch = Launch {
        target: folder.path().to_owned(),
        scratch: folder.path().join("scratch"),
        simulated: true,
        ..Launch::default()
    };
    let mut driver = match snakemake_runtime::driver(&launch) {
        Ok(driver) => driver,
        Err(error) => return Some(error.to_string()),
    };
    let mut simulator = Simulator::new(folder.path().join("simulator"));
    let execution = execute("Snakefile", driver.as_mut(), &mut simulator);
    execution.error.map(|error| error.to_string())
}

#[test]
fn names_that_named_lists_keep_fail() {
    let params =
        "rule a:\n    output: \"a.txt\"\n    params: items=1\n    shell: \"touch {output}\"\n";
    let message = error(params).expect("an error");
    assert!(
        message.contains(
            "AttributeError: invalid name for input, output, wildcard, params or log: items is \
             reserved for internal use"
        ),
        "{message}"
    );
    // Inputs keep their sizes too, outputs do not; `index` and `sort` may be names.
    let input =
        "rule a:\n    input: size=\"x\"\n    output: \"a.txt\"\n    shell: \"touch {output}\"\n";
    assert!(error(input).expect("an error").contains("size is reserved"));
    let output =
        "rule a:\n    output: size=\"a.txt\", index=\"b.txt\"\n    shell: \"touch {output}\"\n";
    assert_eq!(error(output), None);
}
