//! Compiles Snakemake workflows into wir, runs each natively and compiled on the simulator, and
//! compares their tasks, cache keys, resume fingerprints, acceptance and target files.
// Copyright (c) 2026 The uwe Contributors
// SPDX-License-Identifier: MIT or Apache-2.0

use std::collections::{BTreeMap, HashMap, VecDeque};
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::Arc;

use serde_json::Value as Json;
use snakemake_runtime::{
    Answer, JobKey, JobResult, Prepared, RunSettings, Settings, SnakemakeDriver,
};
use wir::{Document, Kind, RuleTask, TaskBuilder};
use workflow_graph::{
    Delivery, Driver, DriverError, ExecutionGraph, Files, Language, Launch, Note, Order, Outcome,
    RunOptions, Simulator, Task, TaskId, execute_with,
};

/// The counts of `just snakemake-test-workflows`: native runs, how workflows compile, and
/// compiled runs that succeed, end as the native run, and give its tasks and target files.
const RECORDED: Counts = Counts {
    workflows: 890,
    native: 633,
    full: 565,
    partly: 110,
    failed: 214,
    compiled: 545,
    ends: 564,
    same: 545,
    equal: 545,
};

#[derive(Debug, Default, PartialEq)]
struct Counts {
    workflows: usize,
    native: usize,
    full: usize,
    partly: usize,
    failed: usize,
    compiled: usize,
    ends: usize,
    same: usize,
    equal: usize,
}

const LAUNCHES: &str =
    include_str!("../../snakemake-runtime/tests/fixtures/run_test_workflows_launches.txt");

const SNAKEFILE: &str = r#"
SAMPLES = ["a", "b"]

rule all:
    input: expand("counts/{s}.txt", s=SAMPLES), "summary.txt"

rule make:
    output: temp("raw/{s}.txt")
    params: word=lambda wildcards: wildcards.s.upper()
    shell: "echo {params.word} > {output}"

rule count:
    input: "raw/{s}.txt"
    output: "counts/{s}.txt"
    log: "logs/{s}.log"
    retries: 1
    shell: "wc -c {input} > {output} 2> {log}"

rule summary:
    input: expand("counts/{s}.txt", s=SAMPLES)
    output: protected("summary.txt")
    threads: 2
    resources: mem_mb=100
    run:
        with open(output[0], "w") as handle:
            handle.write(str(len(input)))
"#;

/// A dialect task op of a compiled document: its job, and where its inputs come from.
struct JobOp {
    rule: RuleTask,
    key: JobKey,
    /// Each input port with its native selector and the op and port that feed it.
    inputs: Vec<(String, usize, String)>,
    /// The port of each native output selector.
    outputs: BTreeMap<String, String>,
}

/// The call of the adapter that commands of workflow code hold up.
#[derive(Clone, Copy, Debug)]
enum Call {
    Start,
    Prepare(usize),
    Take(usize),
}

/// Runs the dialect tasks of a compiled document through the job adapter, each once the ops
/// that feed it have ended, as a wir run of dialect tasks does.
struct Runner {
    adapter: SnakemakeDriver,
    ops: Vec<JobOp>,
    values: Vec<Option<BTreeMap<String, Json>>>,
    /// The ops that each op feeds, how many feeding ops each op waits for, and the ops that
    /// wait for none and have not started.
    consumers: Vec<Vec<usize>>,
    waiting: Vec<usize>,
    queue: Vec<usize>,
    owners: HashMap<TaskId, usize>,
    /// The call that waits for commands, and the outcomes of jobs that came meanwhile.
    held: Option<Call>,
    parked: VecDeque<Outcome>,
}

