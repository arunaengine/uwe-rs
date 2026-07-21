<!-- How `iunctus run` runs a workflow of any language: detection, drivers, the execution graph and executors. -->
<!-- Copyright (c) 2026 The iunctus Contributors -->
<!-- SPDX-License-Identifier: MIT or Apache-2.0 -->

# The engine

Status: implemented for Nextflow, Snakemake, CWL and Argo, 2026-09-29; tasks run in parallel on Tokio since 2026-09-30. This applies [decisions](../decisions.md) 15, 16 and 21, and decisions 11 and 22 for real executors.

`iunctus run <target>` works the same way for every language:

1. It finds the language of the target (section 2).
2. It turns the command line into a `Launch`, the options all languages share.
3. It asks the language's crate for a `Driver` for that launch, and checks the inputs that the driver describes before any task starts ([the inputs of a workflow](inputs.md)).
4. `workflow_graph::Engine` runs the driver against an `Executor` with the run options (section 5) and records the execution graph (section 6). It tells an observer each step of the run (section 5); the binary's observer records the run in the run database ([runs](runs.md)).
5. With `-graph <file>` it writes the graph; `iunctus graph <target>` prints it. A simulated run says so on standard error, and its graph says so too ([the graphs of a run](graph.md)).

iunctus is one binary that needs no other program (decision 26): containers run through the Docker Engine API, Kubernetes Jobs through the Kubernetes API, TES tasks and registries over HTTP. Only the SLURM executor uses the tools of the cluster (`sbatch`, `squeue`, `sacct`, `scancel`), which every SLURM node has.

The engine, the driver interface, the executor interface, the graph and the simulator live in the `workflow-graph` crate, which knows no language. Language crates depend on it, never the other way round. The real executors live in the crates `task-model`, `content-store`, `executor` and `task-runner` (section 4).

## 1. The command line

| Option | Meaning | `Launch` field |
|---|---|---|
| `<target>` | workflow file or project directory | `target` |
| arguments after the target, `-entry <name>` | Snakemake targets, the Nextflow entry workflow, the Argo entrypoint | `targets` |
| `-lang nextflow\|snakemake\|cwl\|argo` | the language, when the target does not tell it | (chooses the driver) |
| `--<name> <value>`, `--config <name>=<value>...` | workflow parameters, in the order given | `params` |
| `-inputs <file>`, `-params-file <file>` | a CWL job file or a params file | `inputs` |
| `-profile <names>` | Nextflow profiles | `profiles` |
| `-c <file>`, `-configfile <file>` | extra config files | `configs` |
| `-stub` | run the stubs of tasks | `stub` |
| `-executor simulator\|local\|tes\|slurm\|kubernetes` | the executor (section 4) | (chooses the executor) |
| `-store <folder or address>` | the cache store of the real executors, in a folder or at an address of a storage provider, like `s3://bucket/cache` ([storage](storage.md)) | |
| `-iunctus-config <file>` | a config file of iunctus, read after the site-wide and the user's file ([storage](storage.md), section 1) | |
| `-tes-server <address>` | the TES server of `-executor tes`; `http://localhost:8000` by default ([executors](executors.md), section 3) | |
| `-slurm-container apptainer\|singularity\|podman\|docker` | how the nodes of `-executor slurm` run containers; `apptainer` by default ([executors](executors.md), section 4) | |
| `-slurm-options '<options>'` | more options of `sbatch`, like `--account=lab` | |
| `-kubeconfig <file>` | the kubeconfig of the cluster of `-executor kubernetes`, which it needs; iunctus never takes the default one ([executors](executors.md), section 5) | |
| `-kubernetes-namespace <name>` | the namespace of the Jobs; the one of the kubeconfig's context by default | |
| `-default-image <image>` | the image of tasks that name none, for real executors (section 5) | |
| `-max-pending <n>` | how many tasks may be pending at once; 1 for the simulator, no limit for real executors (section 5) | |
| `-order asked\|reversed\|finished\|shuffled[:<seed>]` | the order in which pending tasks finish; `asked` for the simulator, `finished` for real executors | |
| `-graph <file>` | write the graph: DOT for a `.dot` name, SVG for `.svg`, a summary for `.txt`, else JSON | |
| `-output-dir <folder>` | where CWL outputs (the current directory by default), Argo output artifacts and the Nextflow `output` block (`results` by default) go | `output_dir` |
| `-format json\|dot\|summary\|svg` | the output of `iunctus graph`; `summary` by default | |
| `-view tasks\|steps` | the graph with one node per task (the default) or one node per step | |
| `-resume [<run>]` | continue a run without running its finished tasks again ([runs](runs.md), section 3) | |
| `-page` | serve the web page of the run ([runs](runs.md), section 4) | |
| `-database <file>` | the run database; `runs.sqlite` in the cache directory by default | |

