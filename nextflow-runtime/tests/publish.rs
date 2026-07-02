//! Nextflow's `publishDir`: which outputs of a finished task the driver asks the engine to
//! deliver, where, and how, from the process and from the config.
// Copyright (c) 2026 The iunctus Contributors
// SPDX-License-Identifier: MIT or Apache-2.0

use workflow_graph::{DeliveryMode, Engine, Event, Launch, Observer, Simulator};

const PIPELINE: &str = r#"
process MAKE {
    publishDir "${params.outdir}/made", mode: 'copy', pattern: '*.txt'
    publishDir "${params.outdir}/all", saveAs: { name -> name.endsWith('.log') ? "logs/${name}" : name }
    input:
    val id
    output:
    path "${id}.txt"
    path "${id}.log"
    path 'versions.yml'
    script:
    """
    echo ${id} > ${id}.txt
    echo log > ${id}.log
    touch versions.yml
    """
}

process CONFIGURED {
    input:
    path text
    output:
    path 'copy.txt'
    path 'versions.yml'
    script:
    """
    cp ${text} copy.txt
    touch versions.yml
    """
}

workflow {
    CONFIGURED(MAKE('a')[0])
}
"#;

const CONFIG: &str = r#"
params.outdir = 'results'
process {
    withName: CONFIGURED {
        publishDir = [
            path: { "${params.outdir}/${task.process.toLowerCase()}" },
            mode: 'rellink',
            saveAs: { filename -> filename.equals('versions.yml') ? null : filename },
        ]
    }
}
"#;

/// Notes each delivery as its target relative to the launch directory and its mode.
#[derive(Default)]
struct Deliveries(Vec<(String, DeliveryMode)>, std::path::PathBuf);

impl Observer for Deliveries {
    fn event(&mut self, event: Event<'_>) {
        if let Event::Delivered { delivery } = event {
            let target = delivery
                .target
                .strip_prefix(&self.1)
                .unwrap_or(&delivery.target);
            self.0.push((target.display().to_string(), delivery.mode));
        }
    }
}

#[test]
fn outputs_go_where_publish_dir_says() {
    let directory = tempfile::tempdir().expect("a temporary directory");
    std::fs::write(directory.path().join("main.nf"), PIPELINE).expect("writes the script");
    std::fs::write(directory.path().join("nextflow.config"), CONFIG).expect("writes the config");
    // Relative publish paths are relative to the launch directory, the current one.
    let launch_dir = std::env::current_dir().expect("a current directory");
    let launch = Launch {
        target: directory.path().to_owned(),
        simulated: true,
        ..Launch::default()
    };
    let mut driver = nextflow_runtime::driver(&launch).expect("a driver");
    let mut executor = Simulator::new(directory.path().join("simulator"));
    let mut deliveries = Deliveries(Vec::new(), launch_dir);
    let execution = Engine::new(&mut executor)
        .observer(&mut deliveries)
        .run("main.nf", driver.as_mut());
    assert!(execution.error.is_none(), "{:?}", execution.error);
    let expected = [
        ("results/made/a.txt", DeliveryMode::Copy),
        ("results/all/a.txt", DeliveryMode::Link),
        ("results/all/logs/a.log", DeliveryMode::Link),
        ("results/all/versions.yml", DeliveryMode::Link),
        ("results/configured/copy.txt", DeliveryMode::RelativeLink),
    ];
    let expected: Vec<(String, DeliveryMode)> = expected
        .iter()
        .map(|(target, mode)| (target.to_string(), *mode))
        .collect();
    assert_eq!(deliveries.0, expected);
}
