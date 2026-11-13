//! Checks merged command-line config values and YTE config templates.
// Copyright (c) 2026 The uwe Contributors
// SPDX-License-Identifier: MIT or Apache-2.0

use workflow_graph::{Launch, Simulator, execute};

const SNAKEFILE: &str = r#"
rule all:
    output: "config.txt"
    shell: "echo {config[block][foo]} {config[block][toad]} {config[other]} > {output}"
"#;

#[test]
fn merges_config_values() {
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

const YTE: &str = r#"__use_yte__: true
?if True: null
?if False: {unexpected: true}
?else: null
__definitions__:
  - import os
  - base = 5
__variables__:
  factor: ?base * 2
sum: ?base + 5
?if factor > 5:
  big: yes
?else:
  big: no
items:
  ?for i in range(3):
    - ?f"item{i}"
names:
  ?for name, n in [("a", 1), ("b", 2)]:
    ?name: ?n * factor
"#;

#[test]
fn renders_config_templates() {
    let scratch = tempfile::tempdir().expect("a temporary directory");
    let folder = scratch.path().join("workflow");
    std::fs::create_dir_all(&folder).expect("makes the folder");
    let snakefile = "configfile: \"config.yaml\"\n\nrule all:\n    output: \"yte.txt\"\n    \
                     shell: \"echo {config[sum]} {config[big]} {config[items]} \
                     {config[names][a]} {config[names][b]} > {output}\"\n";
    std::fs::write(folder.join("Snakefile"), snakefile).expect("writes the Snakefile");
    std::fs::write(folder.join("config.yaml"), YTE).expect("writes the config file");
    let launch = Launch {
        target: folder.clone(),
        scratch: scratch.path().to_path_buf(),
        simulated: true,
        ..Launch::default()
    };
    let mut driver = snakemake_runtime::driver(&launch).expect("a driver");
    let mut executor = Simulator::new(scratch.path().join("simulator"));
    let execution = execute(&folder.to_string_lossy(), driver.as_mut(), &mut executor);
    assert!(execution.error.is_none(), "{:?}", execution.error);
    let command = execution.graph.nodes[0].task.command.join(" ");
    // Snakemake 9.27 with yte 1.9.4 renders the config as {"big": true, "items": ["item0",
    // "item1", "item2"], "names": {"a": 10, "b": 20}, "sum": 10}.
    assert!(
        command.ends_with("echo 10 True item0 item1 item2 10 20 > yte.txt"),
        "{command}"
    );
}

#[test]
fn matches_mapping_classes() {
    let folder = tempfile::tempdir().expect("a folder");
    std::fs::write(
        folder.path().join("Snakefile"),
        "values = {'a1': 'one', 'b2': 'two', 'c3': 'three'}\nassert lookup(dpath='[ab]?', within=values) == ['one', 'two']\nassert lookup(dpath='[!ab]?', within=values) == ['three']\nrule all:\n    shell: 'true'\n",
    )
    .expect("a Snakefile");
    let launch = Launch {
        target: folder.path().to_path_buf(),
        scratch: folder.path().join("scratch"),
        ..Launch::default()
    };
    snakemake_runtime::driver(&launch)
        .expect("a driver")
        .start()
        .expect("character classes match");
}
