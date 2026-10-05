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
| `-keep-going` | after a task failed, run every task that does not depend on it, then fail (section 5) | `keep_going` |
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
| `-no-cache` | take no result from the cache and keep none there, so that every task runs ([the cache](cache.md), section 7) | |
| `-page` | serve the web page of the run ([runs](runs.md), section 4) | |
| `-database <file>` | the run database; `runs.sqlite` in the cache directory by default | |
| `-check-reproducibility <percent>` | run this share of the tasks whose results come from the cache again and compare their outputs by digest ([reproducibility](reproducibility.md), section 4) | |
| `-no-network` | run the tasks without network whose workflow does not say that they need it ([reproducibility](reproducibility.md), section 6) | |

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

What a Snakemake job sees: Snakemake runs a job in the working directory, where its shell command can also call scripts and packages of the workflow that the rule does not declare, like `python -m some_package.module`. A task of iunctus sees only what it gets, so every task also gets the source files of its workflow, at their paths in its work directory (`snakemake-runtime/src/sources.rs`). They are the files that Snakemake itself deploys to jobs that run without a shared file system, as on Kubernetes (`DAG.get_sources`): the files that git tracks below the working directory, read from the index of the repository without git (`src/git.rs`, index versions 2 to 4, SHA-1 and SHA-256, submodules), the folders of the rules' scripts and notebooks, the rules' conda files and the config files; each at most 10 MB and below the working directory. The Snakefiles are left out, since a job runs the code of its own rule, and so is a source where the job's own inputs and outputs go. Each source is a read-only input with its digest in the cache key, so a task stays reproducible: its result is tied to every file that it could read, and a changed script runs the task again. The folder of a source is also visible read-only at its own path, so `{workflow.basedir}/scripts/x.py` works too. Showing the whole working directory instead would put data and the results of earlier runs into every cache key, or leave them out of it; files that the workflow makes while it runs, like those of a `shell()` call in the Snakefile, are not sources, and a job sees them only when its rule declares them. The top-level `shell()` calls of a Snakefile run in the working directory itself, which they change where it is (`Writable::InPlace`), as Snakemake runs them there.

When a task fails, its `errorStrategy` decides, evaluated after the failure with `task.exitStatus` and `task.attempt`, as nf-core's base config needs: `retry` asks for the task again with the same id and the next attempt, whose directives are evaluated again (like `memory = { 6.GB * task.attempt }`), up to `maxRetries` (1 by default); `ignore` goes on without the task's outputs; `finish` starts no more tasks and fails the run once the running ones have ended; `terminate`, the default, fails the run at once. A retry and an ignored failure print a `NOTE` line, as Nextflow does.

A finished task's outputs are published by its `publishDir` directives (section 4, delivering results): each output file whose name matches the `pattern`, under the name that `saveAs` gives, in the folder of `path`, with the mode: `symlink` (the default) is a link, `rellink` a relative link, `link` a hard link, and `copy`, `copyNoFollow` and `move` are copies, since the store keeps its files. `enabled: false` leaves a rule out, `overwrite: false` keeps a file that is there. The rules come from the process and from the config, where a selector like `withName` replaces the process's own.

## 4. Executors

| `-executor` | What it does |
|---|---|
| `simulator` | Writes the TES v1.1 task document of each task to `<cache>/scratch/simulator/<run>/tes/<id>.json` and pretends to run it in `work/<id>/` (decision 21). |
| `local` (default for `run`) | Runs each task in Docker or Podman through the task runner, which reuses results from the cache store (`-store`). |
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

