//! Runs small CWL documents through the driver and the simulator without a job file,
//! so on placeholder inputs, as `iunctus run` and the graph test-workflows test do.
// Copyright (c) 2026 The iunctus Contributors
// SPDX-License-Identifier: MIT or Apache-2.0

use std::path::Path;

use cwl_runtime::{CwlDriver, Options};
use workflow_graph::{
    Attempt, ExecuteError, Executor, Launch, Outcome, Running, Simulator, Task, execute,
};

/// Writes the files into a temporary folder, runs `main.cwl` and gives the task count or the
/// error.
fn run(files: &[(&str, &str)]) -> Result<usize, String> {
    let folder = tempfile::tempdir().map_err(|error| error.to_string())?;
    for (name, text) in files {
        std::fs::write(folder.path().join(name), text).map_err(|error| error.to_string())?;
    }
    let main = folder.path().join("main.cwl");
    let launch = Launch {
        target: main.clone(),
        scratch: folder.path().join("scratch"),
        ..Launch::default()
    };
    let mut driver = CwlDriver::new(&launch, Options::default()).map_err(|error| error.message)?;
    let mut executor = Simulator::new(Path::new(&launch.scratch).join("simulator"));
    let execution = execute(&main.to_string_lossy(), &mut driver, &mut executor);
    match execution.error {
        Some(error) => Err(error.to_string()),
        None => Ok(execution.graph.nodes.len()),
    }
}

#[test]
fn outputs_that_only_cwl_output_json_gives_keep_their_format() {
    let make = "cwlVersion: v1.2\nclass: CommandLineTool\nbaseCommand: make\ninputs: []\n\
        outputs:\n  made: {type: File, format: 'http://example.com/movies'}\n";
    let take = "cwlVersion: v1.2\nclass: CommandLineTool\nbaseCommand: take\n\
        inputs:\n  movies: {type: File, format: 'http://example.com/movies', inputBinding: {}}\n\
        outputs: []\n";
    let main = "cwlVersion: v1.2\nclass: Workflow\ninputs: []\noutputs: []\nsteps:\n  \
        make: {run: make.cwl, in: [], out: [made]}\n  \
        take: {run: take.cwl, in: {movies: make/made}, out: []}\n";
    let files = [("make.cwl", make), ("take.cwl", take), ("main.cwl", main)];
    assert_eq!(run(&files), Ok(2));
}

#[test]
fn placeholder_files_take_the_format_a_tool_asks_for() {
    let take = "cwlVersion: v1.2\nclass: CommandLineTool\nbaseCommand: take\n\
        inputs:\n  reads: {type: File, format: 'http://example.com/fastq', inputBinding: {}}\n\
        outputs: []\n";
    let main = "cwlVersion: v1.2\nclass: Workflow\ninputs: {reads: File}\noutputs: []\n\
        steps:\n  take: {run: take.cwl, in: {reads: reads}, out: []}\n";
    assert_eq!(run(&[("take.cwl", take), ("main.cwl", main)]), Ok(1));
}

#[test]
fn rejects_negative_time_limits() {
    let tool = "cwlVersion: v1.2\nclass: CommandLineTool\nbaseCommand: [sleep, '1']\n\
        requirements: {ToolTimeLimit: {timelimit: -1}}\ninputs: []\noutputs: []\n";
    let error = run(&[("main.cwl", tool)]).expect_err("a negative time limit fails");
    assert!(error.contains("timelimit"), "{error}");
}

