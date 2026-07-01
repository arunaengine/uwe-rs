<!-- How `iunctus run` runs a workflow of any language: detection, drivers, the execution graph and executors. -->
<!-- Copyright (c) 2026 The iunctus Contributors -->
<!-- SPDX-License-Identifier: MIT or Apache-2.0 -->

# The engine

Status: implemented for Nextflow, Snakemake, CWL and Argo, 2026-09-29; tasks run in parallel on Tokio since 2026-09-30. This applies [decisions](../decisions.md) 15, 16 and 21, and decisions 11 and 22 for real executors.

`iunctus run <target>` works the same way for every language:

1. It finds the language of the target (section 2).
2. It turns the command line into a `Launch`, the options all languages share.
3. It asks the language's crate for a `Driver` for that launch.
4. `workflow_graph::Engine` runs the driver against an `Executor` with the run options (section 5) and records the execution graph (section 6). It tells an observer each step of the run (section 5); the binary's observer records the run in the run database ([runs](runs.md)).
5. With `-graph <file>` it writes the graph; `iunctus graph <target>` prints it. A simulated run says so on standard error, and its graph says so too ([the graphs of a run](graph.md)).

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
| `-executor simulator\|local` | the executor (section 4) | (chooses the executor) |
| `-store <directory>` | the cache store of the local executor | |
| `-default-image <image>` | the image of tasks that name none, for real executors (section 5) | |
| `-max-pending <n>` | how many tasks may be pending at once; 1 for the simulator, no limit for real executors (section 5) | |
| `-order asked\|reversed\|finished\|shuffled[:<seed>]` | the order in which pending tasks finish; `asked` for the simulator, `finished` for real executors | |
| `-graph <file>` | write the graph: DOT for a `.dot` name, SVG for `.svg`, a summary for `.txt`, else JSON | |
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

A driver is a state machine (`workflow_graph::Driver`): `start` loads the workflow and returns the first tasks, `complete(outcome)` returns the tasks that can run now, and `finish` checks that nothing waits for input that never comes. `notes` gives what the driver noticed while it planned, like reads of task outputs, which the graph turns into markers ([the graphs of a run](graph.md), section 3). Each language crate exposes

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

Outcomes go back the other way: output files by name, standard output, and the `env` and `eval` values read from `.command.env`. A value that a task did not write, as with the simulator, is empty. `-entry` is not supported yet.

## 4. Executors

| `-executor` | What it does |
|---|---|
| `simulator` (default) | Writes the TES v1.1 task document of each task to `<cache>/scratch/simulator/<run>/tes/<id>.json` and pretends to run it in `work/<id>/` (decision 21). |
| `local` | Runs each task in Docker or Podman through the task runner, which reuses results from the cache store (`-store`). Remote inputs fail as unsupported. |

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
- **Staging** (`executor::stage`): an input is a symbolic link to where its content is, a local file, a directory of the task that made it, or a file of the store, and the container sees the folder of that source read-only at the same path. So nothing is copied, and a task cannot change its inputs. A task gets its own copy only of literal content, of inputs it may change (`writable`) or run (`executable`). An input at an absolute path is mounted there read-only.
- **The container engine** (`executor::ContainerEngine::find`) is `IUNCTUS_ENGINE` if set, else Docker when its service answers, else Podman; no config is needed. The container runs as the current user: `--user` with rootful engines, `--userns=keep-id` with rootless Podman, and as root inside for rootless Docker, whose root is the user.
- **CPUs and memory** (`executor::Pool`): the tasks share the CPUs and memory of the machine. An attempt waits for its share (its `cpus`, and its `memory` if it names one), in the order the tasks asked, and its container gets `--cpus` and `--memory` of that share. A task that asks for more than the machine has gets all of it.
- **Time and stopping**: a task that runs longer than its `time` is stopped (`docker stop` with 10 seconds of grace) and ends with the container's exit code and a note in its standard error. When the run is cancelled, the container is stopped at once and the attempt ends with `Cancelled`. The scratch directory of an attempt that fails or is stopped is removed.
- **Exit codes**: the wrapper records the tool's exit code. The engine's exit codes 125 to 127, or an end without the tool's exit code, are a failure of the machine (`ExecuteError::Machine`), which the engine runs again. A container killed by a signal, like 137 for lack of memory, ends the task with that exit code, as Nextflow reports it, so that the driver's error strategy decides.
- The wrapper (`executor::wrapper`) runs the command with standard output and error captured in `control/`, so that output patterns cannot match the capture files, and with `control/stdin` as standard input when the task has one. After a success it collects the outputs: it expands braces in each pattern (Bash does not expand braces in a variable), matches with `globstar`, `nullglob` and `dotglob`, lists a directory as one entry, and copies an output at an absolute path to `control/root/`, so that it outlives the container.
- The runner stores each output under `runs/<run>/<identity>/<attempt>/work/<path>`, or `.../root/<path>` for an absolute path. A local store moves the file there and hard-links its content into `cas/`, so nothing is copied; other stores get it uploaded in parts. A directory output is stored as its files and a tree. The captured streams are stored by content. Then the scratch directory is removed, and a successful result becomes a cache entry.

