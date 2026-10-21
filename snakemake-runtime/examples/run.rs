//! Runs a Snakemake workflow with the simulator and prints the execution graph:
//! `run <workflow folder or Snakefile> [target...]`.
// Copyright (c) 2026 The uwe Contributors
// SPDX-License-Identifier: MIT or Apache-2.0

use workflow_graph::{Launch, Simulator, execute};

fn main() {
    let mut args = std::env::args().skip(1);
    let Some(target) = args.next() else {
        eprintln!("usage: run <workflow> [target...]");
        std::process::exit(2);
    };
    let scratch = std::env::temp_dir().join(format!("iunctus-snakemake-{}", std::process::id()));
    let launch = Launch {
        target: target.clone().into(),
        targets: args.collect(),
        scratch: scratch.clone(),
        simulated: true,
        ..Launch::default()
    };
    let mut driver = match snakemake_runtime::driver(&launch) {
        Ok(driver) => driver,
        Err(error) => {
            eprintln!("{error}");
            std::process::exit(if error.unsupported { 33 } else { 1 });
        }
    };
    let mut executor = Simulator::new(scratch.join("simulator"));
    let execution = execute(&target, driver.as_mut(), &mut executor);
    print!("{}", execution.graph.summary());
    let _ = std::fs::remove_dir_all(&scratch);
    if let Some(error) = execution.error {
        eprintln!("{error}");
        std::process::exit(if error.unsupported() { 33 } else { 1 });
    }
}