- `Runner::request` turns the `Task` into a `TaskRequest`. Its `TaskSpec` (format 2) is what the task computes and gives the cache key: the image, the command, the work directory, the environment, each input by path and content digest, the output patterns and the digest of the standard input. Literal content goes into the store. Local files and directories are hashed where they are, and the digest of a file that did not change since a run read it comes from the cache of digests ([the cache](cache.md), section 7); a backend on another machine (`Settings::import_inputs`) gets them copied into the store too. The identity of a request is `<step>-<id>`, unique in the run; scratch directories and stored outputs are named after it, never after the display name.
- A stored result answers the request when the image is pinned to a digest (scope `shared`) and the store still has its files. A request for a task that another run runs at the same moment waits for that result instead of running it too ([the cache](cache.md)). Otherwise the attempt runs in `<cache>/scratch/<run>-<identity>-<attempt>/`, a name no other attempt uses: `work/` is the work directory, `control/` holds the wrapper and its files, `home/` is the home folder of the task, and `mounts/` holds copies at absolute paths.
- **Remote inputs**: an input at an address comes through the storage provider that serves it ([storage](storage.md)): a provider of the config file, with its endpoint and credentials, or a built-in one for web addresses and public `s3` and `gs` buckets (read over HTTPS, as anonymous clients do). It is downloaded once into the folder of downloads in the cache, under the SHA-256 of its public address; later runs, and workflow code that reads the same file, take it from there. An address that no provider serves, and remote directories, are not supported (exit code 33).
- **Staging** (`executor::stage`): an input is a symbolic link to where its content is, a local file, a directory of the task that made it, or a file of the store, and the container sees the folder of that source read-only at the same path. So nothing is copied, and a task cannot change its inputs. A task gets its own copy only of literal content, of inputs it may change (`Input.writable` is `Writable::Copy`, as a CWL `InitialWorkDir` entry with `writable: true`, a Nextflow process with `stageInMode 'copy'`, and Argo input artifacts and the files of volumes that a task mounts without `readOnly`) or run (`executable`). An input at an absolute path is mounted there read-only. An input that the task changes where it is (`Writable::InPlace`, as a CWL input with `InplaceUpdateRequirement` or the working directory of a Snakemake `shell()` call) is mounted writable at its path, `.` being the work directory itself; it is named by its path instead of its content, and such a task never comes from the cache. TES copies it in and back out.
- **The container engine** (`executor::ContainerEngine::find`) is the one that `IUNCTUS_ENGINE` names (`docker`, `podman` or a socket), else Docker when its socket answers, else Podman through its Docker-compatible socket; no config is needed, and iunctus talks to it through the Docker Engine API, not through its command. The container runs as the current user: as that user with engines that run as root, and as root inside with engines without root, whose root is the user.
- **CPUs and memory** (`executor::Pool`): the tasks share the CPUs and memory of the machine. An attempt waits for its share (its `cpus`, and its `memory` if it names one): the task with the longest chain of work after it first, by the times of earlier runs, else in the order the tasks asked ([the cache](cache.md), section 6), and its container gets that share of CPUs and memory as its limits. A task that asks for more than the machine has gets all of it.
- **Time and stopping**: a task that runs longer than its `time` is stopped (with 10 seconds of grace; the wrapper passes the stop on to the tool, so the task ends at once) and ends with the container's exit code and a note in its standard error. When the run is cancelled, the container is stopped at once and the attempt ends with `Cancelled`. The scratch directory of an attempt that fails or is stopped is removed.
- **Exit codes**: the wrapper records the tool's exit code. A container that could not start or could not run the wrapper (exit code 126 or 127, as for an image without a shell), or an end without the tool's exit code, is a failure of the machine (`ExecuteError::Machine`), which the engine runs again; the error quotes the last lines of the container. A container that the kernel killed for lack of memory gets a note that says so. A container killed by a signal, like 137 for lack of memory, ends the task with that exit code, as Nextflow reports it, so that the driver's error strategy decides.
- The wrapper (`executor::wrapper`) is a script of `sh` that goes on in Bash where the image has it. It runs the command with `HOME` set to `home/`, a folder the task may write, as Nextflow and Snakemake tasks have one (a container that runs as the user gets `/` as its home otherwise, which the user cannot write; a task that sets `HOME` itself, like a CWL tool, keeps its own), with standard output and error captured in `control/`, so that output patterns cannot match the capture files, and with `control/stdin` as standard input when the task has one. After a success it collects the outputs: it expands braces in each pattern (Bash does not expand braces in a variable), matches with `globstar`, `nullglob` and `dotglob` (in plain `sh`, with its own globs), lists a directory as one entry, and copies an output at an absolute path to `control/root/`, so that it outlives the container.
- The runner stores each output under `runs/<run>/<identity>/<attempt>/work/<path>`, or `.../root/<path>` for an absolute path. A local store moves the file there and hard-links its content into `cas/`, so nothing is copied; other stores get it uploaded in parts. A directory output is stored as its files and a tree. The captured streams are stored by content. Then the scratch directory is removed, and a successful result becomes a cache entry.

## 5. Running tasks

The engine keeps the tasks that the driver asked for in a queue, in the order asked. A task can start when every task in its `after` list has finished; a finished task is one whose outcome the driver has heard. An `after` entry must name a task that the driver asked for before, earlier in the same list or in an earlier call; any other id ends the run with an error. So no task can wait for a task that never comes.

The engine starts an attempt of each task that can start, up to `max_pending`, and drives it on Tokio; the driver stays on its own thread, because the Nextflow and Argo drivers hold `Rc`. When the attempt that the order chose has ended, the driver hears its outcome, and the tasks it asks for join the queue. `Engine::new(&mut executor).options(RunOptions { max_pending, order }).run(workflow, &mut driver)` runs a workflow; `execute` and `execute_with` are shortcuts. The options:

- `max_pending`: how many tasks may be pending at once: started, but the driver has not heard of their outcome yet. It is 1 for the simulator and has no limit for real executors, whose machine or cluster then sets how many run at once.
- `order`: which pending attempt finishes next. `Finished` is the one that ended first, as real runs go. `AsAsked` is the oldest, `Reversed` the newest, and `Shuffled { seed }` a random one from a small seeded generator (SplitMix64). These three do not depend on when attempts end: the engine chooses an attempt, then waits for it. So the same seed gives the same run again, and tests find drivers that depend on the order of outcomes: `RunOptions { max_pending: 8, order: Order::Shuffled { seed: 1 } }`, or `iunctus run -max-pending 8 -order shuffled:1`.