#[test]
fn absolute_entry_names_need_docker_as_a_requirement() {
    let tool = |docker: &str| {
        format!(
            "cwlVersion: v1.2\nclass: CommandLineTool\nbaseCommand: cat\n{docker}\
             requirements:\n  InitialWorkDirRequirement:\n    listing:\n      \
             - {{entryname: /data/x.txt, entry: text}}\ninputs: []\noutputs: []\n"
        )
    };
    let hinted = tool("hints: {DockerRequirement: {dockerPull: alpine}}\n");
    let error = run(&[("main.cwl", &hinted)]).expect_err("a hint is not enough");
    assert!(error.contains("DockerRequirement"), "{error}");
    let required = tool("").replace(
        "requirements:\n",
        "requirements:\n  DockerRequirement: {dockerPull: alpine}\n",
    );
    assert_eq!(run(&[("main.cwl", &required)]), Ok(1));
}

#[test]
fn outputs_of_abstract_steps_bring_their_secondary_files() {
    let index = "cwlVersion: v1.2\nclass: Operation\ninputs: []\n\
        outputs:\n  indexed: {type: File, secondaryFiles: [.fai]}\n";
    let align = "cwlVersion: v1.2\nclass: CommandLineTool\nbaseCommand: align\n\
        inputs:\n  reference: {type: File, secondaryFiles: [.fai], inputBinding: {}}\n\
        outputs: []\n";
    let main = "cwlVersion: v1.2\nclass: Workflow\ninputs: []\noutputs: []\nsteps:\n  \
        index: {run: index.cwl, in: [], out: [indexed]}\n  \
        align: {run: align.cwl, in: {reference: index/indexed}, out: []}\n";
    let files = [
        ("index.cwl", index),
        ("align.cwl", align),
        ("main.cwl", main),
    ];
    assert_eq!(run(&files), Ok(2));
}

#[test]
fn simulated_files_of_bare_wildcards_take_the_extension_the_output_suggests() {
    let extract = "cwlVersion: v1.2\nclass: CommandLineTool\nbaseCommand: extract\ninputs: []\n\
        outputs:\n  fastq_file: {type: File, outputBinding: {glob: '*'}}\n";
    let check = "cwlVersion: v1.2\nclass: CommandLineTool\nbaseCommand: check\n\
        requirements: {InlineJavascriptRequirement: {}}\ninputs: {reads: File}\noutputs: []\n\
        arguments: ['${ if (inputs.reads.nameext != \".fastq\") { throw \"no extension\"; } \
        return inputs.reads.basename; }']\n";
    let main = "cwlVersion: v1.2\nclass: Workflow\ninputs: []\noutputs: []\nsteps:\n  \
        extract: {run: extract.cwl, in: [], out: [fastq_file]}\n  \
        check: {run: check.cwl, in: {reads: extract/fastq_file}, out: []}\n";
    let files = [
        ("extract.cwl", extract),
        ("check.cwl", check),
        ("main.cwl", main),
    ];
    assert_eq!(run(&files), Ok(2));
}

#[test]
fn only_a_launch_that_assumes_its_inputs_takes_absent_files_as_present() {
    let folder = tempfile::tempdir().expect("a temporary folder");
    let tool = "cwlVersion: v1.2\nclass: CommandLineTool\nbaseCommand: cat\n\
        inputs:\n  text: {type: File, loadContents: true}\noutputs: []\n";
    let main = folder.path().join("main.cwl");
    std::fs::write(&main, tool).expect("writes the tool");
    let job = folder.path().join("job.yml");
    std::fs::write(&job, "text: {class: File, location: absent.txt}\n").expect("writes the job");
    let outcome = |simulated: bool| {
        let launch = Launch {
            target: main.clone(),
            inputs: Some(job.clone()),
            scratch: folder.path().join("scratch"),
            simulated,
            ..Launch::default()
        };
        let mut driver = cwl_runtime::driver(&launch).map_err(|error| error.message)?;
        let mut executor = Simulator::new(folder.path().join("simulator"));
        match execute("main", driver.as_mut(), &mut executor).error {
            Some(error) => Err(error.to_string()),
            None => Ok(()),
        }
    };
    assert_eq!(outcome(true), Ok(()));
    let absent = folder.path().join("absent.txt");
    assert_eq!(
        outcome(false),
        Err(format!(
            "main: the input `text` names `{}`, which does not exist",
            absent.display()
        ))
    );
}

