<!-- How uwe runs wir documents: detection, inputs, native blocks, outputs and limits. -->
<!-- Copyright (c) 2026 The uwe Contributors -->
<!-- SPDX-License-Identifier: MIT or Apache-2.0 -->

# Running wir documents

wir is the workflow intermediate representation of uwe: one typed graph format that workflow languages are lowered into. The format, its types and its verifier are described in [wir/README.md](../wir/README.md). This page describes how `uwe run` runs a wir document, through the `wir-runtime` crate.

```console
$ uwe run run.wir.json -inputs inputs.json
```

## Lowering CWL

`uwe lower` prints the wir document of a CWL file, with the source locations from the current folder, so the document belongs there:

```console
$ uwe lower workflow.cwl > workflow.wir.json
uwe: op align stays a native CWL op: step align: scatter
```

`uwe run workflow.cwl -lowered` runs a CWL document through its lowered wir document, which uwe writes below its scratch folder; `uwe graph -lowered` shows its graph. An embedding host sets `run::Settings.lowered`; the inputs are then checked with the schema of the CWL document. The crate `cwl-lowering` does the lowering:

- A CommandLineTool becomes a `task` op when the task builds the same command: literal base command and arguments, inputs on the command line as one word each (scalars, Files, Directories, enums, and arrays of them without a prefix), `stdout` and `stderr` files, outputs of `stdout` and `stderr` type and outputs read by one literal glob. `DockerRequirement` with `dockerPull` sets the image, else it is `debian:stable-slim`; `NetworkAccess` sets `network`; `ResourceRequirement`, `ToolTimeLimit` and `WorkReuse` set the attributes.
- A Workflow becomes a graph: an op per step, at the path of the step id, and an edge per link with one source. A step that runs a Workflow becomes a `call` op with a graph of its own, so its tasks have paths like `inner/wc`, as in the CWL run.
- A scattered step becomes a `map` op over the scattered inputs, with the mode of its `scatterMethod`, whose body graph runs the op of the step. Its tasks have paths like `say/0/say` and the step `say/say`, where the CWL driver names them `say[0]` with the step `say`.
- A step with `when` becomes an `expr` op `<step>.when` that computes the condition from the step inputs, with the `expressionLib` in scope, and a `branch` op at the step path whose body graph runs the op of the step, so its task has the path and step `say/say`. A skipped step gives null outputs. With `scatter`, both are the body of the `map`, so the condition holds per round and a skipped round gives null at its place.
- The `default` of an input of a workflow, a tool or a step becomes the `default` of the input ports that take it; a step default wins over the default of its tool. A File default keeps its location, relative to the folder of the wir document when it lies below it.
- The `valueFrom` of a step input becomes an `expr` op `<step>.<input>` before the op of the step, inside the `map` of a scatter and before the condition of `when`, as CWL orders them. It sees the step inputs as `inputs`, before any `valueFrom`, and the value of its own input as `self`; a Directory has no `listing` there.
- The `valueFrom` of a tool input binding without a prefix or `itemSeparator`, on an input that is not optional, becomes an `expr` op `<tool>.<input>.valueFrom` that fills the task input `<input>.valueFrom` on the command line. It sees the tool inputs and the value of its input as `self`, and gives text, numbers or a list of them, which CWL and a task write alike; a bool or an object fails the op. Other bindings with `valueFrom` stay native.
- A link with several sources, `linkMerge` or `pickValue`, of a step input or a workflow output, becomes ops `<step>.<input>.merge` or `<output>.merge`, then `.flatten` for `merge_flattened`, then `.select` for `pickValue`, in the order of uwe's CWL driver: several sources merge nested by default, and `pickValue` picks among the items of an array value.
- An ExpressionTool becomes a `call` op of a graph with an `expr` op per output, at the path of the output, which takes its field of the object that the expression gives, or null when the object has none. As an `expr` reads no files, an input that loads its contents or listing keeps the tool native.
- Anything else stays a `native` CWL op that runs the process as a whole, and `uwe lower` names it with the first thing that cannot be compiled into wir yet: expressions in tools that read the paths of files or listings that CWL loads, `loadContents` of inputs, Operations, and requirements such as `InplaceUpdateRequirement`. A workflow keeps its steps only when every step lowers; otherwise the whole workflow is one native op.

