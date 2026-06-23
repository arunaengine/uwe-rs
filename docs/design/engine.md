<!-- How `iunctus run` runs a workflow of any language: detection, drivers, the execution graph and executors. -->
<!-- Copyright (c) 2026 The iunctus Contributors -->
<!-- SPDX-License-Identifier: MIT or Apache-2.0 -->

# The engine

Status: implemented for Nextflow, Snakemake, CWL and Argo, 2026-09-29. This applies [decisions](../decisions.md) 15 and 16, and decision 11 for real executors.

`iunctus run <target>` works the same way for every language:

1. It finds the language of the target (section 2).
2. It turns the command line into a `Launch`, the options all languages share.
3. It asks the language's crate for a `Driver` for that launch.
4. `workflow_graph::execute_with` runs the driver against an `Executor` with the run options (section 5) and records the execution graph (section 6).
5. With `-graph <file>` it writes the graph; `iunctus graph <target>` prints it.

The engine, the driver interface, the graph and the mock executors live in the `workflow-graph` crate, which knows no language. Language crates depend on it, never the other way round. The local executor lives in the crates `task-model`, `content-store`, `executor` and `task-runner` (section 4).

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
| `-executor mock-tes\|mock\|local` | the executor (section 4) | (chooses the executor) |
| `-store <directory>` | the cache store of the local executor | |
| `-default-image <image>` | the image of tasks that name none, for real executors (section 5) | |
| `-max-pending <n>` | how many tasks may be pending at once; 1 by default (section 5) | |
| `-order asked\|reversed\|shuffled[:<seed>]` | the order in which pending tasks finish; `asked` by default | |
| `-graph <file>` | write the graph, as DOT for a `.dot` name, else JSON | |
| `-format json\|dot\|summary` | the output of `iunctus graph`; `summary` by default | |

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

A driver is a state machine (`workflow_graph::Driver`): `start` loads the workflow and returns the first tasks, `complete(outcome)` returns the tasks that can run now, and `finish` checks that nothing waits for input that never comes. Each language crate exposes

```rust
pub fn driver(launch: &Launch) -> Result<Box<dyn Driver>, DriverError>
```

and the binary picks one in `driver` in `iunctus/src/main.rs`, a single match on `Language`: `nextflow_runtime::driver` (through `iunctus/src/nextflow.rs`), `snakemake_runtime::driver`, `cwl_runtime::driver` and `argo_runtime::driver`. A `DriverError` with `unsupported` set ends the command with exit code 33. With a mock executor the binary sets `Launch.assume_inputs`, so that a workflow plans all its tasks without its data; drivers log the files they assume.

Drivers look up outcomes by task id, so they must not depend on the order in which tasks finish. How the Argo driver works is in [the Argo runtime](argo/runtime.md).

**The Nextflow driver** (`nextflow_runtime::driver`) loads the config (project, launch directory and `-c` files, profiles, the params file and the command-line params over it), runs `manifest.mainScript` or `main.nf` of a project directory, and starts the dataflow network. The binary gives it a host that downloads remote files into the cache. Each Nextflow task becomes a container task:

- `image` from the `container` directive, `command` from the `shell` directive (default `/bin/bash -ue`) with `.command.sh`;
- `script` is the task script; the `.command.sh` input holds it with the code that captures `env` and `eval` outputs into `.command.env`, which is then an output too;
- each staged file is an input with its source path or URL;
- `resources` from `cpus`, `memory`, `disk`, `time` and `accelerator`, `environment` from the config's `env` scope.

Outcomes go back the other way: output files by name, standard output, and the `env` and `eval` values read from `.command.env`. A value that a task did not write, as with a mock, is empty. `-entry` is not supported yet.

## 4. Executors

| `-executor` | What it does |
|---|---|
| `mock-tes` (default) | Writes the TES v1.1 task document of each task to `<cache>/scratch/mock-tes/<run>/tes/<id>.json` and pretends to run it in `work/<id>/`. |
| `mock` | Only pretends, in `<cache>/scratch/mock/<run>/<id>/`. |
| `local` | Runs each task in Docker or Podman (`IUNCTUS_ENGINE`) through the task runner, which reuses results from the cache store (`-store`). Remote inputs fail as unsupported. |

**Pretending** (`workflow_graph::mock::pretend`) writes inputs with literal content, then makes each output. A small simulation of the task's shell script (`workflow_graph::mock::shell`) tells which files the script makes and what text it writes into them: `touch`, `mkdir -p`, `echo`, `printf` and `cat` with a here document into files, `cp`, `mv`, `ln`, `gzip`, `tar`, `zip`, brace expansion and `for` loops over words. An output is each made file that its pattern matches. When nothing matches, it is one empty file named after the pattern (`*` becomes `mock`), or a directory for a pattern that ends with `/`. A `.gz` file without text is an empty gzip file, and `versions.yml` gets the one entry nf-core code reads. The mocks pretend every task, also a no-op task (section 5), so that a mock run gets the files its driver expects.

