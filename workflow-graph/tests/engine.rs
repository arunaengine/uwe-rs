//! A toy driver runs through the engine and the simulator, and the graph links tasks by
//! the files they pass. The simulator makes the files that a task's script names.
// Copyright (c) 2026 The uwe Contributors
// SPDX-License-Identifier: MIT or Apache-2.0

use workflow_graph::render::{Options, View};
use workflow_graph::simulator::{EMPTY_GZIP, file_name, glob_matches};
use workflow_graph::{
    Attempt, Driver, DriverError, EdgeKind, Engine, Event, Executor, Input, Language, MORE_TASKS,
    Marker, MarkerKind, Note, Observer, Outcome, Reuse, Running, SIMULATION, Simulator, Task,
    TaskId, execute,
};

/// Two `split` tasks, then one `merge` task that reads both outputs.
#[derive(Default)]
struct Toy {
    finished: Vec<Outcome>,
}

impl Driver for Toy {
    fn language(&self) -> Language {
        Language::Snakemake
    }

    fn start(&mut self) -> Result<Vec<Task>, DriverError> {
        Ok((1..=2)
            .map(|n| Task {
                id: TaskId(n),
                name: format!("split {n}"),
                step: "split".into(),
                image: Some("ubuntu:24.04".into()),
                command: vec![
                    "bash".into(),
                    "-c".into(),
                    format!("echo {n} > part{n}.txt"),
                ],
                script: Some(format!("echo {n} > part{n}.txt")),
                outputs: vec!["part*.txt".into()],
                ..Task::default()
            })
            .collect())
    }

    fn complete(&mut self, outcome: Outcome) -> Result<Vec<Task>, DriverError> {
        self.finished.push(outcome);
        if self.finished.len() != 2 {
            return Ok(Vec::new());
        }
        let inputs = self
            .finished
            .iter()
            .flat_map(|outcome| &outcome.files)
            .map(|file| Input {
                path: file.path.clone(),
                source: file.location.clone(),
                directory: false,
                content: None,
                ..Default::default()
            })
            .collect();
        Ok(vec![Task {
            id: TaskId(3),
            name: "merge".into(),
            step: "merge".into(),
            command: vec!["cat".into()],
            inputs,
            outputs: vec!["merged/".into()],
            ..Task::default()
        }])
    }

    fn finish(&mut self) -> Result<(), DriverError> {
        Ok(())
    }
}

#[test]
fn links_task_files() {
    let directory = tempfile::tempdir().expect("a temporary directory");
    let mut executor = Simulator::new(directory.path());
    let graph = execute("toy", &mut Toy::default(), &mut executor)
        .into_result()
        .expect("the run succeeds");
    assert_eq!(graph.nodes.len(), 3);
    assert_eq!(graph.steps(), ["split", "merge"]);
    graph.check().expect("a valid graph");
    let into_merge: Vec<_> = graph
        .edges
        .iter()
        .filter(|edge| edge.to == TaskId(3))
        .collect();
    assert_eq!(into_merge.len(), 2);
    assert_eq!(
        into_merge[0].kind,
        EdgeKind::File {
            path: "part1.txt".into()
        }
    );
    assert!(directory.path().join("work/1/part1.txt").is_file());
    assert!(directory.path().join("work/3/merged").is_dir());
    let tes: serde_json::Value = serde_json::from_str(
        &std::fs::read_to_string(directory.path().join("tes/1.json")).expect("a TES document"),
    )
    .expect("JSON");
    assert_eq!(tes["executors"][0]["image"], "ubuntu:24.04");
    assert!(graph.to_dot().contains("t1 -> t3"));
}

#[test]
fn shows_simulated_containers() {
    let directory = tempfile::tempdir().expect("a temporary directory");
    let simulator = Simulator::new(directory.path());
    let container = |name: &str, image: &str| workflow_graph::Container {
        name: name.into(),
        image: Some(image.into()),
        command: vec!["echo".into(), name.into()],
        attempts: 1,
        ..workflow_graph::Container::default()
    };
    let task = Task {
        id: TaskId(7),
        name: "set".into(),
        image: Some("alpine".into()),
        command: vec!["echo".into(), "main".into()],
        containers: vec![container("main", "alpine"), container("first", "debian")],
        ..Task::default()
    };
    simulator.simulate(&task).expect("an outcome");
    let text = std::fs::read_to_string(directory.path().join("tes/7.json")).expect("a document");
    let tes: serde_json::Value = serde_json::from_str(&text).expect("JSON");
    assert_eq!(tes["executors"][1]["image"], "debian");
    assert_eq!(tes["executors"][1]["command"][1], "first");
}