#[test]
fn expression_tools_over_absent_data_give_placeholders_for_null_outputs() {
    let folder = tempfile::tempdir().expect("a temporary folder");
    let pick = "cwlVersion: v1.2\nclass: ExpressionTool\n\
        requirements: {InlineJavascriptRequirement: {}}\n\
        inputs: {folder: Directory}\noutputs: {picked: File?}\n\
        expression: '${ return {picked: inputs.folder.listing.length ? inputs.folder.listing[0] : null}; }'\n";
    let slop = "cwlVersion: v1.2\nclass: CommandLineTool\nbaseCommand: slop\n\
        inputs:\n  lengths: {type: File, inputBinding: {}}\noutputs: []\n";
    let main = "cwlVersion: v1.2\nclass: Workflow\ninputs: {folder: Directory}\noutputs: []\n\
        steps:\n  pick: {run: pick.cwl, in: {folder: folder}, out: [picked]}\n  \
        slop: {run: slop.cwl, in: {lengths: pick/picked}, out: []}\n";
    for (name, text) in [("pick.cwl", pick), ("slop.cwl", slop), ("main.cwl", main)] {
        std::fs::write(folder.path().join(name), text).expect("writes a document");
    }
    let job = folder.path().join("job.yml");
    std::fs::write(&job, "folder: {class: Directory, location: absent}\n").expect("writes the job");
    let launch = Launch {
        target: folder.path().join("main.cwl"),
        inputs: Some(job),
        scratch: folder.path().join("scratch"),
        simulated: true,
        ..Launch::default()
    };
    let mut driver = cwl_runtime::driver(&launch).expect("a driver");
    let mut executor = Simulator::new(folder.path().join("simulator"));
    let execution = execute("main", driver.as_mut(), &mut executor);
    assert!(execution.error.is_none(), "{:?}", execution.error);
    assert_eq!(execution.graph.nodes.len(), 1);
}

#[test]
fn errors_of_expressions_say_where_the_expression_is() {
    let tool = "cwlVersion: v1.2\nclass: CommandLineTool\nbaseCommand: sh\n\
        requirements:\n  InlineJavascriptRequirement: {}\n  InitialWorkDirRequirement:\n    \
        listing: [{entryname: run.sh, entry: 'cd ${HOME}'}]\ninputs: []\noutputs: []\n";
    assert_eq!(
        run(&[("main.cwl", tool)]),
        Err(
            "main: the InitialWorkDir listing: JavaScript error in `HOME`: ReferenceError: \
             HOME is not defined"
                .to_owned()
        )
    );
    let step = "cwlVersion: v1.2\nclass: CommandLineTool\nbaseCommand: echo\ninputs: []\n\
        outputs: []\n";
    let main = "cwlVersion: v1.2\nclass: Workflow\ninputs: []\noutputs: []\n\
        steps:\n  echo: {run: step.cwl, in: [], out: [], when: $(inputs.useStep)}\n";
    let error = run(&[("step.cwl", step), ("main.cwl", main)]).expect_err("fails");
    assert!(
        error.ends_with("`when`: there is no input `useStep`"),
        "{error}"
    );
}