Parameter names reach the driver as written; Nextflow turns `--max-cpus` into `params.maxCpus`, as `nextflow run` does. `iunctus config` stays Nextflow only.

## 2. Detection

`-lang` wins. Otherwise:

| Target | Language |
|---|---|
| a directory with `main.nf` or `nextflow.config`, a `.nf` file | Nextflow |
| a directory with `Snakefile`, `snakefile`, `workflow/Snakefile`, `workflow/snakefile` or a `.smk` file; a file with one of those names, a `.smk` or `.snakefile` file | Snakemake |
| a `.cwl` file; JSON or YAML with a top-level `cwlVersion` | CWL |
| JSON or YAML with an `apiVersion` of `argoproj.io/...` | Argo |

A directory with both a Nextflow and a Snakemake workflow, or a file that matches nothing, is an error that asks for `-lang`.

## 3. Drivers

A driver is a state machine (`workflow_graph::Driver`): `start` loads the workflow and returns the first tasks, `complete(outcome)` returns the tasks that can run now, and `finish` checks that nothing waits for input that never comes. `inputs` describes the inputs of the workflow as a JSON Schema with the values of the run, which the binary checks before the engine starts the driver ([the inputs of a workflow](inputs.md)). `notes` gives what the driver noticed while it planned, like reads of task outputs, which the graph turns into markers ([the graphs of a run](graph.md), section 3). Each language crate exposes

```rust
pub fn driver(launch: &Launch) -> Result<Box<dyn Driver>, DriverError>
```

and the binary picks one in `driver` in `iunctus/src/main.rs`, a single match on `Language`: `nextflow_runtime::driver` (through `iunctus/src/nextflow.rs`), `snakemake_runtime::driver`, `cwl_runtime::driver` and `argo_runtime::driver`. A `DriverError` with `unsupported` set ends the command with exit code 33. With the simulator the binary sets `Launch.simulated`, so that a workflow plans all its tasks without its data; drivers log the files they assume.

Drivers look up outcomes by task id, so they must not depend on the order in which tasks finish. How the Argo driver works is in [the Argo runtime](argo/runtime.md).

**The Nextflow driver** (`nextflow_runtime::driver`) loads the config (project, launch directory and `-c` files, profiles, the params file and the command-line params over it), runs `manifest.mainScript` or `main.nf` of a project directory, and starts the dataflow network. The binary gives it a host that downloads remote files into the cache. Each Nextflow task becomes a container task:

- `image` from the `container` directive, `command` from the `shell` directive (default `/bin/bash -ue`) with `.command.sh`;
- `script` is the task script; the `.command.sh` input holds it with the code that captures `env` and `eval` outputs into `.command.env`, which is then an output too;
- each staged file is an input with its source path or URL;
- `resources` from `cpus`, `memory`, `disk`, `time` and `accelerator`, `environment` from the config's `env` scope.