**Attempts and retries.** Each attempt of a task gets the next number, from 1, which the outcome carries (`Outcome.attempt`), with its start and end times, the image it ran in and whether the store answered it (`cached`). When the machine fails (`ExecuteError::Machine`: the container engine, a TES server, a cluster), the engine runs the task again, twice by default (`Engine::retries`); the driver does not hear of the failed attempt. A task that fails with an exit code goes to the driver, which decides: to retry it, the driver asks for the task again with the same id, in its answer to the outcome. The graph keeps one node per task, and the tasks after it wait for the new attempt.

**Failed tasks.** A task that ends with an exit code other than 0 goes to the driver, which decides what it means for the run, as its language does. Mostly no task starts after it, the running tasks end and their results are delivered, and then the run fails with the first failure: so Snakemake does, cwltool with `--on-error stop` (its default) and `--parallel`, and Nextflow's error strategy `finish`. Such a driver drains the run (`Driver::draining`): the engine starts no task of its queue and holds the executor (`Executor::hold`), whose attempts that still wait for their share of the machine end without running; tasks that wait in the queue of a TES server or a cluster run, as Snakemake runs the jobs that it submitted to a cluster. Nextflow's default strategy, `terminate`, ends the run at once, and the engine stops the running tasks, as Nextflow 26.04.6 kills them. Argo DAGs follow Argo's controller: a DAG schedules the tasks whose dependencies are done and runs while any of its tasks runs, so its other branches go on. With `-keep-going` (`Launch.keep_going`), the run goes on with every task that does not depend on the failed one and fails at its end: Snakemake's `--keep-going`, cwltool's `--on-error continue`; for Nextflow, a failure that would end the run counts as ignored until the end; Argo DAGs that do not set `failFast` run as with `failFast: false`. A failure of the machine after its retries still ends the run at once.

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
  nf-core/demo `-profile test` ran without network on the local executor: all 76 files of Nextflow 26.04.6, 35 with the same digest, 34 that also vary between two native runs by name; the FastQC reports differ only in their date, and `pipeline_dag` is the graph page of iunctus.
- `iunctus/tests/fixtures/real_runs_cwl.py`: the CWL conformance tests (v1.2) for real with iunctus and cwltool, compared by their output objects (`real_runs_cwl.txt`: 284 of 287 the same).
- `iunctus/tests/fixtures/real_runs_snakemake.py`: the tests of Snakemake's repository for real with iunctus (local executor) and Snakemake 9.27.0, in the same folder one after the other, compared by the files each run made, changed or removed (`real_runs_snakemake.txt`). Of 422 tests, 315 are the same (207 with the same files, 22 fail as Snakemake's tests expect, 53 fail in both, 33 give the same files that differ from `expected-results` in both, mostly for options of Snakemake's test suite), 7 differ and 10 fail in iunctus alone, each with its known reason (outputs that are links, jobs that share undeclared files, template engines, 4000 one-container jobs). A service job (`service()` output) runs in one task with the jobs that use it, as Snakemake runs them in one group: the service starts in the background, each job that uses it starts once the service made its output, and the service gets SIGTERM when they ended (`snakemake-runtime/src/services.rs`), 5 fail in Snakemake alone, and 85 are left out (remote files, Conda environments, wrappers, images that are not on this machine).
- `iunctus/tests/fixtures/real_runs_argo.py`: Argo test workflows for real with iunctus. Argo has no native run without a cluster, so the reference is Argo's own tests: `argo lint --offline`, and the workflows of `expectedfailures`, which must fail. Of 321 workflows that could run here, 277 end as Argo expects (262 succeed, 15 fail as expected), 2 succeed where Argo's tests expect a failure, 5 run although `argo lint` rejects them, and 37 fail in iunctus, mostly where a step fails on purpose (20), which Argo's Go tests expect, or where a step needs the network or a server of Argo's tests. Containers run as Argo runs them (decision 28): as the image's user in the image's working folder, so steps that write outside their work folder work, with no capabilities but `CHOWN` for root. `real_runs_argo.txt` names a probable cause for each.
- These scripts use no network: the tools run in a network namespace of their own, iunctus gives its containers none (`-no-network`), and Docker is reached through `iunctus/tests/fixtures/docker_no_pull.py`, a socket that refuses pulls, since the Docker daemon has network.
- The test-workflows tests of each language run every test workflow with the simulator: `just run-test-workflows-test` (Nextflow), `just snakemake-run-test-workflows-test`, `just cwl-graph-test-workflows-test` and `just cwl-conformance-test`, and the Argo test in `argo-runtime/tests/`.

## 8. Next steps for the execution layer

