//! The module that a `run:` block runs in: the rule function of Snakemake with the block as
//! its body, and the Snakefile's globals that it uses (tests/fixtures/embedded/run).
// Copyright (c) 2026 The iunctus Contributors
// SPDX-License-Identifier: MIT or Apache-2.0

use std::path::Path;

use workflow_graph::{Launch, Simulator, execute};

#[test]
fn run_blocks_carry_the_globals_they_use() {
    let folder = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/embedded/run");
    let scratch = tempfile::tempdir().expect("a temporary directory");
    let launch = Launch {
        target: folder.clone(),
        scratch: scratch.path().to_path_buf(),
        simulated: true,
        ..Launch::default()
    };
    let mut driver = snakemake_runtime::driver(&launch).expect("a driver");
    let mut executor = Simulator::new(scratch.path().join("simulator"));
    let execution = execute("run", driver.as_mut(), &mut executor);
    assert!(execution.error.is_none(), "{:?}", execution.error);
    let task = (execution.graph.nodes.iter())
        .map(|node| &node.task)
        .find(|task| task.step == "block")
        .expect("the task of the block");
    assert_eq!(task.command, ["python3", ".snakemake/scripts/block.run.py"]);
    let module = (task.inputs.iter())
        .find(|input| input.path == ".snakemake/scripts/block.run.py")
        .and_then(|input| input.content.clone())
        .expect("the module");
    let lib = folder.join("lib").to_string_lossy().into_owned();
    let helpers = folder.join("lib/helpers.py").to_string_lossy().into_owned();
    for expected in [
        // The block keeps its text, also inside a text of several lines.
        "        text = \"\"\"kept\nas written\"\"\"\n",
        "def __rule_block(input, output, params, wildcards, threads, resources, log, rule, ",
        "__is_snakemake_rule_func=True):\n",
        // Functions of the workflow come with what they use, before them.
        "square = (lambda x: x * x)\n",
        "def total(values):\n    return sum(square(value) for value in values)\n",
        "NUMBERS = {'a': 1, 'b': [2.5, None]}\n",
        // A local module is imported from its folder, which the task sees.
        "    from helpers import tidy as tidy\n",
        &format!("sys.path.extend(['{lib}'])"),
        // Live objects of the interpreter cannot go.
        "workflow = __iunctus.Unavailable('workflow is ",
        "shell._process_prefix = 'set -e; '\n",
        "config = {}\n",
    ] {
        assert!(module.contains(expected), "no {expected:?} in:\n{module}");
    }
    assert!(module.find("square =") < module.find("def total"));
    assert!(task.inputs.iter().any(|input| input.path == helpers));
    assert!(
        task.inputs
            .iter()
            .any(|input| input.path == ".snakemake/iunctus")
    );
}