A tool compiled into wir runs as a wir task that the runner builds as the CWL driver builds the task of the tool: it works in `/var/spool/cwl`, sets `HOME` and `TMPDIR`, stages each input file in `/var/lib/cwl/inputs/<n>` and quotes its shell commands the same way, so its cache key and resume fingerprint are those of the same tool run as CWL. They differ when a job lists the file inputs in another order than the tool, as the CWL driver numbers the folders in the order of the job, and in a real run, where the CWL driver also asks for `cwl.output.json`. `uwe/tests/lowering.rs` compares the tasks on the simulator and the outputs in Docker, and `cwl-lowering/tests/runs.rs` compares the commands and outputs of both runs on the simulator. Two runs of the same compiled document share cache keys and resume fingerprints.

Not compiled the same way: a compiled tool ignores a `cwl.output.json` that it writes, and the formats of its inputs are checked as equal text, or not at all when the document has `$schemas` or `$namespaces`, through which CWL matches other formats.

### Coverage

`cwl-lowering/tests/conformance.rs` lowers each of the 378 tests of the CWL v1.2 conformance suite and runs it natively and lowered on the simulator; `CWL_TEST_WORKFLOWS` names the folder of the test workflows, and `CWL_TEST=1` prints the outcome of each test. It compares the outputs of both runs where both succeed, with Files and Directories by class and basename. On 2026-11-07 (a checkout of suite commit `97908339` with its data files, and the CWL JavaScript helper image):

| Outcome | Tests |
| --- | --- |
| lowers fully | 49 |
| lowers partly: some ops stay native | 289 |
| does not lower: the parser refuses it, or a native op cannot hold an `Any` or `null` type | 40 |
| the lowered run ends as the native run does: both succeed with as many tasks, or both fail | 310 |
| both succeed with equal outputs | 286 |

Without the helper image, 323 lowered runs end as the native run does and 167 give equal outputs: JavaScript then fails in both runs, apart from two tools with a JavaScript `valueFrom` on an input, whose lowered run fails while the simulated native run succeeds.

What keeps most tests partly native, by the first reason of each: `outputEval` or `loadContents` of an output (89), a requirement (74: `InitialWorkDirRequirement` 42, `ShellCommandRequirement` 26, `EnvVarRequirement` 5), an expression in an argument (36), `stdin` (20), a `run` that the document defines twice (13), command line bindings (10), a resource or time limit set by an expression (8) and secondary files or a format of an input (7).

Where the runs end differently, the lowered run refuses a job key that the document does not take, which CWL ignores (`wf_wc_nomultiple`, `listing_outputBinding_loadListing`), and job requirements (`cwl:requirements`); a simulated native op makes up values that do not fit their types, like the `int` of an ExpressionTool that loads the contents of its input (`step_input_default_value`), and a simulated glob of folders makes up files. These are differences of wir runs and of the simulator, not of lowered ops.

## Lowering WDL

`uwe lower` and `-lowered` take WDL 1.0, 1.1 and 1.2 documents too, and `-entry` picks the workflow or task as for `uwe run`. A run with `-lowered` turns its WDL input file into one for the entry graph: the keys lose the name of the workflow, paths become Files and Directories from the folder of the file, and a pair becomes a tuple. A key `<call>.<input>` gives an input that a call does not give, also inside a scatter or an `if` and when the workflow stays one native op. Inside a scatter or an `if`, such a required input is an optional entry input, and an `expr` op `<call>.<input>` fails when the call runs without it. The crate `wdl-lowering` does the lowering:

- A workflow becomes a graph with its inputs and outputs. A literal default of a required input becomes the default of its port; another default is an `expr` op at the path of the input that takes the given value first. An optional input with a default has a Boolean input `given.<input>`, which a call or an input file sets when it gives the input, so that a given `None` stays `None`; a task input does the same in the library of its `expr` ops, and a native WDL op gets these inputs too. A task or workflow with an input, a declaration or an entry call named `given` stays native.
- A declaration becomes an `expr` op in WDL at the path of its name, and `select_first` or `select_all` of plain references becomes a `merge` op and a `select` op. A call becomes a `call` op at the path of its alias; an input that is not a plain reference is an `expr` op `<call>.<input>`, and `after` is a dependency.
- A scatter becomes a `map` op `scatter.<name>`, after the first name defined in its body, and an `if` a `branch` op `if.<name>`, whose condition is an `expr` op `if.<name>.condition` unless it is a plain reference. Outside, the names defined inside are arrays or optional, as in WDL.
- A call of a subworkflow becomes a `call` op of the graph of that workflow, which all its calls share.
- A task becomes a graph `task.<name>` that all its calls share. The `expr` op `command` evaluates the command section after the input defaults and private declarations, which the definitions of the structs they use precede in its library, with each File at the path where the task op stages it; the files of `write_lines` and the other write functions are made by the command before it runs. The `task` op at the task name runs the command with `bash`, as uwe's WDL driver does. An output `stdout()`, `stderr()`, a literal path or `glob("<pattern>")` is a capture. The paths of other file outputs come from the `expr` op `command.outputs`, and the command copies those files below `.wdl/out/<output>/`, where a capture finds them. An output without files is an `expr` op `output.<name>`. Literal `container`, `cpu`, `memory`, `gpu` and `returnCodes` set the image, the resources and the success codes. The task may use the network, as WDL tasks run by uwe may.
- A task stays a `native` WDL op, and `uwe lower` names it, when a task op cannot run it the same way: an output that reads files the task makes, like `read_string(stdout())`, a declaration that reads or writes files, a command that reads files, a runtime value that depends on inputs, `returnCodes: "*"`, `env` declarations, the `task` variable and files inside struct or map inputs. A workflow keeps its statements only when each lowers, so a declaration that reads files or an `else` clause keeps the whole workflow one native op.

A lowered task is a wir task, so its cache key differs from that of the same task run as WDL: its inputs are staged in `inputs/<port>` and the command comes as an argument. On the simulator the outputs of a lowered task are made up from its captures, as the simulator does not see its command: a glob or a copied path gives a made-up name like `simulated.log`.

### WDL coverage

`wdl-lowering/tests/conformance.rs` lowers each of the 170 examples of the WDL 1.2.1 specification that `just wdl-spec-tests` extracts, and runs it natively and lowered on the simulator; `WDL_SPEC_TESTS` names their folder, and `WDL_TEST=1` prints the outcome of each. It compares the outputs of both runs where both succeed, with Files and Directories by class and basename. On 2026-11-08 (`SPEC.md` of openwdl/wdl commit `42c106d0`, the data files of openwdl/wdl-tests commit `082340ad`):

| Outcome | Examples |
| --- | --- |
| lowers fully | 77 |
| lowers partly: some tasks stay native | 73 |
| does not lower: the analysis refuses it (12 of them are meant to fail), or a native op cannot hold an `Object` or a map with float keys | 20 |
| the lowered run ends as the native run does: both succeed with as many tasks, or both fail | 150 |
| both succeed with equal outputs | 137 |

What keeps tasks native, by the first reason of each: an output that reads files the task makes, like `read_string(stdout())` (59), a write function in a declaration (2), a file inside a struct or map input (3), a runtime value that depends on inputs (2), a workflow expression that reads a file (2), and one each of the `task` variable, `returnCodes: "*"`, a read in the command, an array of optional output files and an `env` declaration. The one example whose outputs differ globs files that the simulator makes up.

## Compiling Nextflow

`uwe lower` and `-lowered` take Nextflow pipelines too, with `-profile`, `-c`, `-entry` and the params of the command line as for `uwe run`. A Nextflow document is compiled for the params and config of one run, as Nextflow builds its dataflow once per run from them; the crate `nextflow-compiler` compiles it from the network that the build phase of uwe's Nextflow driver wires up, without running a task:

- A process call becomes a `process` op whose body graph has one dialect task `nextflow_process`, which builds, accepts, retries and publishes each task as the native driver does, so the tasks and their cache keys are those of a native run. An `each` input is an `each` port; a call with a queue channel input has stream outputs, one without has value outputs.
- An operator becomes the stream op of its meaning, such as `transform` for `map`, `route` for `branch`, `join`, `group_tuple` and `collect`; its closures are Groovy callbacks found by their place in the source, with the values they capture. A value channel that an op reads as a stream goes through an `emit` op.
- A workflow call becomes a `call` op of a graph of its own, whose ports are the channels that cross the call. The mix of a topic sits in the deepest call that holds its readers.
- The params become typed inputs of the entry graph, with the types of `nextflow_schema.json` and the values of the run as defaults. The source `run` pins the params and the resolved config by their digests, and the config files of the run are sources with their digests.
- What the build prints or writes, and checks like `validateParameters`, become ops at the start of the run, before every other op. Channel factories, like `channel.fromPath` and factories of plugins, read when the run starts. Files that the build reads, like the samplesheet of `samplesheetToList`, are read when the pipeline is compiled, as Nextflow reads them when it builds the workflow; each is a source of the document with its digest.
- Each workflow output of the `output { }` block becomes an `observe` op with the `publish` effect of the output folder. Its Groovy callback publishes the files of each item with the options of the output, to the places the native driver uses, and writes the index file when the channel closes.
- Constructs without a wir op keep the whole pipeline one `native` op, and `uwe lower` names the first: nf-boost's `scan`, and the typed operators `groupBy`, `join` of records and `combine` of typed workflows, whose results depend on tuple and record kinds that wir values do not keep. A value channel that is never bound, like `channel.value()`, closes empty. An agent process compiles like a process, but its dialect task does not run agents yet.

A run of a compiled document builds, accepts and publishes the tasks of `nextflow_process` ops through the task adapter of the native driver, with the names of the native tasks. Ops with `values: groovy` order, add and print values as Groovy does: natural sorting compares like Groovy's `sort()`, Files by their paths, `hash` and `deep` sorting use the CacheHelper hash of Nextflow, `sum` adds with Groovy's `+` from the first item, and `view` prints Groovy's text of a value. Keys of joins, groups and `unique` compare by the wir rules.

## Compiling Snakemake

`uwe lower` and `-lowered` take Snakefiles and Snakemake project folders too, with the targets, config files and profiles of the command line. The crate `snakemake-compiler` compiles the jobs that the planner of uwe plans for them, as a run plans them:

- Each job becomes a `task` op with a `snakemake_job` builder: the rule is its `call` and the target of the plan its `entry`, its wildcard values are kept as they are, and its path is the rule, then the wildcard values joined by `.`, like `count/a`, with letters that names do not take as `_`. Its `origin` names the rule.
- Each file that one job makes and another reads is an edge with a File, or a Directory for a `directory()` output, from the output `output_<n>` of the maker to the input `input_<n>` of the reader. The ports map to the native selectors `output/<n>`, the outputs, logs and benchmark of the job in this order, and `input/<n>`, its input files. Inputs that no job makes are read where they are.
- An output that the native run delivers to the working directory is a `publish` effect. `temp()`, `pipe()` and `service()` outputs are not.
- The entry graph has the outputs of a native run: the files of the targets, named as `uwe outputs` names them. A WDL `expr` op `result.<name>` gives the files in the working folder once the ops that make them have ended.
- The command, script, wrapper, notebook, `run:` block, conda and container settings, resources, retries, `touch()` and `protected()` stay as the native job builds and checks them: the builder runs the job through the job adapter of `snakemake-runtime`, which prepares its task and takes its outcome with the code of the native driver, so the task, its cache key and its acceptance are those of the native run.
- Jobs that run together in one task, group jobs of a run that is not local, pipes, and a service with the jobs that use it, are one op: its `group` names the other jobs, its inputs are the files that no job of the group makes, and its outputs the files of each job in turn.
- Every op carries the plan settings of the run: several targets, the options `until`, `omit-from`, `forcerun` and `forceall`, and config values of the command line, which a source with a `data:` location holds and its digest pins. The job adapter plans with them as the run would.
- A `cwl:` job is one op whose call runs the tasks of its CWL run, as the native run does; the job ends with the outputs of that run.
- Commands that workflow code runs while the workflow is planned run while it compiles, as hooks run, so that the plan sees their output. A run starts the adapter, which runs them again as tasks before the jobs, and also those that a job runs while it is prepared, like a `conda:` function; the run checks that it plans every compiled job with its group.
- A workflow with a checkpoint is one `dynamic` region whose planner is the Snakemake planner of the job adapter. Its plans hold the jobs that wait for no checkpoint, as tasks with `snakemake_job` builders whose identities are the rule and wildcards; after each checkpoint the adapter plans again, as a native run does, and the plan is final once no needed checkpoint is pending. The region holds the targets of the launch, but not config values of the command line or the options that pick jobs, so such a workflow does not compile. The region gives no target files as outputs yet.
- The whole workflow stays one `native` Snakemake op, and `uwe lower` names the first reason, when a group runs jobs of several container images, which a run splits into separate tasks, and when uwe cannot plan the workflow, like a storage plugin that uwe lacks. Such a native op cannot hold config values of the command line, several targets or the options that pick jobs, so a workflow with them does not compile. For a Snakefile, its source is the working folder of the launch, so that it starts there as a native run does; a Snakefile that its working folder does not find itself does not compile.