impl Runner {
    /// The runner of a document, with the launch of its jobs.
    fn new(document: &Document, base: &Path, scratch: &Path) -> Result<Runner, String> {
        let graph = (document.graphs.iter())
            .find(|graph| graph.name == document.entry)
            .ok_or("no entry graph")?;
        let index: HashMap<&str, usize> = (graph.ops.iter().enumerate())
            .map(|(position, op)| (op.path.as_str(), position))
            .collect();
        let mut launch = None;
        let mut ops = Vec::new();
        for op in &graph.ops {
            let Kind::Task(task) = &op.kind else {
                return Err(format!("op {} is no task", op.path));
            };
            let Some(TaskBuilder::SnakemakeJob(rule)) = &task.builder else {
                return Err(format!("op {} is no Snakemake job", op.path));
            };
            let reference = &rule.task;
            launch = Some(snakemake_compiler::job_launch(document, rule, base)?);
            let mut inputs = Vec::new();
            for map in &reference.inputs {
                let edge = (graph.edges.iter())
                    .find(|edge| {
                        edge.to.op.as_deref() == Some(op.path.as_str()) && edge.to.port == map.port
                    })
                    .ok_or_else(|| format!("no edge to {}", map.port))?;
                let from = index[edge.from.op.as_deref().unwrap_or_default()];
                inputs.push((map.native.clone(), from, edge.from.port.clone()));
            }
            let outputs = (reference.outputs.iter())
                .map(|map| (map.native.clone(), map.port.clone()))
                .collect();
            ops.push(JobOp {
                rule: rule.clone(),
                key: snakemake_compiler::job_key(rule),
                inputs,
                outputs,
            });
        }
        let launch = Launch {
            scratch: scratch.to_owned(),
            simulated: true,
            ..launch.ok_or("no job")?
        };
        let run = RunSettings {
            workdir: Some(base.to_owned()),
            ..RunSettings::default()
        };
        let adapter = snakemake_runtime::spawn_driver(&launch, Settings::default(), run)
            .map_err(|error| error.message)?;
        let count = ops.len();
        let mut consumers = vec![Vec::new(); count];
        let mut waiting = vec![0; count];
        for (index, op) in ops.iter().enumerate() {
            let mut feeding: Vec<usize> = op.inputs.iter().map(|(_, from, _)| *from).collect();
            feeding.sort_unstable();
            feeding.dedup();
            waiting[index] = feeding.len();
            for from in feeding {
                consumers[from].push(index);
            }
        }
        let queue = (0..count)
            .rev()
            .filter(|index| waiting[*index] == 0)
            .collect();
        Ok(Runner {
            adapter,
            ops,
            values: vec![None; count],
            consumers,
            waiting,
            queue,
            owners: HashMap::new(),
            held: None,
            parked: VecDeque::new(),
        })
    }

    /// Takes what a call gave, then prepares the ops that wait for nothing, in the order of
    /// the document, and takes the outcomes that came while commands ran.
    fn answered(&mut self, call: Call, answer: Answer) -> Result<Vec<Task>, DriverError> {
        let mut tasks = self.taken(call, answer)?;
        while self.held.is_none() {
            if let Some(index) = self.queue.pop() {
                let op = &self.ops[index];
                let staged = (op.inputs.iter())
                    .map(|(native, from, port)| {
                        let values = self.values[*from].as_ref().expect("ended");
                        (native.clone(), values[port].clone())
                    })
                    .collect();
                let answer = self.adapter.prepare_job(&op.key.clone(), staged)?;
                tasks.extend(self.taken(Call::Prepare(index), answer)?);
            } else if let Some(outcome) = self.parked.pop_front() {
                let index = self.owners[&outcome.id];
                let answer = self.adapter.take_job(outcome)?;
                tasks.extend(self.taken(Call::Take(index), answer)?);
            } else {
                break;
            }
        }
        Ok(tasks)
    }