#[test]
fn isolates_simulated_attempts() {
    let directory = tempfile::tempdir().unwrap();
    let mut simulator = Simulator::new(directory.path());
    let mut files = Vec::new();
    for (number, word) in [(1, "first"), (2, "second")] {
        let task = Task {
            id: TaskId(1),
            script: Some(format!("echo {word} > result.txt")),
            outputs: vec!["result.txt".into()],
            ..Task::default()
        };
        let running = simulator.run(Attempt {
            task: std::sync::Arc::new(task),
            number,
            cancel: workflow_graph::CancellationToken::new(),
        });
        let outcome = workflow_graph::runtime()
            .unwrap()
            .block_on(running)
            .unwrap();
        files.push(outcome.files[0].clone());
    }
    assert_ne!(files[0].location, files[1].location);
    assert_ne!(files[0].digest, files[1].digest);
    assert_eq!(
        std::fs::read_to_string(&files[0].location).unwrap(),
        "first\n"
    );
    assert_eq!(
        std::fs::read_to_string(&files[1].location).unwrap(),
        "second\n"
    );
}

/// The simulator, except that an earlier result answers task 2.
struct Reusing(Simulator);

impl Executor for Reusing {
    fn run(&mut self, attempt: Attempt) -> Running {
        let cached = attempt.task.id == TaskId(2);
        let outcome = self.0.simulate(&attempt.task);
        Running::ended(outcome.map(|outcome| Outcome { cached, ..outcome }))
    }
}

/// The simulator, which keeps the token of each attempt.
struct Keeping(Simulator, Vec<tokio_util::sync::CancellationToken>);

impl Executor for Keeping {
    fn run(&mut self, attempt: Attempt) -> Running {
        self.1.push(attempt.cancel.clone());
        Running::ended(self.0.simulate(&attempt.task))
    }
}

#[test]
fn cancels_ended_tasks() {
    let directory = tempfile::tempdir().expect("a temporary directory");
    let mut executor = Keeping(Simulator::new(directory.path()), Vec::new());
    execute("toy", &mut Toy::default(), &mut executor)
        .into_result()
        .expect("the run succeeds");
    assert_eq!(executor.1.len(), 3);
    assert!(
        executor.1.iter().all(|cancel| cancel.is_cancelled()),
        "work that waits for the token of an ended task stops"
    );
}

#[test]
fn records_reused_tasks() {
    let directory = tempfile::tempdir().expect("a temporary directory");
    let mut executor = Reusing(Simulator::new(directory.path()));
    let graph = execute("toy", &mut Toy::default(), &mut executor)
        .into_result()
        .expect("the run succeeds");
    let reused: Vec<bool> = graph.nodes.iter().map(|node| node.reused).collect();
    assert_eq!(reused, [false, true, false]);
    let json = serde_json::to_string(&graph).expect("JSON");
    assert_eq!(json.matches("\"reused\":true").count(), 1, "{json}");
    assert!(
        !json.contains("\"reused\":false"),
        "only reused tasks name it: {json}"
    );
}

#[test]
fn matches_shell_globs() {
    assert!(glob_matches("*.txt", "a.txt"));
    assert!(!glob_matches("*.txt", "d/a.txt"));
    assert!(glob_matches("**/*.txt", "d/e/a.txt"));
    assert!(glob_matches("*.{html,zip}", "r.zip"));
    assert!(glob_matches("s[0-9].bam", "s3.bam"));
    // The name of a placeholder file matches its pattern, which for `**/` needs a directory.
    assert_eq!(file_name("**/*.gz"), "simulated/simulated.gz");
    assert!(glob_matches("**/*.gz", &file_name("**/*.gz")));
}

#[test]
fn confines_simulated_outputs() {
    let directory = tempfile::tempdir().unwrap();
    let simulator = Simulator::new(directory.path().join("sim"));
    for (index, output) in ["../../../escaped.txt", "dir/../../../../escaped.txt"]
        .into_iter()
        .enumerate()
    {
        let task = Task {
            id: TaskId(index as u64),
            outputs: vec![output.into()],
            ..Task::default()
        };
        assert!(simulator.simulate(&task).is_err());
    }
    assert!(!directory.path().join("escaped.txt").exists());
    let task = Task {
        id: TaskId(3),
        outputs: vec!["/absolute/out.txt".into()],
        ..Task::default()
    };
    let outcome = simulator.simulate(&task).unwrap();
    assert!(
        std::path::Path::new(&outcome.files[0].location)
            .starts_with(simulator.work_directory(&task))
    );
}