Outcomes go back the other way: output files by name, standard output, and the `env` and `eval` values read from `.command.env`. A value that a task did not write, as with the simulator, is empty. As in Nextflow, an output glob does not match the staged inputs unless the output says `includeInputs: true`; a name without wildcards may name one. An image without a registry of its own gets the registry of the container engine in front, like `docker.registry = 'quay.io'` of nf-core. `-entry` is not supported yet. An `exec:` section runs in the interpreter when its task is asked for, as evaluation (decision 17); its task has no image and no command, so the engine finishes it at once, and the tasks that read its files wait for it ([embedded code](snakemake/embedded-code.md), section 6).

**The Snakemake driver** gives the code of jobs what Snakemake gives it: scripts, wrappers and notebooks get the `snakemake` object through a preamble in their language, and `run:` blocks run in the task's container in a module with the Snakefile's globals that they use ([embedded code](snakemake/embedded-code.md)). Every task gets the folders of its outputs made, its threads in `OMP_NUM_THREADS` and the like, and, in a real run, `touch()` outputs made by the driver and Snakemake's check for missing outputs.

When a task fails, its `errorStrategy` decides, evaluated after the failure with `task.exitStatus` and `task.attempt`, as nf-core's base config needs: `retry` asks for the task again with the same id and the next attempt, whose directives are evaluated again (like `memory = { 6.GB * task.attempt }`), up to `maxRetries` (1 by default); `ignore` goes on without the task's outputs; `finish` starts no more tasks and fails the run once the running ones have ended; `terminate`, the default, fails the run at once. A retry and an ignored failure print a `NOTE` line, as Nextflow does.

A finished task's outputs are published by its `publishDir` directives (section 4, delivering results): each output file whose name matches the `pattern`, under the name that `saveAs` gives, in the folder of `path`, with the mode: `symlink` (the default) is a link, `rellink` a relative link, `link` a hard link, and `copy`, `copyNoFollow` and `move` are copies, since the store keeps its files. `enabled: false` leaves a rule out, `overwrite: false` keeps a file that is there. The rules come from the process and from the config, where a selector like `withName` replaces the process's own.

## 4. Executors

| `-executor` | What it does |
|---|---|
| `simulator` (default) | Writes the TES v1.1 task document of each task to `<cache>/scratch/simulator/<run>/tes/<id>.json` and pretends to run it in `work/<id>/` (decision 21). |
| `local` | Runs each task in Docker or Podman through the task runner, which reuses results from the cache store (`-store`). |
| `tes` | Runs each task as a task on a GA4GH TES server (`-tes-server`). |
| `slurm` | Runs each task as a SLURM batch job, with Apptainer, Singularity, Podman or Docker on the nodes (`-slurm-container`, `-slurm-options`). |
| `kubernetes` | Runs each task as a Kubernetes Job, in the cluster of the kubeconfig that `-kubeconfig` names (`-kubernetes-namespace`). |

How the real executors work, and how to set each one up: [executors](executors.md).

An executor implements `workflow_graph::Executor`:

```rust
pub trait Executor {
    fn run(&mut self, attempt: Attempt) -> Running;
}
```

`Attempt` holds the task (`Arc<Task>`), the attempt number (from 1) and a `CancellationToken` that fires when the run stops. `Running` is the future of the outcome, which the engine drives on its Tokio runtime, or an outcome that the executor made at once (`Running::ended`), as the simulator does on the thread of the driver.

**Pretending** (`workflow_graph::simulator::pretend`) writes inputs with literal content, then makes each output. A small simulation of the task's shell script (`workflow_graph::simulator::shell`) tells which files the script makes and what text it writes into them: `touch`, `mkdir -p`, `echo`, `printf` and `cat` with a here document into files, `cp`, `mv`, `ln`, `gzip`, `tar`, `zip`, brace expansion and `for` loops over words. An output is each made file that its pattern matches. When nothing matches, it is one empty file named after the pattern (`*` becomes `simulated`), or a directory for a pattern that ends with `/`. A `.gz` file without text is an empty gzip file, and `versions.yml` gets the one entry nf-core code reads (`simulated: 1.0.0`). The simulator pretends every task, also a no-op task (section 5), so that a simulated run gets the files its driver expects.