    /// The tasks of one answer of the adapter to `call`.
    fn taken(&mut self, call: Call, answer: Answer) -> Result<Vec<Task>, DriverError> {
        Ok(match (call, answer) {
            (_, Answer::Commands(commands)) => {
                self.held = Some(call);
                commands
            }
            (Call::Start, Answer::Jobs(jobs)) => {
                let planned = |op: &JobOp| snakemake_compiler::plans_job(&jobs, &op.rule);
                if let Some(op) = self.ops.iter().find(|op| !planned(op)) {
                    let message = format!("the run plans no job {:?} as compiled", op.key);
                    return Err(DriverError::new(message));
                }
                Vec::new()
            }
            (Call::Prepare(index), Answer::Prepared(Prepared::Run(task))) => {
                self.owners.insert(task.id, index);
                vec![*task]
            }
            (Call::Prepare(index), Answer::Prepared(Prepared::Skipped(made)))
            | (Call::Take(index), Answer::Taken(JobResult::Accepted(made))) => {
                self.ended(index, made);
                Vec::new()
            }
            (Call::Prepare(_), Answer::Prepared(Prepared::Failed(message))) => {
                return Err(DriverError::new(message));
            }
            (Call::Take(_), Answer::Taken(JobResult::Retry(task))) => vec![*task],
            (Call::Take(_), Answer::Taken(JobResult::Failed(_))) => Vec::new(),
            (call, answer) => {
                return Err(DriverError::new(format!("{call:?} gave {answer:?}")));
            }
        })
    }

    /// Records the files of an op that ended and queues the ops that wait for nothing else.
    fn ended(&mut self, index: usize, made: Vec<Json>) {
        self.values[index] = Some(self.ported(index, made));
        let mut freed = Vec::new();
        for consumer in &self.consumers[index] {
            self.waiting[*consumer] -= 1;
            if self.waiting[*consumer] == 0 {
                freed.push(*consumer);
            }
        }
        freed.sort_unstable_by(|one, other| other.cmp(one));
        self.queue.extend(freed);
    }

    /// The files of a job by the ports of their selectors.
    fn ported(&self, index: usize, made: Vec<Json>) -> BTreeMap<String, Json> {
        (made.into_iter().enumerate())
            .filter_map(|(position, file)| {
                let port = self.ops[index].outputs.get(&format!("output/{position}"))?;
                Some((port.clone(), file))
            })
            .collect()
    }
}

impl Driver for Runner {
    fn language(&self) -> Language {
        Language::Wir
    }

    fn start(&mut self) -> Result<Vec<Task>, DriverError> {
        let answer = self.adapter.start_jobs()?;
        self.answered(Call::Start, answer)
    }

    fn complete(&mut self, outcome: Outcome) -> Result<Vec<Task>, DriverError> {
        let call = match (self.held, self.owners.get(&outcome.id)) {
            (Some(_), Some(_)) => {
                self.parked.push_back(outcome);
                return Ok(Vec::new());
            }
            (Some(call), None) => call,
            (None, Some(index)) => Call::Take(*index),
            (None, None) => return Err(DriverError::new("an outcome of no op")),
        };
        self.held = None;
        let answer = self.adapter.take_job(outcome)?;
        self.answered(call, answer)
    }

    fn take_acceptance(&mut self, task: TaskId, attempt: u32) -> bool {
        self.adapter.take_acceptance(task, attempt)
    }

    fn notes(&mut self) -> Vec<Note> {
        self.adapter.notes()
    }

    fn deliveries(&mut self) -> Vec<Delivery> {
        self.adapter.deliveries()
    }

    fn delivered(&mut self, delivery: &Delivery) -> Result<(), DriverError> {
        self.adapter.delivered(delivery)
    }

    fn use_files(&mut self, files: Arc<dyn Files>) {
        self.adapter.use_files(files);
    }

    fn finish(&mut self) -> Result<(), DriverError> {
        self.adapter.finish()
    }
}

/// What a simulated run gave: its tasks as text with acceptance, cache key and resume
/// fingerprint, sorted, and its target files; folders and task numbers are left out.
#[derive(Debug, PartialEq)]
struct Ran {
    tasks: Vec<(String, Option<bool>, String, String)>,
    targets: String,
}