#[test]
fn rejects_existing_symlinks() {
    let directory = tempfile::tempdir().unwrap();
    let simulator = Simulator::new(directory.path().join("sim"));
    let outside = directory.path().join("outside");
    std::fs::create_dir(&outside).unwrap();
    for literal in [false, true] {
        let mut task = Task {
            id: TaskId(u64::from(literal)),
            outputs: vec!["link/out.txt".into()],
            ..Task::default()
        };
        if literal {
            task.inputs.push(Input {
                path: "link/out.txt".into(),
                content: Some("changed".into()),
                ..Input::default()
            });
        }
        let work = simulator.work_directory(&task);
        std::fs::create_dir_all(&work).unwrap();
        std::os::unix::fs::symlink(&outside, work.join("link")).unwrap();
        assert!(simulator.simulate(&task).is_err());
        assert!(!outside.join("out.txt").exists());
    }
}

#[test]
fn makes_script_files() {
    let directory = tempfile::tempdir().expect("a temporary directory");
    let script = "touch s_1.fq s_2.fq\nmkdir -p db\ntouch db/x.txt\ncp in.txt copy.txt\n\
                  echo v > versions.yml";
    let input = |path: &str, content: Option<&str>| Input {
        path: path.into(),
        source: format!("/data/{path}"),
        directory: false,
        content: content.map(str::to_owned),
        ..Default::default()
    };
    let task = Task {
        id: TaskId(7),
        name: "TOOL (s)".into(),
        step: "NFCORE:TOOL".into(),
        script: Some(script.into()),
        inputs: vec![input(".command.sh", Some("true\n")), input("in.txt", None)],
        outputs: [
            "*.fq",
            "db",
            "copy.txt",
            "versions.yml",
            "*.vcf.gz",
            "report/",
        ]
        .map(String::from)
        .to_vec(),
        ..Task::default()
    };
    let outcome = Simulator::new(directory.path())
        .simulate(&task)
        .expect("an outcome");
    let paths: Vec<&str> = outcome
        .files
        .iter()
        .map(|file| file.path.as_str())
        .collect();
    // Outputs that the script does not tell how it makes are made up.
    assert_eq!(
        paths,
        [
            "s_1.fq",
            "s_2.fq",
            "db",
            "copy.txt",
            "versions.yml",
            "simulated.vcf.gz",
            "report"
        ]
    );
    let work = directory.path().join("work/7");
    assert!(work.join("db/x.txt").is_file());
    assert!(outcome.files[2].directory);
    let read = |name: &str| std::fs::read(work.join(name)).expect("reads a file");
    assert_eq!(
        read("versions.yml"),
        b"\"NFCORE:TOOL\":\n    simulated: 1.0.0\n"
    );
    // Inputs with content are written, as a TES server places them.
    assert_eq!(read(".command.sh"), b"true\n");
    assert!(!work.join("in.txt").exists());
    // A tool may make any output, which then is one named after the pattern.
    let task = Task {
        id: TaskId(8),
        script: Some("tool --out result\ntouch s_1.fq".into()),
        ..task
    };
    let outcome = Simulator::new(directory.path())
        .simulate(&task)
        .expect("an outcome");
    let paths: Vec<&str> = outcome
        .files
        .iter()
        .map(|file| file.path.as_str())
        .collect();
    assert_eq!(
        paths,
        [
            "s_1.fq",
            "db",
            "copy.txt",
            "versions.yml",
            "simulated.vcf.gz",
            "report"
        ]
    );
    let work = directory.path().join("work/8");
    assert_eq!(
        std::fs::read(work.join("simulated.vcf.gz")).expect("reads"),
        EMPTY_GZIP
    );
    assert!(outcome.files[5].directory);
}