The execution layer runs tasks in parallel, on a local container engine, on GA4GH TES, on SLURM and on Kubernetes (decision 22). Done so far: the async executor interface, the parallel local executor, output locations as strings, delivering results, remote inputs, the switch for simulated runs (sections 4 and 5), retries and cancellation in the engine, Nextflow's error strategies and Snakemake's retries, delivering results in all four languages, and the TES, SLURM and Kubernetes executors ([executors](executors.md)). These are the next changes, in the order they build on each other.

1. **Reading from a store elsewhere.** The engine gives each driver the executor's `Files` service before `start` (`Driver::use_files`), and drivers read the content of task outputs through it (`workflow_graph::files::read`), which downloads a file of a store elsewhere on demand: the Nextflow `.command.env`, the CWL `cwl.output.json`, outputs that Snakemake code reads, and Argo output parameters and plugin replies. Still read as local paths: the listing of an output directory by workflow code, and files that CWL expressions list; workflow code that reads a file by its address goes through the storage providers.
2. **What Nextflow's reports cannot say yet.** A real Nextflow run writes the trace, timeline, report and DAG files that the config turns on, like those that nf-core configs ask for in `pipeline_info/` (`nextflow-runtime/src/reports.rs`); the DAG is the step graph of the run, drawn by iunctus. The trace takes `%cpu`, `peak_rss` (the peak memory of the container, page cache included), `read_bytes` and `write_bytes` from what the wrapper read in the cgroup of the container (`Outcome.usage`); `rchar`, `wchar` and `peak_vmem` stay `-`, and a run that fails writes no reports.
3. **Network:** a workflow can say whether a task needs the network (`Task.network`: CWL tools run without it unless they ask for it with `NetworkAccess`), and `-no-network` or `[network]` of the config file runs the other tasks without it ([reproducibility](reproducibility.md), section 6); the local executor and Podman, Docker or Apptainer jobs of SLURM then give the container none, while TES, Singularity and Kubernetes have no such switch yet.
4. **Tests on real servers and clusters.** The TES executor is tested against a fake server and Funnel, the SLURM executor against a simulated SLURM, and the Kubernetes executor against a fake Kubernetes API server. Next: TESK for TES, a SLURM cluster in containers, and a `kind` cluster. TES and Kubernetes take the entrypoint and the default command of an image from its registry; SLURM does not yet, so there a task without a command fails, and the entrypoint of the image does not run.
5. **Remote inputs for TES**: remote inputs should pass through to a TES server as URLs, instead of a download. Downloads from a remote store read whole objects into memory today.
6. **Argo container sets** share one pod and its volumes. Done: a task may hold several containers (`Task.containers`, `TaskSpec.containers`, `Outcome.containers`), which run at once in one place and share the work directory, the inputs and the outputs. Each runs in a wrapper of its own that waits for the containers it comes after (`executor::containers`). The local executor runs one engine container per image, Kubernetes one pod, SLURM one job, and TES one executor per container, one after the other. Each container keeps its logs and exit code, and every image counts in the cache key ([Argo runtime](argo/runtime.md), section 8.2).

## File descriptions: config-file

### `config-file/src/lib.rs`

The iunctus config file. Settings come from up to three TOML files, merged in this order,
where a later file wins key by key: the site-wide file, the user's file, and the file that a
command-line option names. Without any file, every setting has its default. Each value
remembers its file and line, so that errors say where to look. A secret may be a reference
to an environment variable instead of the secret itself, so that the file can be shared.

### `config-file/src/secret.rs`

Secrets in config files: a password, a key or a token, written into the file or taken from
an environment variable (`{ environment = "NAME" }`), so that the file can be shared without
its secrets. A secret is read only when it is used, and never shows when printed.

## Source contracts: executor

### `executor/src/access.rs:58`

How a container runs for a task with `run_as`, in an image whose config names the
user `image_user` (like `1000:100`, `nobody` or empty for root). `owner` holds the ids
of the user who runs iunctus, and `rootless` says whether the engine maps root of the
container to that user.

### `executor/src/access.rs:110`

Opens the scratch directory `root` of a task to every user, for a container that may run
as another user: its folders get mode 0777 and its files may be written by all. Links stay
as they are. The folder that holds the scratch directories becomes private to the user,
so that no other user of the machine reaches the open folders.

### `executor/src/containers.rs:107`

An order of the containers that runs them one after the other: each after those it comes
after, and the main container as early as it can be while every container that does not
come after it comes before it. For servers that run the containers of a task one after the
other, like TES.

### `executor/src/containers.rs:251`

The lines that run `command` in the background and, when it fails, record its exit code in
each file of `exits` that has none yet. A wrapper or container that is killed, as for lack
of memory, records nothing itself; this record keeps the containers that wait for it from
waiting for ever.

### `executor/src/containers.rs:272`

The wrapper of one container of the engine that runs the wrappers of a group of containers
at once, and ends when all of them ended. Each member is its wrapper and the file of its
exit code, as paths that the container sees. With an `owner`, it then hands what the task
made in `root`, the folder of the control folder, back to that user (see
[`Folders::owner`]).