#[test]
fn a_simulated_run_says_when_a_scatter_needs_real_data() {
    let folder = tempfile::tempdir().expect("a temporary folder");
    let find = "cwlVersion: v1.2\nclass: CommandLineTool\nbaseCommand: find\n\
        inputs: {data: File}\noutputs: {peaks: 'Any[]?'}\n";
    let pick = "cwlVersion: v1.2\nclass: CommandLineTool\nbaseCommand: pick\n\
        inputs: {peak: Any, data: File}\noutputs: []\n";
    let main = "cwlVersion: v1.2\nclass: Workflow\n\
        requirements: {ScatterFeatureRequirement: {}}\ninputs: {data: File}\noutputs: []\n\
        steps:\n  find: {run: find.cwl, in: {data: data}, out: [peaks]}\n  \
        pick: {run: pick.cwl, in: {peak: find/peaks, data: data}, out: [], scatter: peak}\n";
    for (name, text) in [("find.cwl", find), ("pick.cwl", pick), ("main.cwl", main)] {
        std::fs::write(folder.path().join(name), text).expect("writes a document");
    }
    let job = folder.path().join("job.yml");
    std::fs::write(&job, "data: {class: File, location: absent.mzML}\n").expect("writes the job");
    let launch = Launch {
        target: folder.path().join("main.cwl"),
        inputs: Some(job),
        scratch: folder.path().join("scratch"),
        simulated: true,
        ..Launch::default()
    };
    let mut driver = cwl_runtime::driver(&launch).expect("a driver");
    let mut executor = Simulator::new(folder.path().join("simulator"));
    let execution = execute("main", driver.as_mut(), &mut executor);
    let error = execution.error.expect("the scatter fails").to_string();
    assert!(
        error.ends_with(
            "pick: the scattered input `peak` is null, not a list; the simulated run has no \
             real data to fill it"
        ),
        "{error}"
    );
    // The graph marks the task whose output the scatter needs.
    let find = &execution.graph.nodes[0];
    let reasons: Vec<&str> = find.markers.iter().map(|m| m.reason.as_str()).collect();
    assert_eq!(
        reasons,
        ["the scatter of step `pick` over `peak` follows the outputs of this task"]
    );
}

#[test]
fn a_scatter_over_the_files_of_a_glob_marks_the_task_that_made_them() {
    let folder = tempfile::tempdir().expect("a temporary folder");
    let split = "cwlVersion: v1.2\nclass: CommandLineTool\nbaseCommand: split\n\
        inputs: []\noutputs: {parts: {type: 'File[]', outputBinding: {glob: 'part_*.txt'}}, \
        table: {type: 'File[]', outputBinding: {glob: table.txt}}}\n";
    let count = "cwlVersion: v1.2\nclass: CommandLineTool\nbaseCommand: wc\n\
        inputs: {part: File}\noutputs: []\n";
    let main = "cwlVersion: v1.2\nclass: Workflow\n\
        requirements: {ScatterFeatureRequirement: {}}\ninputs: []\noutputs: []\n\
        steps:\n  split: {run: split.cwl, in: [], out: [parts, table]}\n  \
        count: {run: count.cwl, in: {part: split/parts}, out: [], scatter: part}\n  \
        rows: {run: count.cwl, in: {part: {source: split/table}}, out: [], \
        scatter: part}\n";
    for (name, text) in [
        ("split.cwl", split),
        ("count.cwl", count),
        ("main.cwl", main),
    ] {
        std::fs::write(folder.path().join(name), text).expect("writes a document");
    }
    let launch = Launch {
        target: folder.path().join("main.cwl"),
        scratch: folder.path().join("scratch"),
        simulated: true,
        ..Launch::default()
    };
    let mut driver = cwl_runtime::driver(&launch).expect("a driver");
    let mut executor = Simulator::new(folder.path().join("simulator"));
    let execution = execute("main", driver.as_mut(), &mut executor);
    let graph = execution.graph;
    let split = &graph.nodes[0];
    let reasons: Vec<&str> = split.markers.iter().map(|m| m.reason.as_str()).collect();
    assert_eq!(
        reasons,
        ["the output pattern `part_*.txt` matched 1 file, and each started tasks of its own"],
        "{}",
        graph.summary()
    );
}

/// Finishes the tasks that do nothing at once and without files, as real executors do, and
/// pretends the others like the simulator.
struct Real {
    simulator: Simulator,
}

impl Executor for Real {
    fn run(&mut self, attempt: Attempt) -> Running {
        Running::ended(self.execute(&attempt.task))
    }
}