#[test]
fn writes_stub_text() {
    let directory = tempfile::tempdir().expect("a temporary directory");
    let script = "cat <<-END > s.flagstat\n900 + 0 mapped (90.00% : N/A)\nEND\n\
                  touch s.lf.{drp,drs}\necho '' | gzip > s.fq.gz\ntouch versions.yml";
    let task = Task {
        id: TaskId(9),
        script: Some(script.into()),
        outputs: [
            "*.flagstat",
            "s.lf.*",
            "*.fq.gz",
            "*.unaligned.fasta",
            "versions.yml",
        ]
        .map(String::from)
        .to_vec(),
        ..Task::default()
    };
    let outcome = Simulator::new(directory.path())
        .simulate(&task)
        .expect("an outcome");
    let paths: Vec<&str> = outcome
        .files
        .iter()
        .map(|file| file.path.as_str())
        .collect();
    assert_eq!(
        paths,
        [
            "s.flagstat",
            "s.lf.drp",
            "s.lf.drs",
            "s.fq.gz",
            "simulated.unaligned.fasta",
            "versions.yml"
        ]
    );
    let work = directory.path().join("work/9");
    let read = |name: &str| std::fs::read(work.join(name)).expect("reads a file");
    assert_eq!(read("s.flagstat"), b"900 + 0 mapped (90.00% : N/A)\n");
    // A versions file that the stub touches stays empty, as in Nextflow.
    assert_eq!(read("versions.yml"), b"");
    let mut text = String::new();
    std::io::Read::read_to_string(
        &mut flate2::read::GzDecoder::new(read("s.fq.gz").as_slice()),
        &mut text,
    )
    .expect("a gzip file");
    assert_eq!(text, "\n");
}

#[test]
fn makes_argument_files() {
    let directory = tempfile::tempdir().expect("a temporary directory");
    let command = [
        "abra",
        "--in",
        "/in/a.bam",
        "--out",
        "a_IR.bam,b_IR.bam",
        "--log=run.log",
    ];
    let task = Task {
        id: TaskId(3),
        command: command.map(String::from).to_vec(),
        outputs: ["*_IR.bam", "*.log", "*.txt"].map(String::from).to_vec(),
        ..Task::default()
    };
    let outcome = Simulator::new(directory.path())
        .simulate(&task)
        .expect("an outcome");
    let paths: Vec<&str> = outcome
        .files
        .iter()
        .map(|file| file.path.as_str())
        .collect();
    assert_eq!(paths, ["a_IR.bam", "b_IR.bam", "run.log", "simulated.txt"]);
}

/// A script that surely fails, like one that gzips a file onto its input, ends so: the simulator
/// gives the exit status and message of the command, and no files.
#[test]
fn reports_script_failures() {
    let directory = tempfile::tempdir().expect("a temporary directory");
    let task = Task {
        id: TaskId(4),
        script: Some("touch s.fastq\ngzip s.fastq\ntouch s.log".into()),
        inputs: vec![Input {
            path: "s.fastq.gz".into(),
            source: "/data/s.fastq.gz".into(),
            directory: false,
            content: None,
            ..Default::default()
        }],
        outputs: ["*.fastq.gz", "*.log"].map(String::from).to_vec(),
        ..Task::default()
    };
    let outcome = Simulator::new(directory.path())
        .simulate(&task)
        .expect("an outcome");
    assert_eq!(outcome.exit_code, 2);
    assert_eq!(
        outcome.stderr,
        "gzip: s.fastq.gz already exists;\tnot overwritten"
    );
    assert!(outcome.files.is_empty());
}

/// An output of a tool that the simulation does not know is named after the arguments of the
/// tool that match its pattern, as the tool would name it, and is a directory when the pattern
/// ends with `/`. A folder argument like `res/` names no file.
#[test]
fn infers_tool_outputs() {
    let directory = tempfile::tempdir().expect("a temporary directory");
    let task = Task {
        id: TaskId(5),
        script: Some(
            "mkdir output\nFeatureFinderMetabo -in s.mzML -out output/s.featureXML -oc report \
             -d res/"
                .into(),
        ),
        inputs: vec![Input {
            path: "s.mzML".into(),
            source: "/data/s.mzML".into(),
            directory: false,
            content: None,
            ..Default::default()
        }],
        outputs: ["output/*.featureXML", "*.log", "report/", "res/*"]
            .map(String::from)
            .to_vec(),
        ..Task::default()
    };
    let outcome = Simulator::new(directory.path())
        .simulate(&task)
        .expect("an outcome");
    let paths: Vec<&str> = outcome
        .files
        .iter()
        .map(|file| file.path.as_str())
        .collect();
    assert_eq!(
        paths,
        [
            "output/s.featureXML",
            "simulated.log",
            "report",
            "res/simulated"
        ]
    );
    assert!(outcome.files[2].directory);
}