The wir runtime runs the jobs of a document through one job adapter for the Snakefile, its profiles and config files, which `wir_runtime::snakemake::register` adds to a run. Job tasks keep their native names.

### Snakemake coverage

`snakemake-compiler/tests/differential.rs` compiles each test workflow of `just snakemake-test-workflows`, runs it natively and compiled through the wir runtime on the simulator, and compares the tasks of both runs (their descriptions, cache keys, resume fingerprints and acceptance) and the target files that the native run exports with those that the outputs of the compiled entry graph name. `SNAKEMAKE_TEST_WORKFLOWS` names the folder of the test workflows, and `SNAKEMAKE_COMPARE=1` prints the outcome of each. On 2026-11-08 (the commits of `snakemake-parser/tests/fixtures/test_workflows_revisions.txt`, of which two repositories no longer exist), each workflow with a time limit of 300 seconds:

| Outcome | Workflows |
| --- | --- |
| test workflows | 890 |
| the native run succeeds | 633 |
| compiles fully | 666 |
| stays one native op | 3 |
| does not compile: planning fails as the native run fails (219), or a region cannot hold the launch (1) | 220 |
| the compiled run ends as the native run does: both fail, or both give the same tasks | 666 |
| both succeed with the same tasks, cache keys, resume fingerprints and acceptance | 632 |
| both succeed with the same target files | 594 |

The 38 runs with the same tasks and other target files are checkpoint regions, which give no target files yet. What keeps a workflow native: a group whose jobs run in several images (1), and features that uwe does not run yet (2). The one that does not compile for its launch sets config values on the command line beside a checkpoint. `test_many_jobs`, with 150,000 jobs, hits the time limit, as its native run does.

## Detection

uwe takes a file as a wir document when it is JSON and has a top-level `wir_version` key. The file name does not matter, but `<name>.wir.json` is the convention. `-lang wir` picks wir for any file. uwe parses the document, checks its version, runs the verifier and refuses the document with every problem the verifier found before any task starts.

## Inputs

The inputs of the entry graph come from the JSON or YAML file of `-inputs` and from parameters like `--lines 3`. A parameter is read by the type of its port: text for a string, a number for an `int` or a `float`, a path for a File or a Directory, and JSON for anything else. File and Directory values use the JSON form of wir, `{"class": "File", "location": ...}`; a relative `location` or `path` in the inputs file starts from the folder of that file. An input without a value is null when its type is optional; otherwise the run stops before any task starts. An `int` that reaches a `float` port becomes a float.

## Ops