## 5. Running tasks

The engine keeps the tasks that the driver asked for in a queue, in the order asked. A task can start when every task in its `after` list has finished; a finished task is one whose outcome the driver has heard. An `after` entry must name a task that the driver asked for before, earlier in the same list or in an earlier call; any other id ends the run with an error. So no task can wait for a task that never comes.

The engine starts an attempt of each task that can start, up to `max_pending`, and drives it on Tokio; the driver stays on its own thread, because the Nextflow and Argo drivers hold `Rc`. When the attempt that the order chose has ended, the driver hears its outcome, and the tasks it asks for join the queue. `Engine::new(&mut executor).options(RunOptions { max_pending, order }).run(workflow, &mut driver)` runs a workflow; `execute` and `execute_with` are shortcuts. The options:

- `max_pending`: how many tasks may be pending at once: started, but the driver has not heard of their outcome yet. It is 1 for the simulator and has no limit for real executors, whose machine or cluster then sets how many run at once.
- `order`: which pending attempt finishes next. `Finished` is the one that ended first, as real runs go. `AsAsked` is the oldest, `Reversed` the newest, and `Shuffled { seed }` a random one from a small seeded generator (SplitMix64). These three do not depend on when attempts end: the engine chooses an attempt, then waits for it. So the same seed gives the same run again, and tests find drivers that depend on the order of outcomes: `RunOptions { max_pending: 8, order: Order::Shuffled { seed: 1 } }`, or `iunctus run -max-pending 8 -order shuffled:1`.

**Attempts and retries.** Each attempt of a task gets the next number, from 1, which the outcome carries (`Outcome.attempt`), with its start and end times, the image it ran in and whether the store answered it (`cached`). When the machine fails (`ExecuteError::Machine`: the container engine, a TES server, a cluster), the engine runs the task again, twice by default (`Engine::retries`); the driver does not hear of the failed attempt. A task that fails with an exit code goes to the driver, which decides: to retry it, the driver asks for the task again with the same id, in its answer to the outcome. The graph keeps one node per task, and the tasks after it wait for the new attempt.

**Stopping.** `Engine::cancel(token)` stops the run when the token fires; `iunctus run` fires it on Ctrl-C (a second Ctrl-C ends iunctus at once) and exits with 130. When a run stops, for Ctrl-C or an error, the engine cancels the attempts in flight and waits up to a minute for them to end.