**The local executor** (`iunctus/src/local.rs`) pins each image to its digest once per run, then hands the task to the task runner:

- `task_runner::Runner::request` turns the `Task` into a `TaskRequest`. Its `TaskSpec` (format 2) is what the task computes and gives the cache key: the image, the command, the work directory, the environment, each input by path and content digest, the output patterns and the digest of the standard input. Inputs go into the store first: literal content, local files, and local directories as a manifest of their files (a tree). The identity of a request is `<step>-<id>`, unique in the run; scratch directories and stored outputs are named after it, never after the display name.
- A stored result answers the request when the image is pinned to a digest (scope `shared`). Otherwise the task runs in `<cache>/scratch/<run>-<identity>-<attempt>/`, a name no other task uses: `work/` is the work directory, `control/` holds the wrapper and its files, and `mounts/` holds the inputs at absolute paths.
- The container runs as the current user. The work directory is mounted at the task's `workdir` when it has one, and each input at an absolute path is mounted there. A task without a command runs the image's default command, its entrypoint and command.
- The wrapper (`executor::wrapper`) runs the command with standard output and error captured in `control/`, so that output patterns cannot match the capture files, and with `control/stdin` as standard input when the task has one. After a success it collects the outputs: it expands braces in each pattern (Bash does not expand braces in a variable), matches with `globstar`, `nullglob` and `dotglob`, lists a directory as one entry, and copies an output at an absolute path to `control/root/`, so that it outlives the container.
- The runner stores each output under `runs/<run>/<identity>/<attempt>/work/<path>`, or `.../root/<path>` for an absolute path, reading files in parts. A directory output is stored as its files and a tree. The captured streams are stored by content. Then the scratch directory is removed, and a successful result becomes a cache entry.

## 5. Running tasks

The engine keeps the tasks that the driver asked for in a queue, in the order asked. A task can start when every task in its `after` list has finished; a finished task is one whose outcome the driver has heard. An `after` entry must name a task that the driver asked for before, earlier in the same list or in an earlier call; any other id ends the run with an error. So no task can wait for a task that never comes.

`execute_with(workflow, driver, executor, &RunOptions)` takes two options:

- `max_pending`: how many tasks may be pending at once: started, but the driver has not heard of their outcome yet. The engine starts the first tasks in the queue that can start, up to this number.
- `order`: which pending task finishes next. `AsAsked` is the oldest, `Reversed` the newest, and `Shuffled { seed }` a random one from a small seeded generator (SplitMix64), so that the same seed gives the same run again.

Then the driver hears that one outcome, and the tasks it asks for join the queue. `execute` is `execute_with` with the default options: one task at a time, in the order asked. The executors are still synchronous, so a pending task has already run; the options only change when the driver hears of it. That is enough to find drivers that depend on the order of outcomes before tasks really run in parallel. A test can run its workflows with `RunOptions { max_pending: 8, order: Order::Shuffled { seed: 1 } }`, and `iunctus run` takes `-max-pending 8 -order shuffled:1`.

**No-op tasks.** A task with neither a command nor an image does nothing (`Task::is_no_op`), like a CWL `Operation` or a Snakemake rule without an action. A real executor finishes it at once with exit code 0 and no files, and the mocks pretend it like any other task. A task with an image but no command is not a no-op: it runs the image's default command, as an Argo container without `command` and `args` does. The rule sits in the executor layer, not in the engine, so that mock runs keep the made-up files that drivers expect.

**The image check.** Every task runs in a container (decision 11). So before a real executor starts a task, a task without an image is an error, unless the user gives `-default-image <image>`, which such tasks then use. The mocks need no image. Both rules are in `iunctus/src/checked.rs`, which wraps every real executor.

## 6. The execution graph

The engine records each task when the driver asks for it, so the graph grows during the run. A node is the task and its exit code. An edge is `file` when a task reads a file (or a file inside a directory) that an earlier task made, found by the local path of the output, or `order` for the tasks a task lists in `after`. `ExecutionGraph::check` confirms that every edge joins known tasks and points forward. A run that fails keeps the graph so far; `-graph` still writes it and `iunctus graph` still prints it.

`iunctus graph` sends what the workflow prints to standard error, so standard output holds only the graph. The formats are the JSON of `ExecutionGraph`, Graphviz DOT, and a summary with one line per task and its upstream tasks.

## 7. Checks