An op starts once every connected input is bound and every `after` dependency holds. An op whose input can no longer be bound, or whose dependency can no longer hold, ends `omitted`. A `call` op runs its graph with the call's inputs; its tasks have paths below the path of the call, like `align/sort`.

A failed or errored op fails the run, unless an `after` dependency of another op waits for that status. A failure inside a call fails the call. After a failure that fails the run, no new op starts unless the run has `-keep-going`; running ops end first.

A `map` op makes its rounds when it starts: round i is a call of its body at `<map>/<i>`, and the tasks inside keep the path without round numbers as their step, like `each/cat` for `each/0/cat`. A failed round fails the map. A `map` with `parallelism` or `min_success` stops the run with exit code 33.

An `expr` op runs in uwe, without a task, with the CWL evaluator of uwe: parameter references directly, JavaScript in the helper that CWL runs use. A `branch` op runs its body like a call when its condition is true, else it ends skipped with null outputs and its tasks do not run.

`select`, `merge` and `flatten` run in uwe, without a task, as the wir README says; `merge` and `flatten` on streams stop the run with exit code 33. Reserved op kinds, like `collect`, verify but do not run: uwe stops with exit code 33.

## Task ops

A `task` op becomes one uwe task, with the op path as its step and name. It runs in its image in the working folder `/work`. Each File or Directory input is staged at `inputs/<port>/<basename>`, an item of an array at `inputs/<port>/<index>/<basename>`, with its secondary files beside it. The command is built as the wir README says, without a shell; a `stdout` or `stderr` capture, or a stream file the task names, runs it through `/bin/sh` with the stream written to that file, else to `<output>.stdout` or `<output>.stderr`.

An attempt is accepted only when its exit code is in `success_codes` (only 0 when empty) and every capture fits its output: a glob matches one file for a File or a Directory, any number for an array, none only for an optional output. An attempt that is not accepted runs again while `retry` allows; then the op fails. A task that could not run, and no machine retry can mend, errors. The attributes `resources` and `time` set what the task asks for and `cache: never` keeps it from the cache; none of them enters its cache key. A task op runs without network, so runs can share its results, unless it sets `network: true`, like a CWL tool with `NetworkAccess`.

## Native blocks

A `native` op runs a whole document of another language with the driver that uwe has for that language. This version runs CWL, Nextflow, Snakemake, Argo and WDL blocks. The op's inputs become the job of the CWL run, by port name, and the CWL output object becomes the op's outputs, by port name; each output must fit its port type, or the op fails. The block keeps its own rules: its tasks, cache keys, resume fingerprints, acceptance, retries, timers and deliveries are those of the same CWL document run alone. Its tasks keep their step names, so `-no-cache-steps echo` works as it does for the CWL document, and their names start with the op path, like `block: echo`.

The `source` location of a block is relative to the wir document, or a URL. A URL of a repository whose `digest` is `git:<commit>` is fetched at that commit, as `uwe run <URL> -r <commit>` fetches it, and the block runs the checkout. `entry` names the process of a packed CWL document, like `main`. A block delivers its output files to a folder of its own, the op path below the output folder of the run (`-output-dir`, else the current folder), so two blocks do not overwrite each other's files. The ops that wait for a block start after its files were delivered, so a block that reads the files of another finds them. The job file of a block is written below the scratch folder of uwe. No block changes the working directory of the process.

The `cache: never` attribute of a block turns off cached results for its tasks. It only makes reuse stricter: a CWL `WorkReuse` with `enableReuse: false` stays as it is.

## Nextflow blocks

A Nextflow block runs a Nextflow script, or the main script of a project folder, as `uwe run` runs it alone. `entry` is the workflow of `-entry`; without it the unnamed workflow runs. `profiles` are the profiles of `-profile`, and `configs` name sources whose files are read like `-c` files, in order. Its tasks, cache keys and resume fingerprints are those of the same run alone, launched in the same folder.

Params: each op input becomes the param of its name, written to a params file like `-params-file`. A File or a Directory becomes its path, arrays and records keep their shape with paths for their files, and a null input stays out, so the default of the pipeline holds. The pipeline checks the params as it does alone.