**Observers.** `Engine::observer` gets each step of the run (`workflow_graph::Event`), before its effect: `Requested` with the task and its edges, `Started` with the attempt, `Finished` with the outcome (`retried` when the engine runs the task again after a failure of the machine), and `Ended`, on every path, with the error if there is one. The run database is such an observer.

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
- `nextflow-runtime/tests/driver.rs`: a two-process pipeline through the engine, the `env` scope, the params file.
- `iunctus/tests/graph.rs`: `iunctus graph` and `iunctus run -graph` on a two-process pipeline, also with `-max-pending` and `-order`, the step view, SVG and the note of a simulated run; unit tests in `iunctus/src/detect.rs` cover detection and those in `task-runner/src/executor.rs` the no-op rule and the image check.
- `iunctus/tests/run.rs`: a task without an image fails with the local executor; with Docker (ignored by default), a pipeline runs in containers and its second run is answered from the cache, and Ctrl-C stops two running tasks at once.
- `task-model/tests/cache_key.rs`, `content-store/tests/store.rs`, `executor/tests/wrapper.rs` and `task-runner/tests/runner.rs`: the cache key, the store (also the same content stored by tasks at the same moment, and outputs taken over), the wrapper, and the runner with a fake backend. Unit tests in `executor` cover the pool, staging and the engine options. `executor/tests/local.rs` runs containers with Docker (ignored by default): read-only links, time limits, absolute paths and failures; `just s3-test` runs the store on S3.
- The test-workflows tests of each language run every test workflow with the simulator: `just run-test-workflows-test` (Nextflow), `just snakemake-run-test-workflows-test`, `just cwl-graph-test-workflows-test` and `just cwl-conformance-test`, and the Argo test in `argo-runtime/tests/`.

## 8. Next steps for the execution layer

The execution layer runs tasks in parallel, on a local container engine and later on GA4GH TES, SLURM and Kubernetes (decision 22). Done so far: the async executor interface and the parallel local executor (sections 4 and 5), and the engine's part of retries and cancellation. These are the next changes, in the order they build on each other.

1. **Output locations as strings, with size and digest.** `OutputFile.location` is a local path today, and every driver reads it as one. With an S3 store, an output is a location string like `s3://...` with its size and digest, and the recorder links tasks by that string.
2. **A shared `Files` service** that drivers use to read the files that tasks made. It makes a location local on demand, so that a driver reads a Nextflow `.command.env`, a CWL `cwl.output.json` or an Argo parameter file the same way from any store. The `directory` flag of an outcome says whether a location is a directory.
3. **A switch for simulated runs** in `Launch`, set by the binary for the simulator, so that drivers switch their workarounds for made-up files on only for simulated runs: the CWL `simulated` option, Snakemake's assumed inputs, and the placeholders of the Argo and Nextflow drivers. One rule differs from the reference tool in every run today, so that simulated runs go on, and must follow it again in real runs: Nextflow's `splitCsv` with `header: true` gives no rows for an empty file, where Nextflow stops with "Missing 'header' in CSV file".
4. **Retries in the drivers.** The engine gives each attempt its number and lets a driver ask for a task again (section 5). The drivers still have to use it: Nextflow `task.attempt`, `errorStrategy` and `maxRetries`, Snakemake `retries`, Argo `retryStrategy`; CWL has none.
5. **What a task cannot say yet:** whether it may use the network (CWL `NetworkAccess`), and whether an input must be writable or updated in place (CWL `InitialWorkDir` with `writable`, `inplaceUpdate`). `TaskSpec` already has `writable`.
6. **The TES executor.** It maps a `TaskSpec` onto one TES task with one executor and the wrapper as a staged input ([TES research](../research/tes-and-executors.md), section 8), marks directory inputs and outputs as `DIRECTORY`, and reads the manifest, the exit code and the captured streams back as outputs. TES needs a command, so a task without one needs the image's default command from the registry, and image pinning needs a registry lookup too.
7. **Remote inputs**: `s3://`, `gs://` and `https://` inputs are unsupported by the local executor. The runner should download them, or pass them through to TES as URLs. Downloads from a remote store read whole objects into memory today.
8. **Argo container sets** share one pod and its volumes. They need to run as one task, or on one machine with a shared directory.