/// One task that makes `part1.txt`, `part2.txt`, `list.csv` and `log1.txt`; its driver reads
/// `list.csv`, spreads the parts into one task each, spreads the log into no task, and marks
/// the task itself.
#[derive(Default)]
struct Noting {
    notes: Vec<Note>,
}

impl Driver for Noting {
    fn language(&self) -> Language {
        Language::Nextflow
    }

    fn start(&mut self) -> Result<Vec<Task>, DriverError> {
        Ok(vec![Task {
            id: TaskId(1),
            name: "SPLIT".into(),
            step: "SPLIT".into(),
            script: Some(
                "echo a > part1.txt\necho b > part2.txt\necho x > list.csv\ntouch log1.txt".into(),
            ),
            outputs: vec!["part*.txt".into(), "list.csv".into(), "log*.txt".into()],
            ..Task::default()
        }])
    }

    fn complete(&mut self, outcome: Outcome) -> Result<Vec<Task>, DriverError> {
        if outcome.id != TaskId(1) {
            return Ok(Vec::new());
        }
        let location = |name: &str| {
            let file = outcome.files.iter().find(|file| file.path == name);
            std::path::PathBuf::from(&file.expect("an output").location)
        };
        self.notes = vec![
            Note::Read(location("list.csv")),
            // A literal output names one file, and a file of no task marks nothing.
            Note::Spread(location("list.csv")),
            Note::Read("/data/samples.csv".into()),
            Note::Spread(location("part1.txt")),
            Note::Spread(location("part2.txt")),
            // No task of this call reads the log, as when all logs go into one report.
            Note::Spread(location("log1.txt")),
            Note::Mark(
                TaskId(1),
                Marker::new(MarkerKind::Checkpoint, "a checkpoint"),
            ),
        ];
        Ok(["part1.txt", "part2.txt"]
            .iter()
            .enumerate()
            .map(|(index, name)| Task {
                id: TaskId(2 + index as u64),
                name: format!("COUNT ({name})"),
                step: "COUNT".into(),
                inputs: vec![Input {
                    path: (*name).into(),
                    source: location(name).to_string_lossy().into_owned(),
                    directory: false,
                    content: None,
                    ..Default::default()
                }],
                ..Task::default()
            })
            .collect())
    }

    fn finish(&mut self) -> Result<(), DriverError> {
        Ok(())
    }

    fn notes(&mut self) -> Vec<Note> {
        std::mem::take(&mut self.notes)
    }
}

/// The markers that the engine reports to an observer.
#[derive(Default)]
struct Marks(Vec<(TaskId, MarkerKind)>);

impl Observer for Marks {
    fn event(&mut self, event: Event<'_>) {
        if let Event::Marked { task, marker } = event {
            self.0.push((task, marker.kind));
        }
    }
}

#[test]
fn marks_output_tasks() {
    let directory = tempfile::tempdir().expect("a temporary directory");
    let mut executor = Simulator::new(directory.path());
    let mut marks = Marks::default();
    let mut graph = Engine::new(&mut executor)
        .observer(&mut marks)
        .run("noting", &mut Noting::default())
        .into_result()
        .expect("the run succeeds");
    // The observer hears each new marker once.
    let kinds = [
        MarkerKind::Content,
        MarkerKind::Wildcard,
        MarkerKind::Checkpoint,
    ];
    assert_eq!(marks.0, kinds.map(|kind| (TaskId(1), kind)));
    graph.simulate();
    let split = graph.node(TaskId(1)).expect("the first task");
    let kinds: Vec<MarkerKind> = split.markers.iter().map(|marker| marker.kind).collect();
    assert_eq!(
        kinds,
        [
            MarkerKind::Content,
            MarkerKind::Wildcard,
            MarkerKind::Checkpoint
        ]
    );
    assert_eq!(
        split.markers[0].reason,
        "the workflow read `list.csv`, an output of this task, to plan the next tasks"
    );
    assert_eq!(
        split.markers[1].reason,
        "the output pattern `part*.txt` matched 2 files, and each started tasks of its own"
    );
    assert_eq!(graph.marked().count(), 1);
    // The step graph counts tasks and carries the markers of its steps.
    let steps = graph.step_graph();
    assert_eq!(steps.steps.len(), 2);
    assert_eq!(
        (steps.steps[1].name.as_str(), steps.steps[1].tasks),
        ("COUNT", 2)
    );
    assert_eq!(
        (steps.steps[0].marked, steps.steps[0].markers.len()),
        (1, 3)
    );
    assert_eq!((steps.edges.len(), steps.edges[0].tasks), (1, 2));
    // Every format says that the graph is simulated and shows the open point after SPLIT.
    let summary = graph.summary();
    assert!(summary.starts_with(SIMULATION), "{summary}");
    assert!(
        summary
            .contains("3 tasks, 2 steps, 2 edges, 1 point where real data can add or remove tasks"),
        "{summary}"
    );
    assert!(
        summary.contains(&format!("      {MORE_TASKS}: a checkpoint\n")),
        "{summary}"
    );
    let dot = graph.to_dot();
    assert!(dot.contains("t1 -> m1 [style=dashed];"), "{dot}");
    let svg = graph.to_svg();
    assert!(svg.contains("id=\"m1\""), "{svg}");
    let steps = Options {
        view: View::Steps,
        ..Options::default()
    };
    assert_eq!(graph.drawing(&steps).shapes.len(), 3);
    // A graph of a run that was not simulated keeps its markers, but has no open points.
    graph.simulation = None;
    assert!(!graph.to_dot().contains("m1"));
    assert!(
        graph
            .summary()
            .contains("the next tasks depend on its outputs: a checkpoint")
    );
}