Launch folder: a block runs as if it was launched in its own folder, the op path below the output folder of the run (`-output-dir`, else the current folder). Relative paths start there: `file()` and `publishDir` paths, `params.outdir`, the `nextflow.config` of the launch folder and `launchDir`. So `publishDir "${params.outdir}/fastqc"` with `outdir` set to `results` publishes to `<output folder>/<op path>/results/fastqc`. Its own files, like operator files and reports, go to a scratch folder of the block. Two blocks of one run never share a folder, and no block changes the working directory of the process.

Results: when the block has finished, each op output takes its value by its name:

1. An emit of the entry workflow with the output's name. A value channel gives its value, or null when it closed empty, and a queue channel the array of all its values, in the order they came, which can follow the order in which tasks finished. A path becomes a File, or a Directory where the output type wants one there. Numbers, strings, booleans, lists and maps keep their JSON form; a value that JSON cannot hold, like a closure, fails the op.
2. Else the files that the block published below the folder that the op input of the same name names, like `outdir`, from the launch folder: a Directory output gets that folder, an array of Files every file published below it, sorted by path, and a File output the one file published there. An optional output is null when nothing was published; otherwise publishing nothing, or several files for a File, fails the op.
3. Else the op fails with a message that the output is neither an emit nor the folder of an input.

The value must then fit the output type, or the op fails. Nextflow emits have no types, so an output that takes any value has the type `dynamic`; wir allows `dynamic` only on the outputs of Nextflow blocks. A `dynamic` value must fit the type of each op input and graph output that it reaches, or that op fails, or the run fails for an output of the entry graph. In a simulated run the files of emits are made up, and the published files are the targets of the publish rules, which nothing writes.

## Snakemake blocks

A Snakemake block runs a Snakefile, or the Snakefile of a project folder, as `uwe run` runs it alone. `entry` names one target, a rule or a file, like a target on the command line; without it the first rule is the target. `configs` name sources whose files are read like `--configfile` files, in order, and `profiles` name profiles: an absolute path, a path below the block's own folder (the op path below the output folder of the run), else a workflow profile that Snakemake finds as it finds `--workflow-profile <name>`, in the working folder, its `profiles` folder or the `profiles` folder beside the Snakefile. Its tasks, cache keys and resume fingerprints are those of the same run alone, started in the same folder.

Config: the op inputs become a config file, read after the `configs` like a last `--configfile`, so they win. Each input becomes the key of its name: a File or a Directory becomes its path, arrays and records keep their shape with paths for their files, and a null input stays out, so the default of the Snakefile holds.

Working folder: a block of a Snakefile works in its own folder, the op path below the output folder of the run (`-output-dir`, else the current folder), as if Snakemake was started there. Relative paths of the Snakefile start there, and its files, its `.snakemake` folder and its lock are there. A block of a project folder works in that folder, as `uwe run <folder>` does, so two blocks of one project folder share it. No block changes the working directory of the process.

Results: when the block has finished, each op output takes the files of the target that `uwe outputs` names alike. For a target rule these are its named outputs, or its named inputs when it has no outputs, like those of `all`; every other file of the targets, and a target file, goes into the output `targets`. A name that one item takes once is a File, or a Directory where the Snakefile marks it `directory()`; a name of several files, like `expand(...)`, is an array of them, in the order of the rule. So `rule all: input: reports=expand("results/{s}.txt", s=SAMPLES)` gives the output `reports`, an array of Files. The files are at their paths in the working folder. An output that the targets do not name is null, and fails the op unless its type is optional; the value must fit the output type. In a simulated run the files are named but not made.

Checkpoints work inside a block: the block plans again when a checkpoint has run, and ends only when the jobs of that plan have run.

## Argo blocks

An Argo block runs the Workflow of an Argo file, as `uwe run` runs it alone. `entry` names the template to start with instead of `spec.entrypoint`, and `configs` name sources whose files hold the WorkflowTemplates and ClusterWorkflowTemplates that `templateRef` finds, like files given with `-c`. Argo has no profiles: a block with `profiles` stops the run with exit code 33. Its tasks, cache keys and resume fingerprints are those of the same run alone, with `-output-dir` set to the folder of the block.