impl Real {
    fn execute(&mut self, task: &Task) -> Result<Outcome, ExecuteError> {
        if task.is_no_op() {
            return Ok(Outcome {
                id: task.id,
                ..Outcome::default()
            });
        }
        self.simulator.simulate(task)
    }
}

/// Runs `main.cwl` of these files for real, not assuming its inputs, and gives the task count
/// or the error.
fn run_for_real(files: &[(&str, &str)]) -> Result<usize, String> {
    let folder = tempfile::tempdir().map_err(|error| error.to_string())?;
    for (name, text) in files {
        std::fs::write(folder.path().join(name), text).map_err(|error| error.to_string())?;
    }
    let launch = Launch {
        target: folder.path().join("main.cwl"),
        scratch: folder.path().join("scratch"),
        simulated: false,
        ..Launch::default()
    };
    let mut driver = cwl_runtime::driver(&launch).map_err(|error| error.message)?;
    let mut executor = Real {
        simulator: Simulator::new(folder.path().join("simulator")),
    };
    let execution = execute("main", driver.as_mut(), &mut executor);
    match execution.error {
        Some(error) => Err(error.to_string()),
        None => Ok(execution.graph.nodes.len()),
    }
}

#[test]
fn operations_do_nothing_in_a_real_run() {
    let index = |kind: &str| {
        format!("cwlVersion: v1.2\nclass: Operation\ninputs: []\noutputs:\n  indexed: {kind}\n")
    };
    let align = |kind: &str| {
        format!(
            "cwlVersion: v1.2\nclass: CommandLineTool\nbaseCommand: align\n\
             inputs:\n  reference: {{type: '{kind}', inputBinding: {{}}}}\noutputs: []\n"
        )
    };
    let main = "cwlVersion: v1.2\nclass: Workflow\ninputs: []\noutputs: []\nsteps:\n  \
        index: {run: index.cwl, in: [], out: [indexed]}\n  \
        align: {run: align.cwl, in: {reference: index/indexed}, out: []}\n";
    // An optional output is null, and the next step runs without it.
    let (optional_index, optional_align) = (index("File?"), align("File?"));
    let files = [
        ("index.cwl", optional_index.as_str()),
        ("align.cwl", optional_align.as_str()),
        ("main.cwl", main),
    ];
    assert_eq!(run_for_real(&files), Ok(2));
    // A required output has no value, which the error says.
    let (required_index, required_align) = (index("File"), align("File"));
    let files = [
        ("index.cwl", required_index.as_str()),
        ("align.cwl", required_align.as_str()),
        ("main.cwl", main),
    ];
    let error = run_for_real(&files).expect_err("nothing makes the output");
    assert!(
        error.contains(
            "the output `indexed`: the step runs an abstract Operation, which has no tool, so a \
             real run has no value for this output"
        ),
        "{error}"
    );
}

/// Notes the targets of deliveries.
struct Targets(Vec<std::path::PathBuf>);

impl workflow_graph::Observer for Targets {
    fn event(&mut self, event: workflow_graph::Event<'_>) {
        if let workflow_graph::Event::Delivered { delivery } = event {
            self.0.push(delivery.target.clone().into());
        }
    }
}

