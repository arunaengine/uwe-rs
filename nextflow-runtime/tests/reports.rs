//! Nextflow's own reports of a real run: the trace, the timeline, the report and the graph of
//! the processes, which the driver writes at the end and asks the engine to deliver.
// Copyright (c) 2026 The uwe Contributors
// SPDX-License-Identifier: MIT or Apache-2.0

use workflow_graph::{Delivery, Driver, Engine, Event, Launch, Observer, Simulator};

const PIPELINE: &str = r#"
process MAKE {
    input:
    val id
    output:
    path "${id}.txt"
    script:
    "echo ${id} > ${id}.txt"
}

process JOIN {
    input:
    path texts
    output:
    path 'all.txt'
    script:
    "cat ${texts} > all.txt"
}

workflow {
    JOIN(MAKE(Channel.of('a', 'b')).collect())
}
"#;

const CONFIG: &str = r#"
params.outdir = 'results'
trace {
    enabled = true
    file = "${params.outdir}/pipeline_info/execution_trace.txt"
    fields = 'task_id,name,process,tag,status,exit,duration'
}
timeline {
    enabled = true
    file = "${params.outdir}/pipeline_info/execution_timeline.html"
}
report {
    enabled = true
    file = "${params.outdir}/pipeline_info/execution_report.html"
}
dag {
    enabled = true
    file = "${params.outdir}/pipeline_info/pipeline_dag.html"
}
"#;

#[derive(Default)]
struct Deliveries(Vec<Delivery>);

impl Observer for Deliveries {
    fn event(&mut self, event: Event<'_>) {
        if let Event::Delivered { delivery } = event {
            self.0.push(delivery.clone());
        }
    }
}

/// The deliveries of a run, and its driver, which keeps the reports until it goes.
fn run(simulated: bool) -> (Vec<Delivery>, Box<dyn Driver>, tempfile::TempDir) {
    let directory = tempfile::tempdir().expect("a temporary directory");
    std::fs::write(directory.path().join("main.nf"), PIPELINE).expect("writes the script");
    std::fs::write(directory.path().join("nextflow.config"), CONFIG).expect("writes the config");
    let launch = Launch {
        target: directory.path().to_owned(),
        scratch: directory.path().join("scratch"),
        simulated,
        ..Launch::default()
    };
    let mut driver = nextflow_runtime::driver(&launch).expect("a driver");
    let mut executor = Simulator::new(directory.path().join("simulator"));
    let mut deliveries = Deliveries::default();
    let execution = Engine::new(&mut executor)
        .observer(&mut deliveries)
        .run("main.nf", driver.as_mut());
    assert!(execution.error.is_none(), "{:?}", execution.error);
    (deliveries.0, driver, directory)
}

#[test]
fn delivers_enabled_reports() {
    let (deliveries, driver, _directory) = run(false);
    let launch_dir = std::env::current_dir().expect("a current directory");
    let targets: Vec<String> = (deliveries.iter())
        .map(|delivery| {
            let target = std::path::Path::new(&delivery.target);
            let target = target.strip_prefix(&launch_dir).unwrap_or(target);
            target.display().to_string()
        })
        .collect();
    let expected = [
        "results/pipeline_info/execution_trace.txt",
        "results/pipeline_info/execution_timeline.html",
        "results/pipeline_info/execution_report.html",
        "results/pipeline_info/pipeline_dag.html",
    ];
    assert_eq!(targets, expected);
    let trace = std::fs::read_to_string(&deliveries[0].location).expect("the trace");
    let lines: Vec<&str> = trace.lines().collect();
    assert_eq!(
        lines[0],
        "task_id\tname\tprocess\ttag\tstatus\texit\tduration"
    );
    assert_eq!(lines.len(), 4, "{trace}");
    assert!(
        lines[1].starts_with("1\tMAKE (1)\tMAKE\t-\tCOMPLETED\t0\t"),
        "{trace}"
    );
    assert!(
        lines[3].starts_with("3\tJOIN\tJOIN\t-\tCOMPLETED\t0\t"),
        "{trace}"
    );
    let dag = std::fs::read_to_string(&deliveries[3].location).expect("the graph");
    assert!(
        dag.contains("<svg") && dag.contains("MAKE") && dag.contains("JOIN"),
        "{dag}"
    );
    let report = std::fs::read_to_string(&deliveries[2].location).expect("the report");
    assert!(
        report.contains("3 completed, 0 cached, 0 failed"),
        "{report}"
    );
    drop(driver);
    assert!(!std::path::Path::new(&deliveries[0].location).exists());
}

