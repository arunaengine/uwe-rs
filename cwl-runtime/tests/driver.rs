//! Runs small CWL documents through the driver and the mock TES executor without a job file,
//! so on placeholder inputs, as `iunctus run` and the graph test-workflows test do.
// Copyright (c) 2026 The iunctus Contributors
// SPDX-License-Identifier: MIT or Apache-2.0

use std::path::Path;

use cwl_runtime::{CwlDriver, Options};
use workflow_graph::{ExecuteError, Executor, Launch, MockTes, Outcome, Task, execute};

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
    let mut executor = MockTes::new(Path::new(&launch.scratch).join("mock"));
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
fn mock_files_of_bare_wildcards_take_the_extension_the_output_suggests() {
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
    let outcome = |assume_inputs: bool| {
        let launch = Launch {
            target: main.clone(),
            inputs: Some(job.clone()),
            scratch: folder.path().join("scratch"),
            assume_inputs,
            ..Launch::default()
        };
        let mut driver = cwl_runtime::driver(&launch).map_err(|error| error.message)?;
        let mut executor = MockTes::new(folder.path().join("mock"));
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
        assume_inputs: true,
        ..Launch::default()
    };
    let mut driver = cwl_runtime::driver(&launch).expect("a driver");
    let mut executor = MockTes::new(folder.path().join("mock"));
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
fn a_mock_run_says_when_a_scatter_needs_real_data() {
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
        assume_inputs: true,
        ..Launch::default()
    };
    let mut driver = cwl_runtime::driver(&launch).expect("a driver");
    let mut executor = MockTes::new(folder.path().join("mock"));
    let error = execute("main", driver.as_mut(), &mut executor)
        .error
        .expect("the scatter fails")
        .to_string();
    assert!(
        error.ends_with(
            "pick: the scattered input `peak` is null, not a list; the mock run has no real \
             data to fill it"
        ),
        "{error}"
    );
}

/// Finishes the tasks that do nothing at once and without files, as real executors do, and
/// pretends the others like the mock.
struct Real {
    mock: MockTes,
}

impl Executor for Real {
    fn execute(&mut self, task: &Task) -> Result<Outcome, ExecuteError> {
        if task.is_no_op() {
            return Ok(Outcome {
                id: task.id,
                ..Outcome::default()
            });
        }
        self.mock.execute(task)
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
        assume_inputs: false,
        ..Launch::default()
    };
    let mut driver = cwl_runtime::driver(&launch).map_err(|error| error.message)?;
    let mut executor = Real {
        mock: MockTes::new(folder.path().join("mock")),
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