#[test]
fn real_runs_deliver_the_output_files_to_the_output_folder() {
    let folder = tempfile::tempdir().expect("a temporary directory");
    let tool = "cwlVersion: v1.2\nclass: CommandLineTool\nbaseCommand: [touch, out.txt]\n\
                inputs: []\noutputs:\n  made: {type: File, outputBinding: {glob: out.txt}}\n";
    std::fs::write(folder.path().join("main.cwl"), tool).expect("writes the tool");
    let delivered = |results: std::path::PathBuf| {
        let launch = Launch {
            target: folder.path().join("main.cwl"),
            scratch: folder.path().join("scratch"),
            output_dir: Some(results),
            ..Launch::default()
        };
        let mut driver = cwl_runtime::driver(&launch).expect("a driver");
        let mut executor = Real {
            simulator: Simulator::new(folder.path().join("simulator")),
        };
        let mut targets = Targets(Vec::new());
        let execution = workflow_graph::Engine::new(&mut executor)
            .observer(&mut targets)
            .run("main", driver.as_mut());
        assert!(execution.error.is_none(), "{:?}", execution.error);
        targets.0
    };
    let targets = delivered(folder.path().join("results"));
    assert_eq!(targets, [folder.path().join("results/out.txt")]);
    // An address of a storage provider stays as it is.
    let targets = delivered("s3://bucket/out".into());
    assert_eq!(
        targets,
        [std::path::PathBuf::from("s3://bucket/out/out.txt")]
    );
}

/// Whether the task of a tool with this version and these requirements runs offline.
fn offline(version: &str, requirements: &str) -> bool {
    let folder = tempfile::tempdir().expect("a temporary directory");
    let tool = format!(
        "cwlVersion: {version}\nclass: CommandLineTool\n{requirements}inputs: []\noutputs: []\n\
         baseCommand: [curl, https://example.org]\n"
    );
    let main = folder.path().join("main.cwl");
    std::fs::write(&main, tool).expect("writes the tool");
    let launch = Launch {
        target: main.clone(),
        scratch: folder.path().join("scratch"),
        ..Launch::default()
    };
    let mut driver = CwlDriver::new(&launch, Options::default()).expect("a driver");
    let mut executor = Simulator::new(folder.path().join("simulator"));
    let execution = execute(&main.to_string_lossy(), &mut driver, &mut executor);
    assert!(execution.error.is_none(), "{:?}", execution.error);
    execution.graph.nodes[0].task.network == workflow_graph::Network::Denied
}

#[test]
fn tools_run_offline_unless_they_ask_for_the_network() {
    assert!(offline("v1.2", ""));
    let asked = "requirements:\n  NetworkAccess:\n    networkAccess: true\n";
    assert!(!offline("v1.2", asked));
    let expression = "requirements:\n  InlineJavascriptRequirement: {}\n  NetworkAccess:\n    \
                      networkAccess: $(1 == 1)\n";
    assert!(!offline("v1.2", expression));
    // CWL v1.0 has no NetworkAccess, and its tools may use the network, as with cwltool.
    assert!(!offline("v1.0", ""));
}

/// A store elsewhere: each task leaves `cwl.output.json` at an `s3://store/` location, whose
/// content is in a local folder.
struct Elsewhere(std::path::PathBuf);

impl workflow_graph::Files for Elsewhere {
    fn local(&self, location: &str) -> std::io::Result<std::path::PathBuf> {
        let key = location
            .strip_prefix("s3://store/")
            .ok_or(std::io::ErrorKind::NotFound)?;
        Ok(self.0.join(key))
    }

    fn deliver(&self, _delivery: &workflow_graph::Delivery) -> std::io::Result<()> {
        Ok(())
    }
}

impl Executor for Elsewhere {
    fn run(&mut self, attempt: Attempt) -> Running {
        Running::ended(Ok(Outcome {
            id: attempt.task.id,
            files: vec![workflow_graph::OutputFile {
                path: "cwl.output.json".into(),
                location: "s3://store/work/cwl.output.json".into(),
                ..Default::default()
            }],
            ..Outcome::default()
        }))
    }

    fn files(&self) -> Option<std::sync::Arc<dyn workflow_graph::Files>> {
        Some(std::sync::Arc::new(Elsewhere(self.0.clone())))
    }
}

