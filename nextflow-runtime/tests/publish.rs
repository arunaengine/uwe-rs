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

/// Gives each task back its inputs and a file `made.txt`, as a glob in a real work directory
/// finds them.
struct WithInputs(std::path::PathBuf);

impl workflow_graph::Executor for WithInputs {
    fn run(&mut self, attempt: workflow_graph::Attempt) -> workflow_graph::Running {
        let task = &attempt.task;
        let mut files = Vec::new();
        let names = task.inputs.iter().map(|input| input.path.clone());
        for name in names.chain(["made.txt".to_owned()]) {
            let location = self.0.join(format!("{}-{name}", task.id));
            std::fs::write(&location, "x").expect("writes");
            files.push(workflow_graph::OutputFile {
                path: name,
                location: location.to_string_lossy().into_owned(),
                ..Default::default()
            });
        }
        workflow_graph::Running::ended(Ok(workflow_graph::Outcome {
            id: task.id,
            files,
            ..Default::default()
        }))
    }
}

#[test]
fn globs_leave_out_the_staged_inputs() {
    let directory = tempfile::tempdir().expect("a temporary directory");
    let script = r#"
process TAKE {
    publishDir 'published', mode: 'copy'
    input:
    path text
    output:
    path '*.txt'
    path 'in.txt', emit: named
    script:
    "touch made.txt"
}

workflow {
    TAKE(file('in.txt'))
}
"#;
    std::fs::write(directory.path().join("main.nf"), script).expect("writes the script");
    std::fs::write(directory.path().join("in.txt"), "in\n").expect("writes the input");
    let launch = Launch {
        target: directory.path().join("main.nf"),
        ..Launch::default()
    };
    let mut driver = nextflow_runtime::driver(&launch).expect("a driver");
    let mut executor = WithInputs(directory.path().to_owned());
    let mut deliveries = Deliveries(Vec::new(), std::env::current_dir().expect("a directory"));
    let execution = Engine::new(&mut executor)
        .observer(&mut deliveries)
        .run("main.nf", driver.as_mut());
    assert!(execution.error.is_none(), "{:?}", execution.error);
    // The glob takes only what the task made; the name of an input takes it.
    let targets: Vec<&str> = deliveries
        .0
        .iter()
        .map(|(target, _)| target.as_str())
        .collect();
    assert_eq!(targets, ["published/made.txt", "published/in.txt"]);
}

const OUTPUTS: &str = r#"
process MAKE {
    input:
    val id
    output:
    tuple val(id), path("${id}.txt"), path("${id}.log")
    script:
    "touch ${id}.txt ${id}.log"
}

workflow {
    main:
    made = MAKE(channel.of('a'))
    publish:
    texts = made.map { id, text, log -> [id: id, text: text] }
    logs = made
}

output {
    texts {
        path 'texts'
        index {
            path 'texts.csv'
            header true
        }
    }
    logs {
        path { id, text, log -> log >> "logs/${id}/" }
    }
}
"#;

/// Makes each output of a task as a file named after it.
struct Maker(std::path::PathBuf);

impl workflow_graph::Executor for Maker {
    fn run(&mut self, attempt: workflow_graph::Attempt) -> workflow_graph::Running {
        let task = &attempt.task;
        let files = (task.outputs.iter())
            .map(|name| {
                let location = self.0.join(name);
                std::fs::write(&location, "x").expect("writes");
                workflow_graph::OutputFile {
                    path: name.clone(),
                    location: location.to_string_lossy().into_owned(),
                    ..Default::default()
                }
            })
            .collect();
        workflow_graph::Running::ended(Ok(workflow_graph::Outcome {
            id: task.id,
            files,
            ..Default::default()
        }))
    }
}

#[test]
fn workflow_outputs_go_below_the_output_directory() {
    let directory = tempfile::tempdir().expect("a temporary directory");
    std::fs::write(directory.path().join("main.nf"), OUTPUTS).expect("writes the script");
    let results = directory.path().join("results");
    let launch = Launch {
        target: directory.path().join("main.nf"),
        output_dir: Some(results.clone()),
        ..Launch::default()
    };
    let made = directory.path().join("made");
    std::fs::create_dir(&made).expect("makes the folder");
    let mut driver = nextflow_runtime::driver(&launch).expect("a driver");
    let mut deliveries = Deliveries(Vec::new(), results.clone());
    let execution = Engine::new(&mut Maker(made))
        .observer(&mut deliveries)
        .run("main.nf", driver.as_mut());
    assert!(execution.error.is_none(), "{:?}", execution.error);
    let mut targets: Vec<&str> = deliveries
        .0
        .iter()
        .map(|(target, _)| target.as_str())
        .collect();
    targets.sort_unstable();
    assert_eq!(targets, ["logs/a/a.log", "texts/a.txt"]);
    // The index names the files at their new places.
    let index = std::fs::read_to_string(results.join("texts.csv")).expect("an index");
    let text = results.join("texts/a.txt");
    assert_eq!(
        index,
        format!("\"id\",\"text\"\n\"a\",\"{}\"\n", text.display())
    );
}