### `executor/src/containers.rs:293`

Writes the wrapper of every container and of every group of a task that runs several into
the control folder `control` of this machine. `folders` are the folders as the containers
see them. The arguments of the containers must be what they run, the entrypoints of their
images included. Gives the groups with the paths of their wrappers as the containers see
them.

### `executor/src/lib.rs:51`

Starts no attempt that waits for its turn on this machine, as a run that ends after a
failure starts no more tasks; an attempt that waits ends as cancelled. Backends whose
attempts wait in a queue of their server or cluster start them all, as Snakemake does
with the jobs that it submitted to a cluster. The default holds nothing.

### `executor/src/local.rs:81`

Runs a task in `<scratch>/<name>`, a name that must not be in use: stages its inputs,
runs the wrapper in the task's image as the current user, and reads the control files.
The directory stays for the caller to take the outputs from, unless the attempt fails
or is cancelled.

The task waits for its share of CPUs and memory, and its container gets no more. The
work directory is mounted at the task's `workdir` when it has one. A task without
arguments runs the image's default command. The container stops when `cancel` fires,
and when the task runs longer than its time.

### `executor/src/local.rs:197`

The folders that the container sees, each with its place inside it: the scratch
directory at the same path, the work directory at the task's `workdir` when it names
one, and the mounts of the inputs. A mount with a relative target is inside the work
directory, and one at the work directory itself, like the working directory of a
Snakemake `shell()` call that the task changes in place, takes its place.

### `executor/src/local.rs:420`

Runs a task of several containers: one container of the engine for each image, all at
once, on the same scratch directory and mounts, within the share of the task. When one
of them ends without its wrapper, the others stop, since containers that wait for it
would wait for ever.

### `executor/src/local.rs:556`

Hands back to the user `user` what a container that ran as another user left in the
scratch directory `root`, when its wrapper could not, with a short container of the
task's image as root with only the capabilities that this needs. A failure leaves the
files as they are; the wrapper let every user change them.

### `executor/src/local.rs:636`

Runs a container to its end: creates it, attaches to its output before it starts, and
reads the output until the wrapper says that it ended. The wrapper writes only that
last line there, so the output tells when the task is done, without asking the engine
again and again, and without waiting for the engine to take the container down. Only
when the wrapper did not end does the container's exit code count; the request for it
goes out before the start, as the engine removes the container once it ended.
The container stops when `cancel` fires, and when it runs longer than `time`.

### `executor/src/local.rs:780`

What a container left behind. The wrapper exits with 0 whenever it recorded the tool's
exit code. A container that could not run the wrapper (exit code 126 or 127, as for an
image without Bash), or an end without the tool's exit code, is a failure of the
machine. A container stopped by a signal, as for lack of memory, ends the task with the
container's exit code, and so does one that ran out of time; that one fails even when its
tool ended well while the engine stopped it.

### `executor/src/pool.rs:122`

Waits until a task's share is free. A task that asks for more than the pool has gets
all of it; a task that names no memory takes none from the pool. The tasks that wait
take their shares one after another, the one with the highest `priority` first, and of
equal ones the one that came first. The choice is made when a share comes free, so a
task that comes later with a higher priority goes before those that wait.

### `executor/src/process.rs:644`

The container of a spec and its command. Core dumps are off: a tool that crashes inside
such a container would only fill the journal of the machine. It has the least permissions,
as the containers of tasks (decision 28): no capabilities and no way to gain privileges. An
engine without root runs it as root, which is the user outside.

### `executor/src/stage.rs:13`

Makes the scratch directory `root` of a task and places its inputs. The result is what the
container must mount besides the scratch directory.

A linked input is a symbolic link to its source, and the container sees the folder of the
source read-only at the same path. A copy is the task's own. A linked input at an absolute
path is mounted there read-only; a copy and a made directory at an absolute path are made
in `mounts/` and mounted there. An input that the task changes in place is mounted
writable at its path; the target of such a mount is relative to the work directory for a
relative path, and `.` is the work directory itself.

### `executor/src/wrapper.rs:48`

The user and group, like `1000:100`, that get what the task made in the folder of the
control folder at the end, for a container that may run as another user; empty for
none. The wrapper hands the files back when it runs as root with the capability
`CHOWN`, else it lets every user change them.

### `executor/src/wrapper.rs:55`

The wrapper for a task in the given folders. The task runs the arguments of the
specification, which must not be empty.

It runs the arguments with both streams captured in the control directory, so that output
patterns cannot match the capture files, and with `stdin` of the control directory as
standard input when the task has one. `HOME` is the home folder, unless the task sets it
itself. The wrapper records the exit code and times, collects the outputs only on success,
writes the manifest, then writes [`ENDED`] to its standard output and exits with 0, so that
the logs of a failed tool are kept. Outputs at absolute paths are copied below `root` in the control
directory, so that they outlive the container.