#[test]
fn cwl_output_json_comes_through_the_files_of_a_store_elsewhere() {
    let folder = tempfile::tempdir().expect("a temporary directory");
    let tool = "cwlVersion: v1.2\nclass: CommandLineTool\nbaseCommand: count\ninputs: []\n\
                outputs:\n  n: int\n";
    std::fs::write(folder.path().join("main.cwl"), tool).expect("writes the tool");
    std::fs::create_dir_all(folder.path().join("store/work")).expect("makes the store");
    let json = folder.path().join("store/work/cwl.output.json");
    std::fs::write(json, "{\"n\": 3}").expect("writes the output object");
    let launch = Launch {
        target: folder.path().join("main.cwl"),
        scratch: folder.path().join("scratch"),
        output_dir: Some(
            folder
                .path()
                .join("out")
                .to_string_lossy()
                .into_owned()
                .into(),
        ),
        simulated: false,
        ..Launch::default()
    };
    let mut driver = cwl_runtime::driver(&launch).expect("a driver");
    let mut executor = Elsewhere(folder.path().join("store"));
    let execution = execute("main", driver.as_mut(), &mut executor);
    assert!(execution.error.is_none(), "{:?}", execution.error);
}

/// Fails the tools whose names end with `fail`, makes `out.txt` for the others, and notes the
/// name of each task that started.
struct Failing {
    folder: std::path::PathBuf,
    ran: Vec<String>,
}

impl Executor for Failing {
    fn run(&mut self, attempt: Attempt) -> Running {
        let task = &attempt.task;
        self.ran.push(task.name.clone());
        let exit_code = if task.name.ends_with("fail") { 3 } else { 0 };
        let location = self.folder.join(format!("{}.txt", task.id));
        std::fs::write(&location, "x\n").expect("writes");
        let files = task
            .outputs
            .iter()
            .filter(|output| output.as_str() == "out.txt")
            .map(|output| workflow_graph::OutputFile {
                path: output.clone(),
                location: location.to_string_lossy().into_owned(),
                ..workflow_graph::OutputFile::default()
            })
            .collect();
        Running::ended(Ok(Outcome {
            id: task.id,
            exit_code,
            files,
            ..Outcome::default()
        }))
    }
}

/// Runs a workflow whose step `fail` fails while `slow` runs; `later` reads what `slow` made.
/// The result has the error and the tasks that started.
fn run_failing(keep_going: bool) -> (String, Vec<String>) {
    let folder = tempfile::tempdir().expect("a temporary directory");
    let tool = "cwlVersion: v1.2\nclass: CommandLineTool\nbaseCommand: tool\n\
        inputs: {x: {type: File?, inputBinding: {}}}\n\
        outputs: {out: {type: File, outputBinding: {glob: out.txt}}}\n";
    let main = "cwlVersion: v1.2\nclass: Workflow\ninputs: []\noutputs: []\nsteps:\n  \
        fail: {run: tool.cwl, in: [], out: [out]}\n  \
        slow: {run: tool.cwl, in: [], out: [out]}\n  \
        later: {run: tool.cwl, in: {x: slow/out}, out: [out]}\n";
    std::fs::write(folder.path().join("tool.cwl"), tool).expect("writes");
    std::fs::write(folder.path().join("main.cwl"), main).expect("writes");
    let launch = Launch {
        target: folder.path().join("main.cwl"),
        scratch: folder.path().join("scratch"),
        keep_going,
        ..Launch::default()
    };
    let options = Options {
        simulated: false,
        ..Options::default()
    };
    let mut driver = CwlDriver::new(&launch, options).expect("a driver");
    let mut executor = Failing {
        folder: folder.path().to_owned(),
        ran: Vec::new(),
    };
    let run_options = workflow_graph::RunOptions {
        max_pending: 2,
        order: workflow_graph::Order::AsAsked,
    };
    let execution = workflow_graph::Engine::new(&mut executor)
        .options(run_options)
        .run("main.cwl", &mut driver);
    let error = execution.error.expect("the run fails").to_string();
    let ran = executor
        .ran
        .iter()
        .map(|name| name.replace("main/", ""))
        .collect();
    (error, ran)
}