#[test]
fn skips_simulated_reports() {
    let (deliveries, _driver, directory) = run(true);
    assert!(deliveries.is_empty());
    assert!(!directory.path().join("scratch/nextflow").exists());
}

#[test]
fn delivers_failure_reports() {
    struct Failed;
    impl workflow_graph::Executor for Failed {
        fn run(&mut self, attempt: workflow_graph::Attempt) -> workflow_graph::Running {
            workflow_graph::Running::ended(Ok(workflow_graph::Outcome {
                id: attempt.task.id,
                exit_code: 7,
                ..Default::default()
            }))
        }
    }
    let directory = tempfile::tempdir().unwrap();
    std::fs::write(
        directory.path().join("main.nf"),
        "process FAIL {\n script:\n 'exit 7'\n}\nworkflow { FAIL() }",
    )
    .unwrap();
    std::fs::write(directory.path().join("nextflow.config"), CONFIG).unwrap();
    let mut driver = nextflow_runtime::driver(&Launch {
        target: directory.path().to_owned(),
        scratch: directory.path().join("scratch"),
        ..Launch::default()
    })
    .unwrap();
    let mut executor = Failed;
    let mut deliveries = Deliveries::default();
    let execution = Engine::new(&mut executor)
        .observer(&mut deliveries)
        .run("main.nf", driver.as_mut());
    assert!(execution.error.is_some());
    assert!(execution.failure_hook_error.is_none());
    assert_eq!(deliveries.0.len(), 4);
    let trace = std::fs::read_to_string(&deliveries.0[0].location).unwrap();
    assert!(trace.contains("FAILED\t7"), "{trace}");
}

#[test]
fn fills_measured_columns() {
    use std::time::{Duration, UNIX_EPOCH};
    use workflow_graph::{Outcome, Usage};
    let directory = tempfile::tempdir().unwrap();
    std::fs::write(
        directory.path().join("main.nf"),
        "process P {\n script:\n 'true'\n}\nworkflow { P() }",
    )
    .unwrap();
    std::fs::write(
        directory.path().join("nextflow.config"),
        "trace.enabled = true\ntrace.fields = 'task_id,%cpu,peak_rss,read_bytes,write_bytes'\n",
    )
    .unwrap();
    let mut driver = nextflow_runtime::driver(&Launch {
        target: directory.path().to_owned(),
        scratch: directory.path().join("scratch"),
        ..Launch::default()
    })
    .unwrap();
    let tasks = driver.start().unwrap();
    driver
        .complete(Outcome {
            id: tasks[0].id,
            attempt: 1,
            started: Some(UNIX_EPOCH + Duration::from_secs(1)),
            ended: Some(UNIX_EPOCH + Duration::from_secs(3)),
            usage: Some(Usage {
                cpu_usec: Some(1_000_000),
                memory_peak: Some(3 << 20),
                read_bytes: Some(0),
                write_bytes: Some(4096),
            }),
            ..Default::default()
        })
        .unwrap();
    driver.finish().unwrap();
    let deliveries = driver.deliveries();
    let trace = std::fs::read_to_string(&deliveries[0].location).unwrap();
    assert_eq!(
        trace,
        "task_id\t%cpu\tpeak_rss\tread_bytes\twrite_bytes\n1\t50.0%\t3 MB\t0\t4 KB\n"
    );
}