fn simulate(driver: &mut dyn Driver, folders: &[&Path], simulator: &Path) -> Result<Ran, String> {
    let mut executor = Simulator::new(simulator);
    let options = RunOptions {
        max_pending: 8,
        order: Order::AsAsked,
    };
    let execution = execute_with("differential", driver, &mut executor, &options);
    let graph = execution.into_result().map_err(|error| error.to_string())?;
    Ok(Ran {
        tasks: described(&graph, folders, simulator),
        targets: String::new(),
    })
}

/// Text with the folders of the run and the numbers of the tasks in simulator paths left out.
fn normal(text: &str, folders: &[&Path], simulator: &Path, names: &HashMap<u64, String>) -> String {
    let mut text = text.replace(&simulator.to_string_lossy()[..], "<simulator>");
    for folder in folders {
        text = text.replace(&folder.to_string_lossy()[..], "<work>");
    }
    let marker = "<simulator>/work/";
    let mut shown = String::new();
    while let Some(start) = text.find(marker) {
        let after = start + marker.len();
        let digits = text[after..]
            .chars()
            .take_while(char::is_ascii_digit)
            .count();
        let number = text[after..after + digits].parse::<u64>().ok();
        let name = number.and_then(|number| names.get(&number)).cloned();
        shown.push_str(&text[..after]);
        shown.push_str(&name.unwrap_or_default().replace('"', "'"));
        text = text[after + digits..].to_owned();
    }
    shown + &text
}

fn described(
    graph: &ExecutionGraph,
    folders: &[&Path],
    simulator: &Path,
) -> Vec<(String, Option<bool>, String, String)> {
    let names: HashMap<u64, String> = (graph.nodes.iter())
        .map(|node| (node.task.id.0, node.task.name.clone()))
        .collect();
    let mut tasks: Vec<_> = (graph.nodes.iter())
        .map(|node| {
            let task = Task {
                id: TaskId(0),
                after: Vec::new(),
                ..node.task.clone()
            };
            let text = serde_json::to_string(&task).expect("serializes");
            let text = normal(&text, folders, simulator, &names);
            let task: Task = serde_json::from_str(&text).expect("parses");
            let request = task_runner::request(&task, task.image.clone(), Vec::new(), None);
            // A rule without a directive, like `all`, runs no program and has no key.
            let key = request.spec.cache_key().map(|key| key.to_string());
            let key = key.unwrap_or_else(|error| format!("{error:?}"));
            let fingerprint = run_database::fingerprint(&task, &[]);
            (text, node.accepted, key, fingerprint)
        })
        .collect();
    tasks.sort_by(|one, other| one.3.cmp(&other.3));
    tasks
}

/// How one workflow compared, and which counts it adds to.
#[derive(Debug, Default)]
struct Compared {
    native: bool,
    full: bool,
    partly: bool,
    failed: bool,
    compiled: bool,
    ends: bool,
    same: bool,
    equal: bool,
    shown: String,
}