With an outbox, the wrapper also copies each output in the work directory to the same path
there, and at last lets every user change what the task made, since such a server may run
the container as root and then cannot remove the folders that the task made.
An invalid specification produces only a failure wrapper.

### `executor/src/wrapper.rs:173`

Runs a command so that a stop reaches it, and sets `status` to its exit code. The script
that runs it is the first process of its container, which ignores signals that it has no
trap for, so without the trap a stop would wait for the engine's whole grace time. The trap
passes SIGTERM on to the command, for a stop by SIGTERM, SIGINT or SIGHUP, and sets
`stopped`. The command runs in the background to let the trap run while it waits, with the
script's own standard input, which a background command would not get otherwise.

### `executor/src/wrapper.rs:195`

Records what the task used, from the cgroup of the container, when the container has one of
its own, as with Docker and Podman: CPU time, the peak of memory (page cache included), the
bytes of block I/O, and how many processes the kernel killed for lack of memory. Shell
builtins only, so that any image with a shell has what it needs, and no process starts.

### `executor/src/wrapper.rs:231`

Adds each file and directory that a pattern matches to the manifest, as `file <path>` or
`directory <path>`, without a process per file. Bash expands the pattern with `globstar`,
`nullglob` and `dotglob`; plain `sh` with its own globs, whose unmatched patterns stay as
they are and are left out. A match at an absolute path is copied below `root` in the
control directory, and one in the work directory to the outbox, when there is one; the
copies hold what links point to.

## File descriptions: executor

### `executor/src/access.rs`

Who a container runs as and what it may do (decision 28): the least permissions that let
its task run, and the files that it makes owned by the user who runs iunctus.

Every container drops all Linux capabilities and may not gain privileges (the option
`no-new-privileges`, which also turns off setuid programs). It runs without a privileged
mode and with its own process and IPC namespaces, the defaults of the engines; it gets
network only as the run allows. A task of Nextflow, Snakemake or CWL runs as the user who
runs iunctus. A task that runs as Kubernetes runs it (`TaskSpec::run_as`, for Argo) runs as
the user of its image, or the one that it names, and that user may differ from the user
who runs iunctus. Then:

- its folders in the scratch directory are open to every user, so that it may write
  there; the folder of all scratch directories stays private to the user;
- when it runs as root, it keeps the capability `CHOWN`, and its wrapper hands what the
  task made back to the user at its end;
- otherwise the wrapper opens what the task made to every user, and the local executor
  hands it back with a short container of its own, as root with only `CHOWN` and
  `DAC_READ_SEARCH`.

A rootless engine maps root of the container to the user who runs iunctus, so a task that
runs as root there needs none of this.

### `executor/src/containers.rs`

Tasks of several containers, like an Argo container set: the containers run at once in one
place, as the containers of one Kubernetes pod do, and share the work directory, the inputs
and the control folder of the task.

Each container runs a wrapper of its own. It waits until the containers that it comes after
recorded their exit codes, and runs its command only when all of them ended with 0, as
Argo's executor does; else it fails without running. It runs a failed command again while
it has attempts left, as the `retryStrategy` of an Argo container set asks, unless the shell
could not find the command (exit code 127). It records its exit code and streams in its own
folder below `containers/` in the control folder. The main container, the first, uses the
control folder itself, as the wrapper of a task of one container does, so its standard
output is the task's. After its own end it waits for every container that does not come
after it, then collects the outputs and writes the manifest.

Containers that share an image can run in one container of the engine: its wrapper starts
their wrappers at once and waits for all of them.

### `executor/src/engine.rs`

The container engine of this machine, Docker or Podman, found without any config and reached
through the Docker Engine API over its socket, so that no program like `docker` is needed;
and the command of an engine on another machine, for job scripts.

### `executor/src/http.rs`

The one HTTP client of the process for executors and the task runner: checks of images at
their registries, and requests to TES servers. It is the client of the storage crate, which
the storage providers share, so that the whole process reuses connections and looks up a
host's address once.

### `executor/src/pool.rs`

The CPUs and memory that local tasks share. A task starts when its share is free. Tasks that
wait take their shares one after another: the one with the highest priority first, and of
equal ones the one that asked first.

### `executor/src/process.rs`

A container whose main process answers lines: it reads one line on standard input and
writes one reply line on standard output, like the Python process of decision 19. The
container keeps running between lines, and is removed when it stops. The containers talk to
the engine through its API, on a runtime of this module, since their callers wait for them
without a runtime of their own.

### `executor/src/registry.rs`

Image digests and configs from their registry, for backends without a local container
engine, like TES or a cluster: a `HEAD` request for the manifest of the tag gives the
digest, as `docker pull` finds it, and the config of the image gives its entrypoint and
default command. Registries that ask for a token, like Docker Hub, quay.io and ghcr.io, give
an anonymous one for public images. A registry on `localhost` is asked over plain HTTP, as
Docker asks it.

