//! Config values from the command line merge into the values of the config files, as
//! Snakemake 9.27 merges them: `--config block={toad: t}` keeps the other keys of `block`.
// Copyright (c) 2026 The iunctus Contributors
// SPDX-License-Identifier: MIT or Apache-2.0

use workflow_graph::{Launch, Simulator, execute};

const SNAKEFILE: &str = r#"
rule all:
    output: "config.txt"
    shell: "echo {config[block][foo]} {config[block][toad]} {config[other]} > {output}"
"#;

#[test]
fn config_values_merge_into_config_files() {
    let scratch = tempfile::tempdir().expect("a temporary directory");
    let folder = scratch.path().join("workflow");
    std::fs::create_dir_all(&folder).expect("makes the folder");
    std::fs::write(folder.join("Snakefile"), SNAKEFILE).expect("writes the Snakefile");
    let config = folder.join("config.yaml");
    std::fs::write(&config, "block:\n  foo: from_file\n  toad: from_file\n").expect("writes");
    let launch = Launch {
        target: folder.clone(),
        configs: vec![config],
        params: vec![
            ("block".into(), "{toad: from_line}".into()),
            ("other".into(), "1".into()),
        ],
        scratch: scratch.path().to_path_buf(),
        simulated: true,
        ..Launch::default()
    };
    let mut driver = snakemake_runtime::driver(&launch).expect("a driver");
    let mut executor = Simulator::new(scratch.path().join("simulator"));
    let execution = execute(&folder.to_string_lossy(), driver.as_mut(), &mut executor);
    assert!(execution.error.is_none(), "{:?}", execution.error);
    let command = execution.graph.nodes[0].task.command.join(" ");
    // Snakemake 9.27 runs `echo from_file from_line 1 > config.txt` here.
    assert!(
        command.ends_with("echo from_file from_line 1 > config.txt"),
        "{command}"
    );
}