/// Runs a workflow natively, compiles it and runs the compiled document, with the files of
/// the runs below `scratch`.
fn compare(launch: &Launch, workdir: &Path, scratch: &Path) -> Compared {
    let mut compared = Compared::default();
    let run = RunSettings {
        workdir: Some(workdir.to_owned()),
        ..RunSettings::default()
    };
    let (own, compiled_scratch) = (scratch.join("native"), scratch.join("compiled"));
    let native_launch = Launch {
        scratch: own.clone(),
        ..launch.clone()
    };
    let folders = [own.as_path(), compiled_scratch.as_path(), workdir, scratch];
    let native = snakemake_runtime::spawn_driver(&native_launch, Settings::default(), run.clone());
    let native = native
        .map_err(|error| error.message)
        .and_then(|mut driver| {
            let simulator = scratch.join("simulator-native");
            let ran = simulate(&mut driver, &folders, &simulator)?;
            let targets = driver.exported().unwrap_or_default();
            let targets = normal(&targets.to_string(), &folders, &simulator, &HashMap::new());
            Ok(Ran { targets, ..ran })
        });
    compared.native = native.is_ok();
    let compile_launch = Launch {
        scratch: scratch.join("compile"),
        ..launch.clone()
    };
    let compiled =
        snakemake_compiler::compile(&compile_launch, Settings::default(), run.clone(), workdir);
    let compiled = match compiled {
        Ok(compiled) => compiled,
        Err(error) => {
            compared.failed = true;
            compared.shown = format!("does not compile: {}", first_line(&error));
            return compared;
        }
    };
    if let Some(kept) = compiled.kept.first() {
        compared.partly = true;
        compared.shown = format!("stays native: {}", kept.reason);
        return compared;
    }
    compared.full = true;
    if let Err(problems) = wir::verify(&compiled.document) {
        compared.shown = format!("does not verify: {:?}", problems.first());
        return compared;
    }
    let ran = Runner::new(&compiled.document, workdir, &compiled_scratch).and_then(|mut runner| {
        let simulator = scratch.join("simulator-compiled");
        let ran = simulate(&mut runner, &folders, &simulator)?;
        let targets = runner.adapter.exported().unwrap_or_default();
        let targets = normal(&targets.to_string(), &folders, &simulator, &HashMap::new());
        Ok(Ran { targets, ..ran })
    });
    compared.compiled = ran.is_ok();
    compared.shown = match (&native, &ran) {
        (Err(error), Err(_)) => {
            compared.ends = true;
            format!("both runs fail: {}", first_line(error))
        }
        (Err(error), Ok(_)) => format!("only the native run fails: {}", first_line(error)),
        (Ok(_), Err(error)) => format!("the compiled run fails: {}", first_line(error)),
        (Ok(native), Ok(ran)) => {
            compared.same = native.tasks == ran.tasks;
            // `SNAKEMAKE_COMPARE=2` prints the first pair of tasks that differ.
            let pairs = native.tasks.iter().zip(&ran.tasks);
            if let Some(pair) = pairs.into_iter().find(|(one, other)| one != other)
                && std::env::var("SNAKEMAKE_COMPARE").is_ok_and(|value| value == "2")
            {
                eprintln!("native:   {:?}\ncompiled: {:?}", pair.0, pair.1);
            }
            compared.equal = compared.same && native.targets == ran.targets;
            compared.ends = compared.same;
            match (compared.same, compared.equal) {
                (true, true) => format!("same: {} tasks", native.tasks.len()),
                (true, false) => "other target files".to_owned(),
                _ => format!(
                    "other tasks: {} native, {} compiled",
                    native.tasks.len(),
                    ran.tasks.len()
                ),
            }
        }
    };
    compared
}

fn first_line(text: &str) -> String {
    text.lines()
        .next()
        .unwrap_or_default()
        .chars()
        .take(200)
        .collect()
}

/// Compares the runs of `snakefile` with the targets of the command line.
fn compared(snakefile: &str, targets: &[&str]) -> Compared {
    let folder = tempfile::tempdir().expect("a temporary directory");
    let root = folder.path().join("work");
    std::fs::create_dir(&root).expect("makes the working folder");
    std::fs::write(root.join("Snakefile"), snakefile).expect("writes the Snakefile");
    let launch = Launch {
        target: root.join("Snakefile"),
        targets: targets.iter().map(|target| (*target).to_owned()).collect(),
        simulated: true,
        ..Launch::default()
    };
    compare(&launch, &root, &folder.path().join("scratch"))
}

#[test]
fn compiled_runs_match() {
    let compared = compared(SNAKEFILE, &[]);
    assert_eq!(compared.shown, "same: 6 tasks");
    assert!(compared.equal);
}