Parameters: the op inputs become a parameter file, like `argo submit --parameter-file`, so each input sets the workflow parameter of its name. A string stays as it is; a File or a Directory becomes its path, and other values their JSON text. A null input stays out, so the default of the workflow holds.

Output folder: the output artifacts of the steps are delivered to `<output folder>/<op path>/<step>/<artifact>`, as a run alone delivers them to `-output-dir`. A block takes no requests of `uwe resume`, so a real run of a block cannot suspend a step without a duration.

Results: when the block has finished, each op output takes the output of the same name, as `uwe outputs` names them: the output parameters and artifacts of the entrypoint, then the global outputs by their `globalName`. An output parameter is a string. An output artifact is a File, or a Directory when it is a folder, named by the basename of its path in the container, like `loud.txt` for `/tmp/loud.txt`; an artifact that the workflow holds as raw text gives null. An output that the workflow does not give is null, and fails the op unless its type is optional. In a simulated run the parameters read from made-up files are empty.

## WDL blocks

A WDL block runs the workflow of a WDL document, or the task of a document without one, as `uwe run` runs it alone. `entry` names the workflow or task to run. WDL has neither profiles nor config files: a block with `profiles` or `configs` stops the run with exit code 33. Its tasks, cache keys and resume fingerprints are those of the same run alone, with `-output-dir` set to the folder of the block.

Inputs: the op inputs become the input object of the run, like an inputs file: the input `x` sets the input `x` of the workflow, as `<workflow>.x` does, and an input whose name has a dot, like `align.threads`, sets the input `threads` of the call `align`. A File or a Directory becomes its path, arrays, maps and structs keep their shape with paths for their files, and a null input stays out, so the default of the workflow holds. A `Pair` input takes a record of `left` and `right`.

Output folder: the files of the outputs are delivered to `<output folder>/<op path>/<output>/`, numbered for arrays, as a run alone delivers them to `-output-dir`.

Results: when the block has finished, each op output takes the workflow output of the same name, without the name of the workflow: `all` for `scatter_names.all`. A File or a Directory is at its delivered path, a pair is an array of two, so a wir tuple of two takes it, and the other values keep the JSON form of WDL. An output that the workflow does not give is null, and fails the op unless its type is optional; the value must fit the output type.

## Outputs

A real run that succeeds delivers the files and folders of the entry graph's outputs that are not in the output folder yet, like those of task ops, to the output folder by their basenames. When a later file or folder would get a place that another one has, it goes to a numbered folder, `<name>-2/<name>` and so on, as the CWL driver delivers outputs, so the items of a `map` output with one name keep theirs. When one would go to a place that holds, or lies in, the place of another output or of a file that a native block delivered, the run fails before it delivers anything, and names those places. It prints the outputs as JSON, like CWL prints its output object, unless `-no-print` is given. An embedding host gets the same object as `Run::outputs`. A simulated run has no outputs.

## Provenance

The run database records a wir run with the language `wir`, and its run crate names the language wir.

## Not yet

- Compiling expressions of CWL tools that read the paths of files or the listings that CWL loads, `loadContents` of inputs, and `InplaceUpdateRequirement`; such tools stay native ops.
- Equal cache keys of a compiled tool and the same tool run as CWL in a real run, where the CWL driver asks for `cwl.output.json`, and for jobs that list file inputs in another order than the tool.

- `uwe resume` of the suspended steps of an Argo block.
- Several targets in one Snakemake block; a rule that asks for them can stand in.
- Workflow outputs of the `output { }` block of Nextflow as op outputs by their names; their files count as published files below their folder.
- A remote output folder for Nextflow blocks, and the launch folder in `${launchDir}` of include paths and in relative `conda` files, which still start from the working directory.
- The attributes `place` of every op and `retry` and `time` of native ops; `cache: resume`.
- Input descriptions for `uwe inputs` and strict admission of embedded runs, and a record of the document's sources and version in the run crate.