**Real executors** share one layer, `task_runner::RunnerExecutor`: it makes the checks of section 5, pins each image to its digest once per run, and hands each attempt to the task runner (`task_runner::Runner`), which runs it on a backend (`executor::Backend`). The runner is cheap to clone, so that the attempts of a run run at the same time. The backend of `-executor local` is `executor::LocalContainer`.

- `Runner::request` turns the `Task` into a `TaskRequest`. Its `TaskSpec` (format 2) is what the task computes and gives the cache key: the image, the command, the work directory, the environment, each input by path and content digest, the output patterns and the digest of the standard input. Literal content goes into the store. Local files and directories are hashed where they are; a backend on another machine (`Settings::import_inputs`) gets them copied into the store too. The identity of a request is `<step>-<id>`, unique in the run; scratch directories and stored outputs are named after it, never after the display name.
- A stored result answers the request when the image is pinned to a digest (scope `shared`). Otherwise the attempt runs in `<cache>/scratch/<run>-<identity>-<attempt>/`, a name no other attempt uses: `work/` is the work directory, `control/` holds the wrapper and its files, and `mounts/` holds copies at absolute paths.
- **Remote inputs**: an input at an address comes through the storage provider that serves it ([storage](storage.md)): a provider of the config file, with its endpoint and credentials, or a built-in one for web addresses and public `s3` and `gs` buckets (read over HTTPS, as anonymous clients do). It is downloaded once into the folder of downloads in the cache, under the SHA-256 of its public address; later runs, and workflow code that reads the same file, take it from there. An address that no provider serves, and remote directories, are not supported (exit code 33).
- **Staging** (`executor::stage`): an input is a symbolic link to where its content is, a local file, a directory of the task that made it, or a file of the store, and the container sees the folder of that source read-only at the same path. So nothing is copied, and a task cannot change its inputs. A task gets its own copy only of literal content, of inputs it may change (`writable`) or run (`executable`). An input at an absolute path is mounted there read-only.
- **The container engine** (`executor::ContainerEngine::find`) is the one that `IUNCTUS_ENGINE` names (`docker`, `podman` or a socket), else Docker when its socket answers, else Podman through its Docker-compatible socket; no config is needed, and iunctus talks to it through the Docker Engine API, not through its command. The container runs as the current user: as that user with engines that run as root, and as root inside with engines without root, whose root is the user.
- **CPUs and memory** (`executor::Pool`): the tasks share the CPUs and memory of the machine. An attempt waits for its share (its `cpus`, and its `memory` if it names one), in the order the tasks asked, and its container gets that share of CPUs and memory as its limits. A task that asks for more than the machine has gets all of it.
- **Time and stopping**: a task that runs longer than its `time` is stopped (with 10 seconds of grace) and ends with the container's exit code and a note in its standard error. When the run is cancelled, the container is stopped at once and the attempt ends with `Cancelled`. The scratch directory of an attempt that fails or is stopped is removed.
- **Exit codes**: the wrapper records the tool's exit code. A container that could not start or could not run the wrapper (exit code 126 or 127, as for an image without Bash), or an end without the tool's exit code, is a failure of the machine (`ExecuteError::Machine`), which the engine runs again; the error quotes the last lines of the container. A container that the kernel killed for lack of memory gets a note that says so. A container killed by a signal, like 137 for lack of memory, ends the task with that exit code, as Nextflow reports it, so that the driver's error strategy decides.
- The wrapper (`executor::wrapper`) runs the command with standard output and error captured in `control/`, so that output patterns cannot match the capture files, and with `control/stdin` as standard input when the task has one. After a success it collects the outputs: it expands braces in each pattern (Bash does not expand braces in a variable), matches with `globstar`, `nullglob` and `dotglob`, lists a directory as one entry, and copies an output at an absolute path to `control/root/`, so that it outlives the container.
- The runner stores each output under `runs/<run>/<identity>/<attempt>/work/<path>`, or `.../root/<path>` for an absolute path. A local store moves the file there and hard-links its content into `cas/`, so nothing is copied; other stores get it uploaded in parts. A directory output is stored as its files and a tree. The captured streams are stored by content. Then the scratch directory is removed, and a successful result becomes a cache entry.