/// A group job, a pipe and a service with its users each run as one task.
#[test]
fn compiled_groups_match() {
    let snakefile = SNAKEFILE.replace(
        "    shell: \"echo {params.word} > {output}\"",
        "    group: \"g\"\n    shell: \"echo {params.word} > {output}\"",
    );
    let snakefile = snakefile.replace("    retries: 1", "    retries: 1\n    group: \"g\"");
    let piped = "rule all:\n    input: \"b.txt\"\n\nrule a:\n    output: pipe(\"a.pipe\")\n    shell: \"echo a > {output}\"\n\nrule b:\n    input: \"a.pipe\"\n    output: \"b.txt\"\n    shell: \"cat {input} > {output}\"\n";
    let served = "rule all:\n    input: \"b.txt\", \"c.txt\"\n\nrule a:\n    output: service(\"a.socket\")\n    shell: \"sleep 100 > {output}\"\n\nrule b:\n    input: \"a.socket\"\n    output: \"b.txt\"\n    shell: \"head -n1 {input} > {output}\"\n\nrule c:\n    input: \"a.socket\"\n    output: \"c.txt\"\n    shell: \"head -n1 {input} > {output}\"\n";
    let cases = [
        (snakefile.as_str(), "same: 4 tasks"),
        (piped, "same: 2 tasks"),
        (served, "same: 2 tasks"),
    ];
    for (snakefile, shown) in cases {
        let compared = compared(snakefile, &[]);
        assert_eq!(compared.shown, shown);
        assert!(compared.equal);
    }
}

/// Commands of workflow code run while the run plans and while a job is prepared, for two
/// targets.
#[test]
fn compiled_commands_match() {
    let snakefile = SNAKEFILE.replace("rule all:", "shell(\"echo planned\")\n\nrule all:");
    let snakefile = snakefile.replace(
        "    threads: 2",
        "    threads: 2\n    conda: lambda wildcards: shell(\"echo env.yaml\", read=True).strip()",
    );
    let compared = compared(&snakefile, &["summary.txt", "counts/a.txt"]);
    assert_eq!(compared.shown, "same: 7 tasks");
    assert!(compared.equal);
}

/// How a test workflow is launched: its working folder and Snakefile, config files, targets
/// and profiles, from the launches of the test-workflows test of snakemake-runtime.
fn launch_of(name: &str, folder: &Path, scratch: &Path) -> (Launch, PathBuf) {
    let mut launch = Launch {
        target: folder.to_owned(),
        scratch: scratch.to_owned(),
        simulated: true,
        ..Launch::default()
    };
    let mut workdir = folder.to_owned();
    let line = (LAUNCHES.lines())
        .filter(|line| !line.starts_with('#'))
        .find(|line| line.split('\t').next() == Some(name));
    for field in line.into_iter().flat_map(|line| line.split('\t').skip(2)) {
        let (key, value) = field.split_once('=').unwrap_or((field, ""));
        match key {
            "directory" => workdir = folder.join(value),
            "snakefile" => launch.target = folder.join(value),
            "configfile" => launch.configs.push(folder.join(value)),
            "config" => {
                let (name, value) = value.split_once('=').expect("a config value");
                launch.params.push((name.to_owned(), value.to_owned()));
            }
            "target" => launch.targets.push(value.to_owned()),
            "profile" | "workflow-profile" => {
                let path = folder.join(value);
                let profile = match path.exists() {
                    true => path.to_string_lossy().into_owned(),
                    false => value.to_owned(),
                };
                launch.profiles.push(profile);
            }
            _ => {}
        }
    }
    if launch.target == folder && workdir != folder {
        launch.target = workdir.clone();
    }
    (launch, workdir)
}

/// The workflow folders of the test workflows that have a Snakefile where Snakemake looks.
fn workflows(root: &Path) -> Vec<(String, PathBuf)> {
    let mut found = Vec::new();
    for (group, folder) in [
        ("catalog", root.join("catalog")),
        ("tests", root.join("snakemake-tests/tests")),
    ] {
        let Ok(entries) = std::fs::read_dir(&folder) else {
            continue;
        };
        for entry in entries.flatten() {
            let path = entry.path();
            if path.is_dir() && snakemake_runtime::find_snakefile(&path).is_some() {
                let name = path.file_name().unwrap_or_default().to_string_lossy();
                found.push((format!("{group}/{}", name.replace(' ', "_")), path));
            }
        }
    }
    found.sort();
    found
}