- `workflow-graph/tests/engine.rs`: a toy driver, the file edges, the TES documents, and what the mocks make.
- `workflow-graph/tests/order.rs`: the `after` rule, and pending tasks that finish as asked, reversed or shuffled.
- `nextflow-runtime/tests/driver.rs`: a two-process pipeline through the engine, the `env` scope, the params file.
- `iunctus/tests/graph.rs`: `iunctus graph` and `iunctus run -graph` on a two-process pipeline, also with `-max-pending` and `-order`; unit tests in `iunctus/src/detect.rs` cover detection and those in `iunctus/src/checked.rs` the no-op rule and the image check.
- `iunctus/tests/run.rs`: a task without an image fails with the local executor; with Docker (ignored by default), a pipeline runs in containers and its second run is answered from the cache.
- `task-model/tests/cache_key.rs`, `content-store/tests/store.rs`, `executor/tests/wrapper.rs` and `task-runner/tests/runner.rs`: the cache key, the store and its trees, the wrapper, and the runner with a fake executor. `executor/tests/local.rs` runs containers with Docker (ignored by default); `just s3-test` runs the store on S3.
- The test-workflows tests of each language run every test workflow with the mock TES executor: `just run-test-workflows-test` (Nextflow), `just snakemake-run-test-workflows-test`, `just cwl-graph-test-workflows-test` and `just cwl-conformance-test`, and the Argo test in `argo-runtime/tests/`.

## 8. Next steps for the execution layer

The next layer runs tasks in parallel, on a local container engine and on GA4GH TES. These are the changes it needs, in the order they build on each other. The fields of `Task`, `Input`, `Outcome`, `OutputFile` and `Launch` and the `Driver` and `Executor` traits stay as they are until the language drivers are ready for them.

1. **An async executor interface.** Executors run tasks on Tokio, while the driver loop stays synchronous on the calling thread, because the Nextflow and Argo drivers hold `Rc` and are not `Send`. A proposal:

   ```rust
   pub trait Executor: Send + Sync {
       fn run(&self, task: Arc<Task>, attempt: u32, cancel: CancellationToken)
           -> BoxFuture<'static, Result<Outcome, ExecuteError>>;
   }
   ```

   The engine keeps its queue and the `after` rule, lets up to `max_pending` tasks run at once, and hands the driver each outcome when it arrives. `Order::Shuffled` stays for tests: it collects the outcomes of the tasks in flight and hands them over in a seeded order.
2. **Output locations as strings, with size and digest.** `OutputFile.location` is a local path today, and every driver reads it as one. With an S3 store, an output is a location string like `s3://...` with its size and digest, and the recorder links tasks by that string.
3. **A shared `Files` service** that drivers use to read the files that tasks made. It makes a location local on demand, so that a driver reads a Nextflow `.command.env`, a CWL `cwl.output.json` or an Argo parameter file the same way from any store. The `directory` flag of an outcome says whether a location is a directory.
4. **`Launch.mock`**, set by the binary for the mock executors, so that drivers switch their mock workarounds on only for mock runs: the CWL mock option, Snakemake's assumed inputs, and the placeholders of the Argo and Nextflow drivers. One rule differs from the reference tool in every run today, so that mock runs go on, and must follow it again in real runs: Nextflow's `splitCsv` with `header: true` gives no rows for an empty file, where Nextflow stops with "Missing 'header' in CSV file".
5. **Retries and cancellation.** An attempt number for tasks (Nextflow `task.attempt` and `errorStrategy`, Snakemake retries, Argo `retryStrategy`). The engine retries infrastructure errors with the next attempt; a non-zero exit code goes to the driver, which decides. On an error the engine cancels the tasks in flight, waits a grace time, records what it knows and returns.
6. **What a task cannot say yet:** whether it may use the network (CWL `NetworkAccess`), and whether an input must be writable or updated in place (CWL `InitialWorkDir` with `writable`, `inplaceUpdate`). `TaskSpec` already has `writable`.
7. **The TES executor.** It maps a `TaskSpec` onto one TES task with one executor and the wrapper as a staged input ([TES research](../research/tes-and-executors.md), section 8), marks directory inputs and outputs as `DIRECTORY`, and reads the manifest, the exit code and the captured streams back as outputs. TES needs a command, so a task without one needs the image's default command from the registry, and image pinning needs a registry lookup too.
8. **A parallel local executor**, on Tokio, behind the same interface as TES. The task runner has its own runtime today, and `LocalContainer::run` blocks.
9. **Remote inputs**: `s3://`, `gs://` and `https://` inputs are unsupported by the local executor. The runner should download them, or pass them through to TES as URLs. Downloads from a remote store read whole objects into memory today.
10. **Argo container sets** share one pod and its volumes. They need to run as one task, or on one machine with a shared directory.
