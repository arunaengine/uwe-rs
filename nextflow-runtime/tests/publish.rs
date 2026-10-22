//! Nextflow's `publishDir`: which outputs of a finished task the driver asks the engine to
//! deliver, where, and how, from the process and from the config.
// Copyright (c) 2026 The uwe Contributors
// SPDX-License-Identifier: MIT or Apache-2.0

use workflow_graph::{DeliveryMode, Engine, Event, Launch, Observer, Simulator};

#[test]
fn carries_remote_guard() {
    let directory = tempfile::tempdir().unwrap();
    std::fs::write(
        directory.path().join("main.nf"),
        r#"
process MAKE {
    publishDir 's3://bucket/results', mode: 'copy', overwrite: false
    output:
    path 'result.txt'
    script:
    'touch result.txt'
}
workflow { MAKE() }
"#,
    )
    .unwrap();
    let mut driver = nextflow_runtime::driver(&Launch {
        target: directory.path().to_owned(),
        ..Launch::default()
    })
    .unwrap();
    let tasks = driver.start().unwrap();
    driver
        .complete(workflow_graph::Outcome {
            id: tasks[0].id,
            files: vec![workflow_graph::OutputFile {
                path: "result.txt".into(),
                location: "/work/result.txt".into(),
                ..Default::default()
            }],
            ..Default::default()
        })
        .unwrap();
    let deliveries = driver.deliveries();
    assert_eq!(deliveries.len(), 1);
    assert_eq!(deliveries[0].target, "s3://bucket/results/result.txt");
    assert!(!deliveries[0].overwrite);
}

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

#[test]
fn rejects_publish_options() {
    for (option, diagnostic) in [
        ("failOnError: false", "publishDir option `failOnError`"),
        ("failOnError: 'false'", "publishDir option `failOnError`"),
        ("failOnError: 1", "publishDir option `failOnError`"),
        ("failOnError: { false }", "publishDir option `failOnError`"),
        ("contentType: true", "publishDir option `contentType`"),
        (
            "contentType: 'text/plain'",
            "publishDir option `contentType`",
        ),
        ("tags: [sample: 'a']", "publishDir option `tags`"),
    ] {
        for from_config in [false, true] {
            let directory = tempfile::tempdir().unwrap();
            let directive = if from_config {
                String::new()
            } else {
                format!(" publishDir 'published', {option}\n")
            };
            let script = format!(
                "process P {{\n{directive} output:\n path 'out.txt'\n script:\n 'touch out.txt'\n}}\nworkflow {{ P() }}"
            );
            std::fs::write(directory.path().join("main.nf"), script).unwrap();
            if from_config {
                std::fs::write(
                    directory.path().join("nextflow.config"),
                    format!("process.publishDir = [path: 'published', {option}]"),
                )
                .unwrap();
            }
            let mut driver = nextflow_runtime::driver(&Launch {
                target: directory.path().to_owned(),
                ..Launch::default()
            })
            .unwrap();
            let error = driver.start().expect_err("unsupported publishing");
            assert!(error.unsupported, "{error}");
            assert!(error.to_string().contains(diagnostic), "{error}");
        }
    }
}

#[test]
fn accepts_disabled_publish() {
    for options in [
        "mode: 'copy', failOnError: true, contentType: false, tags: [:]",
        "mode: 'copy', failOnError: 'TRUE'",
        "mode: 'copy', failOnError: null, contentType: null, tags: null",
        "enabled: false, mode: 'move', failOnError: false, contentType: true, tags: [sample: 'a']",
    ] {
        let directory = tempfile::tempdir().unwrap();
        std::fs::write(directory.path().join("main.nf"), format!("process P {{\n publishDir 'published', {options}\n script:\n 'true'\n}}\nworkflow {{ P() }}")).unwrap();
        let mut driver = nextflow_runtime::driver(&Launch {
            target: directory.path().to_owned(),
            ..Launch::default()
        })
        .unwrap();
        assert_eq!(driver.start().expect("supported options").len(), 1);
    }
}

/// Notes each delivery as its target relative to the launch directory and its mode.
#[derive(Default)]
struct Deliveries(Vec<(String, DeliveryMode)>, std::path::PathBuf);

impl Observer for Deliveries {
    fn event(&mut self, event: Event<'_>) {
        if let Event::Delivered { delivery } = event {
            let target = std::path::Path::new(&delivery.target);
            let target = target.strip_prefix(&self.1).unwrap_or(target);
            self.0.push((target.display().to_string(), delivery.mode));
        }
    }
}

#[test]
fn copies_move_outputs() {
    for configured in [false, true] {
        let directory = tempfile::tempdir().unwrap();
        let script = if configured {
            PIPELINE.to_owned()
        } else {
            PIPELINE.replace("mode: 'copy'", "mode: 'move'")
        };
        let config = if configured {
            CONFIG.replace("mode: 'rellink'", "mode: 'move'")
        } else {
            CONFIG.to_owned()
        };
        std::fs::write(directory.path().join("main.nf"), script).unwrap();
        std::fs::write(directory.path().join("nextflow.config"), config).unwrap();
        let mut driver = nextflow_runtime::driver(&Launch {
            target: directory.path().to_owned(),
            simulated: true,
            ..Launch::default()
        })
        .unwrap();
        let mut executor = Simulator::new(directory.path().join("simulator"));
        let mut deliveries = Deliveries(Vec::new(), std::env::current_dir().unwrap());
        let execution = Engine::new(&mut executor)
            .observer(&mut deliveries)
            .run("main.nf", driver.as_mut());
        assert!(execution.error.is_none(), "{:?}", execution.error);
        let target = if configured {
            "results/configured/copy.txt"
        } else {
            "results/made/a.txt"
        };
        assert!(deliveries.0.contains(&(target.into(), DeliveryMode::Copy)));
        assert_eq!(execution.graph.nodes.len(), 2);
    }
}

#[test]
fn publishes_directed_outputs() {
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
fn excludes_staged_inputs() {
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
fn publishes_workflow_outputs() {
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

#[test]
fn excludes_work_directory() {
    let directory = tempfile::tempdir().expect("a temporary directory");
    // nf-core's EPANG_PLACE with the default publishDir of nf-core pipelines: Nextflow gives
    // the work directory the empty name, which saveAs turns down.
    let script = r#"
process PLACE {
    publishDir 'published', saveAs: { filename -> filename.equals('versions.yml') ? null : filename }
    output:
    path("./."), emit: all
    path "*.log", emit: log
    script:
    """
    echo log > x.log
    """
}

workflow {
    PLACE()
}
"#;
    std::fs::write(directory.path().join("main.nf"), script).expect("writes the script");
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
    let targets: Vec<&str> = deliveries
        .0
        .iter()
        .map(|(target, _)| target.as_str())
        .collect();
    assert_eq!(targets, ["published/x.log"]);
}