## 5. Running tasks

The engine keeps the tasks that the driver asked for in a queue, in the order asked. A task can start when every task in its `after` list has finished; a finished task is one whose outcome the driver has heard. An `after` entry must name a task that the driver asked for before, earlier in the same list or in an earlier call; any other id ends the run with an error. So no task can wait for a task that never comes.

The engine starts an attempt of each task that can start, up to `max_pending`, and drives it on Tokio; the driver stays on its own thread, because the Nextflow and Argo drivers hold `Rc`. When the attempt that the order chose has ended, the driver hears its outcome, and the tasks it asks for join the queue. `Engine::new(&mut executor).options(RunOptions { max_pending, order }).run(workflow, &mut driver)` runs a workflow; `execute` and `execute_with` are shortcuts. The options:

- `max_pending`: how many tasks may be pending at once: started, but the driver has not heard of their outcome yet. It is 1 for the simulator and has no limit for real executors, whose machine or cluster then sets how many run at once.
- `order`: which pending attempt finishes next. `Finished` is the one that ended first, as real runs go. `AsAsked` is the oldest, `Reversed` the newest, and `Shuffled { seed }` a random one from a small seeded generator (SplitMix64). These three do not depend on when attempts end: the engine chooses an attempt, then waits for it. So the same seed gives the same run again, and tests find drivers that depend on the order of outcomes: `RunOptions { max_pending: 8, order: Order::Shuffled { seed: 1 } }`, or `iunctus run -max-pending 8 -order shuffled:1`.

**Attempts and retries.** Each attempt of a task gets the next number, from 1, which the outcome carries (`Outcome.attempt`), with its start and end times, the image it ran in and whether the store answered it (`cached`). When the machine fails (`ExecuteError::Machine`: the container engine, a TES server, a cluster), the engine runs the task again, twice by default (`Engine::retries`); the driver does not hear of the failed attempt. A task that fails with an exit code goes to the driver, which decides: to retry it, the driver asks for the task again with the same id, in its answer to the outcome. The graph keeps one node per task, and the tasks after it wait for the new attempt.

**Stopping.** `Engine::cancel(token)` stops the run when the token fires; `iunctus run` fires it on Ctrl-C (a second Ctrl-C ends iunctus at once) and exits with 130. When a run stops, for Ctrl-C or an error, the engine cancels the attempts in flight and waits up to a minute for them to end.

**Observers.** `Engine::observer` gets each step of the run (`workflow_graph::Event`), before its effect: `Requested` with the task and its edges, `Started` with the attempt, `Finished` with the outcome (`retried` when the engine runs the task again after a failure of the machine), `Delivered` for each result delivered (below), and `Ended`, on every path, with the error if there is one. The run database is such an observer.

**Delivering results.** The store keeps what tasks make, so a run must deliver its results to where each language expects them. A driver asks for it with `Driver::deliveries`, which the engine calls after `start`, after each `complete` and after `finish`: each `Delivery` names the location of an output, its target, and the mode, a copy (the default, which file systems like btrfs and XFS make without copying blocks), a link, a relative link or a hard link. A target is a path, or an address of a storage provider, like `s3://bucket/results/x`, which `publishDir`, the Nextflow output directory and `-output-dir` may name ([storage](storage.md), section 4). The engine delivers through the executor's `Files` service (`Executor::files`), and replaces what is at the target: `task_runner::StorageFiles` for every real executor, which uploads to addresses and copies from a store elsewhere; a link becomes a copy where the source or the target is not on this machine. The simulator has none, so a simulated run delivers nothing, and never writes into the folders of the user; the observer still hears of each delivery. A delivery that fails ends the run (`ExecuteError::Delivery`). What each driver delivers:

- Nextflow: the files of `publishDir` (section 3), and the workflow outputs of the `output { }` block below the output directory (`-output-dir`, else `outputDir` of the config, else `results`): into the folder of `path`, or where the publish statements of a `path` closure (`file >> 'dir/'`) say, with the `mode` of the config's `workflow.output` scope, and an index file (CSV, JSON or YAML) of the published values once the channel ends.
- Snakemake: the outputs, logs and benchmarks of each job, into the working directory at their paths, except `temp()` outputs, as Snakemake leaves them.
- CWL: the files and directories of the output object, into the output folder under their basenames (`-output-dir`, else the current directory), as `cwltool --outdir` does; the driver prints the output object, which then names them there.
- Argo: the output artifacts of each step, as `<output folder>/<step>/<artifact>` (`-output-dir`, else `results`).

**Output locations** are strings (`OutputFile.location`): a path on this machine for a local store, a URL like `s3://bucket/key` for a store elsewhere, with the size and digest of the file when the executor knows them. `Files::local` gives a local path for a location: for a local store the location itself, else a copy in the folder of downloads.

**Simulated runs.** `Launch.simulated` is set for the simulator. Drivers switch on their workarounds for made-up files only then: they take absent inputs as present and give missing values placeholders. A real run follows the reference tool instead; so a Nextflow `splitCsv` with `header: true` of a file without a header line stops with Nextflow's "Missing 'header' in CSV file" in a real run, and reads no rows in a simulated one.

**No-op tasks.** A task with neither a command nor an image does nothing (`Task::is_no_op`), like a CWL `Operation` or a Snakemake rule without an action. A real executor finishes it at once with exit code 0 and no files, and the simulator pretends it like any other task. A task with an image but no command is not a no-op: it runs the image's default command, as an Argo container without `command` and `args` does. The rule sits in the executor layer, not in the engine, so that simulated runs keep the made-up files that drivers expect.

**The image check.** Every task runs in a container (decision 11). So before a real executor starts a task, a task without an image is an error, unless the user gives `-default-image <image>`, which such tasks then use. The simulator needs no image. Both rules are in `task_runner::RunnerExecutor`.

## 6. The execution graph

The engine records each task when the driver asks for it, so the graph grows during the run. A node is the task and its exit code. An edge is `file` when a task reads a file (or a file inside a directory) that an earlier task made, found by the local path of the output, or `order` for the tasks a task lists in `after`. `ExecutionGraph::check` confirms that every edge joins known tasks and points forward. A run that fails keeps the graph so far; `-graph` still writes it and `iunctus graph` still prints it.

After `start` and after each `complete`, also when they fail, the engine records the tasks that the driver asked for, then the driver's notes, which become markers on the tasks whose outputs they concern: points where a run with real data can have more or fewer tasks ([the graphs of a run](graph.md), section 3). A graph of the simulator says that it is simulated (`ExecutionGraph.simulation`).

`iunctus graph` sends what the workflow prints to standard error, so standard output holds only the graph. The graph with one node per task and the graph with one node per step come as JSON, Graphviz DOT, SVG drawn by iunctus itself, and a summary ([the graphs of a run](graph.md), sections 4 and 5).

## 7. Checks