/// One task per step, each with the reuse its workflow gave it.
struct Steps(Vec<(&'static str, Reuse)>);

impl Driver for Steps {
    fn language(&self) -> Language {
        Language::Nextflow
    }

    fn start(&mut self) -> Result<Vec<Task>, DriverError> {
        Ok((self.0.iter().zip(1..))
            .map(|((step, reuse), id)| Task {
                id: TaskId(id),
                name: format!("{step} (1)"),
                step: (*step).into(),
                command: vec!["true".into()],
                reuse: *reuse,
                ..Task::default()
            })
            .collect())
    }

    fn complete(&mut self, _outcome: Outcome) -> Result<Vec<Task>, DriverError> {
        Ok(Vec::new())
    }

    fn finish(&mut self) -> Result<(), DriverError> {
        Ok(())
    }
}

/// Keeps the step and reuse of each task that it runs.
#[derive(Default)]
struct Seen(Vec<(String, Reuse)>);

impl Executor for Seen {
    fn run(&mut self, attempt: Attempt) -> Running {
        self.0.push((attempt.task.step.clone(), attempt.task.reuse));
        Running::ended(Ok(Outcome {
            id: attempt.task.id,
            exit_code: 0,
            ..Outcome::default()
        }))
    }
}

#[test]
fn never_caches_steps() {
    let steps = [
        ("NFCORE:DEMO:FASTQC", Reuse::Allowed),
        ("align/sort", Reuse::Denied),
        ("MULTIQC", Reuse::Allowed),
        ("FASTQC_EXTRA", Reuse::Allowed),
        ("collect", Reuse::Disabled),
    ];
    let mut executor = Seen::default();
    let graph = Engine::new(&mut executor)
        .no_cache_steps(vec!["FASTQC".into(), "sort".into(), "NFCORE".into()])
        .run("main.nf", &mut Steps(steps.to_vec()))
        .into_result()
        .expect("the run succeeds");
    // A named step neither reads nor writes the cache; the others keep what their workflow says.
    let expected = [
        ("NFCORE:DEMO:FASTQC", Reuse::Disabled),
        ("align/sort", Reuse::Disabled),
        ("MULTIQC", Reuse::Allowed),
        ("FASTQC_EXTRA", Reuse::Allowed),
        ("collect", Reuse::Disabled),
    ];
    let expected: Vec<_> = (expected.iter())
        .map(|(step, reuse)| (step.to_string(), *reuse))
        .collect();
    assert_eq!(executor.0, expected);
    let recorded: Vec<_> = (graph.nodes.iter())
        .map(|node| (node.task.step.clone(), node.task.reuse))
        .collect();
    assert_eq!(recorded, expected, "the graph records what ran");
    let mut plain = Seen::default();
    Engine::new(&mut plain).run("main.nf", &mut Steps(steps.to_vec()));
    let given: Vec<_> = (steps.iter())
        .map(|(step, reuse)| (step.to_string(), *reuse))
        .collect();
    assert_eq!(plain.0, given, "without rules every task keeps its reuse");
}