#[test]
fn failed_tools_stop_new_steps_unless_the_run_keeps_going() {
    // As `cwltool --parallel` with `--on-error stop`, the default: the running step `slow`
    // ends, but `later` never starts.
    let (error, ran) = run_failing(false);
    assert!(error.contains("exit code 3"), "{error}");
    assert_eq!(ran, ["fail", "slow"]);
    // As `--on-error continue`: the steps that do not depend on the failure run, and the run
    // still fails.
    let (error, ran) = run_failing(true);
    assert!(error.contains("exit code 3"), "{error}");
    assert_eq!(ran, ["fail", "slow", "later"]);
}

#[test]
fn writable_initial_work_dir_entries_are_copies_of_the_task() {
    let folder = tempfile::tempdir().expect("a temporary directory");
    std::fs::write(folder.path().join("whale.txt"), "whale\n").expect("writes");
    let tool = "cwlVersion: v1.2\nclass: CommandLineTool\nbaseCommand: edit\n\
        requirements:\n  InitialWorkDirRequirement:\n    listing:\n      \
        - {entry: $(inputs.infile), entryname: bob.txt, writable: true}\n      \
        - {entry: $(inputs.other)}\n\
        inputs: {infile: File, other: File}\noutputs: []\n";
    std::fs::write(folder.path().join("main.cwl"), tool).expect("writes");
    let job =
        "infile: {class: File, location: whale.txt}\nother: {class: File, location: whale.txt}\n";
    std::fs::write(folder.path().join("job.yml"), job).expect("writes");
    let launch = Launch {
        target: folder.path().join("main.cwl"),
        inputs: Some(folder.path().join("job.yml")),
        scratch: folder.path().join("scratch"),
        ..Launch::default()
    };
    let mut driver = CwlDriver::new(&launch, Options::default()).expect("a driver");
    let mut executor = Simulator::new(folder.path().join("simulator"));
    let execution = execute("main.cwl", &mut driver, &mut executor);
    let graph = execution.into_result().expect("the run succeeds");
    let task = &graph.nodes[0].task;
    let writable = |name: &str| {
        let input = (task.inputs.iter())
            .find(|input| input.path.ends_with(name))
            .unwrap_or_else(|| panic!("no input {name} in {:?}", task.inputs));
        input.writable
    };
    assert_eq!(writable("bob.txt"), workflow_graph::Writable::Copy);
    assert_eq!(writable("whale.txt"), workflow_graph::Writable::No);
}

#[test]
fn requirements_of_the_input_object_override_those_of_the_tool() {
    let folder = tempfile::tempdir().expect("a temporary directory");
    // As the conformance tests cwl_requirements_*: the tool's own value gives way.
    let tool = "cwlVersion: v1.2\nclass: CommandLineTool\nbaseCommand: [printenv, TEST_ENV]\n\
        requirements:\n  EnvVarRequirement:\n    envDef:\n      TEST_ENV: conflict_original\n\
        inputs: {in: string}\noutputs: []\n";
    std::fs::write(folder.path().join("main.cwl"), tool).expect("writes");
    let job = "in: hello test env\ncwl:requirements:\n  - class: EnvVarRequirement\n    \
        envDef:\n      - envName: TEST_ENV\n        envValue: $(inputs.in)\n";
    std::fs::write(folder.path().join("job.yml"), job).expect("writes");
    let launch = Launch {
        target: folder.path().join("main.cwl"),
        inputs: Some(folder.path().join("job.yml")),
        scratch: folder.path().join("scratch"),
        ..Launch::default()
    };
    let mut driver = CwlDriver::new(&launch, Options::default()).expect("a driver");
    let mut executor = Simulator::new(folder.path().join("simulator"));
    let execution = execute("main.cwl", &mut driver, &mut executor);
    let graph = execution.into_result().expect("the run succeeds");
    let environment = &graph.nodes[0].task.environment;
    assert_eq!(environment["TEST_ENV"], "hello test env");
}