- `workflow-graph/tests/engine.rs`: a toy driver, the file edges, the TES documents, and what the simulator makes.
- `workflow-graph/tests/order.rs`: the `after` rule, and pending tasks that finish as asked, reversed or shuffled.
- `workflow-graph/tests/attempts.rs`: attempts that run at the same time and finish as they end, deterministic orders, retries after failures of the machine and by the driver, and a cancelled run; `workflow-graph/tests/events.rs`: what an observer hears.
- `nextflow-runtime/tests/driver.rs`: a two-process pipeline through the engine, the `env` scope, the params file; `nextflow-runtime/tests/publish.rs`: `publishDir` from the process and the config, and globs that leave out staged inputs; `nextflow-runtime/tests/errors.rs`: the error strategies; `nextflow-runtime/tests/operators.rs` also the CSV header rule of real and simulated runs.
- `iunctus/tests/graph.rs`: `iunctus graph` and `iunctus run -graph` on a two-process pipeline, also with `-max-pending` and `-order`, the step view, SVG and the note of a simulated run; unit tests in `iunctus/src/detect.rs` cover detection and those in `task-runner/src/executor.rs` the no-op rule and the image check.
- `iunctus/tests/run.rs`: a task without an image fails with the local executor; with Docker (ignored by default), a pipeline runs in containers and its second run is answered from the cache, and Ctrl-C stops two running tasks at once.
- `task-model/tests/cache_key.rs`, `content-store/tests/store.rs`, `executor/tests/wrapper.rs` and `task-runner/tests/runner.rs`: the cache key, the store (also the same content stored by tasks at the same moment, and outputs taken over), the wrapper, and the runner with a fake backend. Unit tests in `executor` cover the pool, staging and the engine options. `executor/tests/local.rs` runs containers with Docker (ignored by default): read-only links, time limits, absolute paths and failures; `just s3-test` runs the store on S3.
- `iunctus/tests/fixtures/real_runs.py` runs workflows for real with iunctus and with the native tool and compares the names and digests of what each delivers, in `real_runs.txt`; files whose digests differ between two native runs are compared by name.
- The test-workflows tests of each language run every test workflow with the simulator: `just run-test-workflows-test` (Nextflow), `just snakemake-run-test-workflows-test`, `just cwl-graph-test-workflows-test` and `just cwl-conformance-test`, and the Argo test in `argo-runtime/tests/`.

## 8. Next steps for the execution layer

The execution layer runs tasks in parallel, on a local container engine, on GA4GH TES, on SLURM and on Kubernetes (decision 22). Done so far: the async executor interface, the parallel local executor, output locations as strings, delivering results, remote inputs, the switch for simulated runs (sections 4 and 5), retries and cancellation in the engine, Nextflow's error strategies and Snakemake's retries, delivering results in all four languages, and the TES, SLURM and Kubernetes executors ([executors](executors.md)). These are the next changes, in the order they build on each other.

1. **Reading from a store elsewhere.** The engine gives each driver the executor's `Files` service before `start` (`Driver::use_files`), and drivers read the content of task outputs through it (`workflow_graph::files::read`), which downloads a file of a store elsewhere on demand: the Nextflow `.command.env`, the CWL `cwl.output.json`, outputs that Snakemake code reads, and Argo output parameters and plugin replies. Still read as local paths: the listing of an output directory by workflow code, and files that CWL expressions list; workflow code that reads a file by its address goes through the storage providers.
2. **What Nextflow's reports cannot say yet.** A real Nextflow run writes the trace, timeline, report and DAG files that the config turns on, like those that nf-core configs ask for in `pipeline_info/` (`nextflow-runtime/src/reports.rs`); the DAG is the step graph of the run, drawn by iunctus. The trace leaves the CPU, memory and I/O that a task used as `-`, since no executor measures them yet, and a run that fails writes no reports.
3. **What a task cannot say yet:** whether an input must be writable or updated in place (CWL `InitialWorkDir` with `writable`, `inplaceUpdate`). `TaskSpec` already has `writable`. A task can say that it runs without network (`Task.offline`, which CWL tools are unless they ask for it with `NetworkAccess`); the local executor and Podman or Docker jobs of SLURM then give the container none, while TES, Apptainer and Kubernetes have no such switch yet.
4. **Tests on real servers and clusters.** The TES executor is tested against a fake server and Funnel, the SLURM executor against a simulated SLURM, and the Kubernetes executor against a fake Kubernetes API server. Next: TESK for TES, a SLURM cluster in containers, and a `kind` cluster. TES and SLURM need a command, so a task without one needs the image's default command from the registry.
5. **Remote inputs for TES**: remote inputs should pass through to a TES server as URLs, instead of a download. Downloads from a remote store read whole objects into memory today.
6. **Argo container sets** share one pod and its volumes. They need to run as one task, or on one machine with a shared directory.
