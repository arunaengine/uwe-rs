<!-- How the real executors run tasks, and how to set each one up. -->
<!-- Copyright (c) 2026 The iunctus Contributors -->
<!-- SPDX-License-Identifier: MIT or Apache-2.0 -->

# Executors

An executor runs the tasks of a run. The simulator only pretends ([the engine](engine.md), section 4). The real executors run each task in a container: on this machine, on a GA4GH TES server, or as SLURM batch jobs. A Kubernetes executor is planned (decision 22). This page tells how each one works and how to set it up.

## 1. What all real executors share

All real executors run through the task runner (`task_runner::RunnerExecutor`). Only the backend differs (`executor::Backend`), which runs one attempt of a task:

```rust
pub trait Backend: Send + Sync + 'static {
    fn pin(&self, image: &str) -> impl Future<Output = Result<Option<String>, ExecutorError>> + Send;
    fn run(&self, job: Job<'_>, cancel: &CancellationToken) -> impl Future<Output = Result<Finished, ExecutorError>> + Send;
}
```

The runner does the rest, the same for every backend:

- It pins each image to its digest once per run (`pin`), so that every task of the run uses the same image and the cache key names it.
- It answers a task from the store when the store has its outputs already.
- It stages the inputs: local files, literal content, files of the store, and remote files, which it downloads once into `<cache>/downloads`.
- It adopts the outputs into the store and fills the outcome: the exit code, the output files with size and digest, the attempt, the pinned image and the times.
- It sorts errors: a failure of the machine (`ExecutorError::Engine`) lets the engine run the task again, twice by default, and a cancelled attempt ends quietly.

Each attempt gets a scratch directory, `<scratch>/<run>-<task>-<attempt>`, with the folders `work` (the work directory of the task) and `control` (the wrapper and its records). The wrapper (`executor::wrapper`) runs the command of the task with its streams captured in `control`, records the exit code, and on success lists the outputs in the manifest. Every backend runs the same wrapper, so a task behaves the same everywhere.

Inputs that the task only reads are symbolic links to their sources, and the container sees the folders of the sources read-only at the same paths (`executor::place`). Inputs that the task may change or run are copies.

## 2. Local containers

```sh
iunctus run main.nf -executor local
```

The local executor needs only Docker or Podman. It takes the engine that `IUNCTUS_ENGINE` names, else Docker when its service answers, else Podman, and it handles rootless engines. Each task runs as the current user, with `--rm`, and with the CPUs and memory it asks for (`--cpus`, `--memory`). Tasks wait for their share of the CPUs and memory of this machine, so parallel tasks never ask for more than the machine has. A task that runs longer than its time is stopped. Ctrl-C stops all containers of the run.

## 3. GA4GH TES

```sh
iunctus run main.nf -executor tes -tes-server http://localhost:8000
```

Each attempt becomes one TES v1.1 task with the wrapper as its one executor. Its inputs are `file://` URLs of the store, the wrapper and the staged inputs; its outputs are the outbox of the wrapper, which it copies back into the work directory, and the control folder. So the TES server must see the store and the scratch directories of this machine at the same paths: Funnel on this machine, or a server on a cluster with a shared file system. Inputs are copied into the store first (`Settings::import_inputs`).

- `-tes-server <address>` names the server; Funnel's `http://localhost:8000` is the default.
- `IUNCTUS_TES_TOKEN` gives a bearer token, when the server needs one.
- Images are pinned through their registry, which needs no container engine on this machine.
- A task without a command fails: TES needs one, and the default command of the image is not known without its engine.
- The state `SYSTEM_ERROR` and failed requests are failures of the machine. A cancelled run cancels its TES tasks.

The TES backend is tested against a small fake TES server (`tes-executor/tests/fake_server.rs`). A test with Funnel is still to come.

## 4. SLURM

```sh
iunctus run main.nf -executor slurm -slurm-container apptainer -slurm-options '--account=lab'
```

Each attempt becomes one batch job (`slurm-executor`). The backend places the inputs in the scratch directory, writes the wrapper and the job script into its `control` folder, and submits the job with `sbatch`. The job runs the wrapper in the task's container on a node and then records the exit code of the container in `control/container-exit`. So the nodes must see the store and the scratch directories at the same paths. The defaults, `store` and `scratch/slurm` in the cache directory (`$XDG_CACHE_HOME/iunctus`, else `~/.cache/iunctus`), are in the home folder, which clusters usually share. Inputs are copied into the store first.

The options of `sbatch` come from the task: `--cpus-per-task`, `--mem`, `--time`, `--gpus`, and `--partition` from the queue of the task. `-slurm-options` adds more, like `--account=lab --qos=low`; they come last and so win. The output of a job goes to `control/slurm.log`.

`-slurm-container` says how the nodes run containers:

| Runtime | How the job runs the task |
|---|---|
| `apptainer` (default), `singularity` | `apptainer exec --containall` with the scratch directory, the work directory and the mounts bound, and `--nv` for tasks with GPUs. Images of registries get `docker://`; a path to a `.sif` file stays as it is. The container runs inside the job, so SLURM limits and stops it. |
| `podman` | `podman run --init` as the user (`--userns=keep-id`), with the same limits as the job. The init process hands the signal that stops the job to the task, so the container ends with the job. |
| `docker` | as Podman, as the user with `--user`. |

The backend asks `squeue` about all jobs of the run at once, at most every 10 seconds, so that many tasks do not load the scheduler. `squeue --states=all` also shows jobs that ended lately; for a job that SLURM has forgotten, `sacct` tells how it ended, when the cluster keeps accounting. How a job ends decides the outcome:

- The job recorded the exit code of its container, and the wrapper recorded the exit code of the tool: that is the exit code of the task.
- A container stopped by a signal, like one killed for lack of memory, ends the task with its own exit code and a note on standard error.
- `TIMEOUT` ends the task with 143 and `OUT_OF_MEMORY` with 137, each with a note, as a stopped container would.
- Every other end is a failure of the machine, which the engine retries: `NODE_FAIL`, `PREEMPTED`, `BOOT_FAIL`, a job cancelled by someone else, a container runtime that could not start the image, or a job that left no record. The error quotes the end of the job's log. A job that left no log at all usually means that the nodes do not see the scratch directory.

A cancelled run cancels its jobs with `scancel` and waits until they leave the queue.

The tests run the backend against a simulated SLURM on this machine (`slurm-executor/tests/simulated_slurm.rs`): shell scripts in place of `sbatch`, `squeue`, `scancel` and `sacct`, and a simulated Apptainer that runs the wrapper without a container. One test runs a real container with Docker. A test on a SLURM cluster in containers is still to come.

## 5. Kubernetes

Planned (decision 22): one Kubernetes Job per task, with the store and the scratch directories on volumes that the pods share. Tests will use only a local `kind` cluster with a kubeconfig of its own, never the kubeconfig of the user.