### `executor/tests/process.rs`

Containers that answer lines, and containers that run to their end, with the local Docker
or Podman through its API. Needs the `debian:stable-slim` image; run with
`cargo test -p executor --test process -- --ignored`.

## File descriptions: slurm-executor

### `slurm-executor/src/backend.rs`

Runs an attempt of a task as one SLURM batch job: the inputs are placed in the scratch
directory of the attempt, and the job runs the wrapper in the task's container on a node.
So the nodes must see the scratch directory and the store at the same paths, as on a
cluster with a shared file system.

### `slurm-executor/src/images.rs`

The images of Apptainer and Singularity as SIF files, made once per image by a job of their
own. Jobs that each turn the same image of a registry into a SIF file at the same time spoil
the shared cache of Apptainer ("conveyor failed to get: unexpected end of JSON input"), and
each spends the time of its task on it. So the first task of an image waits for one job
that pulls it into the folder of images, and every job then runs that file.

### `slurm-executor/src/lib.rs`

The SLURM executor: each attempt of a task runs as one batch job, which runs the wrapper in
the task's container with Apptainer, Singularity, Podman or Docker on a node. The nodes see
the store and the scratch directories at the same paths, through a shared file system.

### `slurm-executor/src/queue.rs`

What SLURM says about the jobs of a run. One `squeue` asks about all jobs at once, at most
once per poll interval, so that many tasks do not load the scheduler. The tools write JSON
where they can (`--json`, since SLURM 21.08); older ones write text, which is read then.

### `slurm-executor/tests/simulated_slurm.rs`

The SLURM backend against a simulated SLURM on this machine: `sbatch` runs the job script in
the background, `squeue` tells whether it still runs, `scancel` stops it, and the cluster
keeps no accounting. A simulated Apptainer runs the wrapper without a container, which works
because the job sees the files at the same paths. One test runs a real container with
Docker and needs the `debian:stable-slim` image; run it with `-- --ignored`.

## Source contracts: slurm-executor

### `slurm-executor/src/backend.rs:212`

The command that a container of `image` runs for these arguments. An image of a
registry runs them through its entrypoint, or its default command when there are none,
as `docker run`, the local executor and Kubernetes run them; a task that replaces the
entrypoint runs them as they are. A SIF file or an address runs them as they are, as
Apptainer's `exec` does.

### `slurm-executor/src/backend.rs:255`

The config of an image of a registry. An image pinned to its digest never changes, so
its config is kept in a file next to its SIF file and asked of the registry only once:
Docker Hub counts each request for a manifest against a limit per address, which all
nodes of a cluster may share.

### `slurm-executor/src/backend.rs:468`

The image as the jobs run it: with Apptainer or Singularity, the SIF file of the image,
pulled once by a job of its own, else the image itself. The file of an image pinned to
its digest, or of an address, serves later runs too; that of a tag is pulled once per
run, as the tag may move.

### `slurm-executor/src/backend.rs:498`

The SIF file of an image, pulled once by a job of its own. A file that is already there
counts only when it is `lasting`. The tasks of the image share one pull per run, also
when it fails: a failed pull is tried again a few times, with a growing pause, and then
every task of the image fails with the same error.

### `slurm-executor/src/backend.rs:1037`

The exit code of the task and a note for its standard error, from the exit code that the
wrapper recorded and the end of the job; `Err` when the machine failed rather than the task.

The wrapper records the tool's exit code and ends with 0. A container stopped by a signal,
as for lack of memory, ends the task with its own exit code. A job that ran out of time or
memory ends the task as a stopped container would. Other ends are failures of the machine,
like a container runtime that could not start the image, or a node that failed.

### `slurm-executor/src/images.rs:30`

The name of the SIF file of an image that is pinned to its digest, like
`debian@sha256:ab` as `sha256-ab.sif`, or of a SIF file at an `http://` or `https://`
address, like those that nf-core modules name for Apptainer, by the digest of its address;
`None` for any other image, which the jobs take as it is: a tag that may move, a path, or
an address with another scheme.

### `slurm-executor/src/images.rs:50`

The name of the SIF file of an image of a registry named by a tag, like
`debian:stable-slim` as `tag-<digest of the name>.sif`. A tag may move, so a run never takes
such a file from an earlier run: it pulls the image again, once for all its jobs. `None` for
a path, a SIF file, an address with a scheme, and an image that [`file_name`] names.

### `slurm-executor/src/images.rs:65`

The script of the job that pulls an image into `file`. It writes a file of its own first,
syncs it and moves it into place at the end, so that no task reads half a file, also on
another node of a shared file system, and two runs that pull the same image at once do not
spoil each other's file. `exit` gets the exit code.

### `slurm-executor/src/script.rs:119`