/// Compares one workflow when the test binary runs as a child for it, and writes the result.
#[test]
#[ignore = "a child of the test-workflows test"]
fn compare_one() {
    let (Some(name), Some(folder), Some(result), Some(scratch)) = (
        std::env::var_os("SNAKEMAKE_COMPARE_NAME"),
        std::env::var_os("SNAKEMAKE_COMPARE_FOLDER"),
        std::env::var_os("SNAKEMAKE_COMPARE_RESULT"),
        std::env::var_os("SNAKEMAKE_COMPARE_SCRATCH"),
    ) else {
        return;
    };
    let (folder, scratch) = (PathBuf::from(folder), PathBuf::from(scratch));
    let (launch, workdir) = launch_of(&name.to_string_lossy(), &folder, &scratch);
    let compared = compare(&launch, &workdir, &scratch);
    let flags = [
        compared.native,
        compared.full,
        compared.partly,
        compared.failed,
        compared.compiled,
        compared.ends,
        compared.same,
        compared.equal,
    ];
    let flags: Vec<&str> = flags
        .iter()
        .map(|flag| if *flag { "1" } else { "0" })
        .collect();
    let shown = compared.shown.replace('\n', " ");
    std::fs::write(result, format!("{}\t{shown}", flags.join(""))).expect("writes the result");
}

/// Compiles and runs every test workflow in a child process with a time limit and compares the
/// counts with the recorded ones; `SNAKEMAKE_COMPARE=1` prints the result of each.
#[test]
#[ignore = "needs the test workflows of just snakemake-test-workflows"]
fn matches_recorded() {
    let root = std::env::var_os("SNAKEMAKE_TEST_WORKFLOWS").map(PathBuf::from);
    let root = root.expect("SNAKEMAKE_TEST_WORKFLOWS names the test workflows");
    let results = tempfile::tempdir().expect("a temporary directory");
    let print = std::env::var_os("SNAKEMAKE_COMPARE").is_some();
    let mut counts = Counts::default();
    for (index, (name, folder)) in workflows(&root).into_iter().enumerate() {
        let result = results.path().join(index.to_string());
        // The parent removes the scratch folder, also after the time limit ended the child.
        let scratch = tempfile::tempdir().expect("a temporary directory");
        let status = Command::new("timeout")
            .arg("300")
            .arg(std::env::current_exe().expect("the test binary"))
            .args(["--exact", "compare_one", "--ignored", "--test-threads", "1"])
            .env("SNAKEMAKE_COMPARE_NAME", &name)
            .env("SNAKEMAKE_COMPARE_FOLDER", &folder)
            .env("SNAKEMAKE_COMPARE_RESULT", &result)
            .env("SNAKEMAKE_COMPARE_SCRATCH", scratch.path())
            .env("UWE_QUIET", "1")
            .env("LC_CTYPE", "C.UTF-8")
            .output()
            .expect("runs the test binary");
        let text = std::fs::read_to_string(&result).unwrap_or_else(|_| {
            let code = status
                .status
                .code()
                .map_or("a signal".into(), |code| code.to_string());
            format!("00000000\tthe child ended with {code}")
        });
        let (flags, shown) = text.split_once('\t').unwrap_or((&text, ""));
        let flag = |position: usize| flags.as_bytes().get(position) == Some(&b'1');
        counts.workflows += 1;
        for (position, count) in [
            &mut counts.native,
            &mut counts.full,
            &mut counts.partly,
            &mut counts.failed,
            &mut counts.compiled,
            &mut counts.ends,
            &mut counts.same,
            &mut counts.equal,
        ]
        .into_iter()
        .enumerate()
        {
            *count += usize::from(flag(position));
        }
        if print {
            println!("{name}\t{shown}");
        }
    }
    assert_eq!(counts, RECORDED);
}