The job script: it waits until the node sees the whole wrapper, runs the wrapper in the
task's container, makes what the task wrote durable, then records the exit code of the
container. Podman and Docker get an init process, which hands the signal that stops the job
to the task, so that the container ends with the job. A task with a `time` runs under
`timeout`, as SLURM counts the time of a job in whole minutes. `size` is the length of the
wrapper in bytes.

### `slurm-executor/src/script.rs:151`

The job script of a task of several containers: it starts a container for each of `runs`,
one for each image, all at once, as the local executor does, then records the exit code of
the first container that failed, else 0. `exits` holds for each run the files of the exit
codes of its containers: a container of the engine that fails without its wrappers, as when
it is killed for lack of memory, records its exit code there, so that the containers that
wait for one of them go on. `sizes` holds the length in bytes of the wrapper of each run,
which the job waits to see whole, as [`job_script`] does.

### `slurm-executor/src/script.rs:200`

The lines that wait until the node sees each file at its length in bytes. A shared file
system like CephFS may show a file that another machine has just written as empty or short
for a while, and a shell runs an empty wrapper as a task that did nothing and ended well.
A file that the node does not see whole in time ends the job with [`NOT_SEEN`] in `exit`.

### `slurm-executor/src/script.rs:267`

The command of Apptainer: a container with its own `/tmp`, home and environment, which sees
the scratch directory at the same path, the work directory, and the other mounts. It runs
as the user who runs iunctus, without setuid or fakeroot. A task that runs as Kubernetes
runs it starts in its `workdir`, or in `/`, the working directory of most images, which a
job cannot ask the registry for; its file system is writable in memory
(`--writable-tmpfs`), so that it may write outside its folders where its user may write.
A task without network requests an isolated network. Sites or runtimes that cannot
provide it must fail the container launch.

## File descriptions: kubernetes-executor

### `kubernetes-executor/src/backend.rs`

Runs an attempt of a task as one Kubernetes Job: the inputs are placed in the scratch
directory of the attempt, and the pod runs the wrapper in the task's image with the folders
of the task as `hostPath` volumes. So the nodes must see the scratch directory and the store
at the same paths, as a `kind` cluster whose node mounts them, or nodes with a shared file
system.

### `kubernetes-executor/src/cluster.rs`

How the backend reaches the cluster: a client of the Kubernetes API, made from the kubeconfig
that the run names. It never reads the default kubeconfig or its contexts, so that no run
reaches a cluster by accident, and it needs no program like `kubectl`.

### `kubernetes-executor/src/lib.rs`

The Kubernetes executor: each attempt of a task runs as one Kubernetes Job with one pod,
which runs the wrapper in the task's image. The nodes see the store and the scratch
directories at the same paths, as `hostPath` volumes. The backend talks to the Kubernetes API
itself, with the kubeconfig that the run names; there is no default.

### `kubernetes-executor/src/pods.rs`

What Kubernetes says about the pods of the Jobs of a backend. One request lists all of them
at once, by the label of the backend, at most once per poll interval, so that many tasks do
not load the API server.

### `kubernetes-executor/tests/fake_api.rs`

The Kubernetes backend against a small fake Kubernetes API server on this machine, which
the backend reaches through a kubeconfig of the test. Creating a Job runs the command of its
container in the background, without a container, which works because the pod sees the
files at the same paths; listing pods tells how each Job stands; deleting a Job stops it.
The server refuses every request without the token of the test's kubeconfig.

## Source contracts: kubernetes-executor

### `kubernetes-executor/src/backend.rs:630`

The exit code of the task and a note for its standard error, from the exit code that the
wrapper recorded and the end of the container; `Err` when the machine failed rather than
the task.

The wrapper records the tool's exit code and ends with 0. A container killed for lack of
memory or stopped by a signal ends the task with its own exit code, and a pod that ran out
of time with 143. Other ends are failures of the machine, like an image without Bash, or a
pod that was evicted.

### `kubernetes-executor/src/manifest.rs:72`

The Job that runs the wrapper of each of `runs` in its image, labelled with `label`: one
container for a task, or one for each image of a task of several containers, which share
the folders of the task. The first container asks for the CPUs and memory of the task, and
each may use all of its memory.

## File descriptions: tes-executor

### `tes-executor/src/backend.rs`

Runs an attempt of a task as one TES task: the wrapper is its one executor, the inputs come
from their `file://` places, and the outputs and control files go back to the scratch
directory of the attempt. So the server must see the files of this machine at the same
paths, as Funnel on this machine or on a cluster with a shared file system does.

### `tes-executor/src/client.rs`

A small client of the GA4GH TES v1.1 API: the parts of a task that iunctus sends, and the
calls to create a task, read its state and cancel it. Field names are those of the OpenAPI
file, in snake case.

### `tes-executor/tests/funnel.rs`

The TES backend against a real Funnel server on this machine, which `just funnel-server`
starts. Needs Docker and the `debian:stable-slim` image; run with `just tes-test`, which
sets `IUNCTUS_TES_SERVER`. Funnel must be allowed to read and write the temporary folder.
