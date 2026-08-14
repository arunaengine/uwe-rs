<!-- How the real executors run tasks, and how to set each one up. -->
<!-- Copyright (c) 2026 The iunctus Contributors -->
<!-- SPDX-License-Identifier: MIT or Apache-2.0 -->

# Executors

An executor runs the tasks of a run. The simulator only pretends ([the engine](engine.md), section 4). The real executors run each task in a container: on this machine, on a GA4GH TES server, as SLURM batch jobs, or as Kubernetes Jobs (decision 22). This page tells how each one works and how to set it up.

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
- **A store elsewhere**, like `s3://lab-cache/iunctus` ([storage](storage.md)), works with every real executor, and its data goes through this machine: the runner downloads each input that only the store has into `<cache>/downloads` (once per content), and uploads each output into the store after the attempt. So with TES, SLURM and Kubernetes the servers and nodes must see `<cache>/downloads` and the scratch folders at the same paths, as they must see the store folder of a local store; they never need the credentials of the store. A TES server or nodes that read the store themselves, without this machine in between, are a next step. With a folder in front of the store (`local` in `[store]`, [the cache](cache.md), section 8), what this machine reads once stays in that folder.
- It adopts the outputs into the store and fills the outcome: the exit code, the output files with size and digest, the attempt, the pinned image and the times.
- It sorts errors: a failure of the machine (`ExecutorError::Engine`) lets the engine run the task again, twice by default, and a cancelled attempt ends quietly. An image that its registry does not have, or does not let this machine pull, fails the task at once (`ExecutorError::MissingImage`), with the advice to check its name and tag or to log in to the registry, since another attempt would pull in vain.

Each attempt gets a scratch directory, `<scratch>/<run>-<task>-<attempt>`, with the folders `work` (the work directory of the task), `control` (the wrapper and its records) and `home` (the home folder of the task, which the wrapper sets as `HOME`; on TES it is `/iunctus/home`, a volume of the task). The wrapper (`executor::wrapper`) runs the command of the task with its streams captured in `control`, records the exit code, and on success lists the outputs in the manifest. Every backend runs the same wrapper, so a task behaves the same everywhere.

Inputs that the task only reads are symbolic links to their sources, and the container sees the folders of the sources read-only at the same paths (`executor::place`). Inputs that the task may change or run are copies.

**Tasks of several containers**, like an Argo container set, run their containers at once in one place: they share the scratch directory, the inputs and the mounts. Each container runs a wrapper of its own (`executor::containers`) that waits until the containers it comes after recorded their exit codes, runs its command only when they ended with 0, runs it again while it fails and has attempts left, and records its exit code and streams in `control/containers/<name>/`. The main container uses `control/` itself and collects the outputs. The exit code of the task is that of the main container when it failed, else that of the first other container that failed. The local executor, Kubernetes and SLURM start one container for each image, all at once; containers of one image run in one container, whose wrapper starts theirs and waits for all. TES runs one executor per container, one after the other, in an order where each comes after those it waits for.

**Settings in the config file.** Each executor takes its settings from a section `[executor.<name>]` of the config file ([storage](storage.md), section 1), so that a run needs no long command line. Options on the command line and environment variables win over the file. A key that a section does not know is an error.

```toml
[executor.local]
engine = "podman"                    # like IUNCTUS_ENGINE: docker, podman or a socket

[executor.tes]
server = "https://tes.example.org"   # like -tes-server
token = { environment = "TES_TOKEN" } # like IUNCTUS_TES_TOKEN

[executor.tes.sites.hamburg]         # more TES servers, each near some data
server = "https://tes.hamburg.example.org"
near = ["s3://hh-sequencing/", "/mnt/hamburg/"]

[executor.slurm]
container = "apptainer"              # like -slurm-container
options = ["--account=lab", "--qos=low"]   # like -slurm-options

[executor.kubernetes]
kubeconfig = "/home/me/kind.kubeconfig"    # like -kubeconfig
namespace = "iunctus"                # like -kubernetes-namespace
```

## 2. Local containers

```sh
iunctus run main.nf -executor local
```

The local executor needs only Docker or Podman, and no program of theirs: it talks to the engine through the Docker Engine API on its socket (bollard). It takes the engine that `IUNCTUS_ENGINE` names (`docker`, `podman`, or a socket like `/run/user/1000/podman/podman.sock`), else Docker when its socket answers (`DOCKER_HOST`, `/var/run/docker.sock`, or the socket of Docker without root), else Podman through its Docker-compatible socket (`$XDG_RUNTIME_DIR/podman/podman.sock`, `/run/podman/podman.sock`). When none answers, the error says how to turn one on; for Podman that is `systemctl --user enable --now podman.socket`. `iunctus check-machine` tells which engine answers, and checks the other executors of the config file too ([checks](checks.md), section 4). Each task runs as the current user, with the CPUs and memory it asks for; an engine without root runs it as root inside, which is the user outside. A task without network, like a CWL tool that does not ask for it, or any task whose workflow does not say with `-no-network`, gets none ([reproducibility](reproducibility.md), section 6); TES, Kubernetes and SLURM with Apptainer or Singularity cannot cut a task off the network yet, and a run that asks for it there stops before its first task. The engine pulls an image only when the runner pins it; the container itself never pulls one. The command of a task runs through the entrypoint of its image, as `docker run` runs it, which images like those of Biocontainers need to set up their tools. The wrapper starts in the `sh` of the image and goes on in Bash where the image has it, wherever its `PATH` has it; in an image without Bash, like plain Alpine, it runs in `sh`, whose output patterns know no `**` and leave hidden files out of `*`. An image without any shell stops the task with an error that says so. Tasks wait for their share of the CPUs and memory of this machine, so parallel tasks never ask for more than the machine has. A task that runs longer than its time is stopped. Ctrl-C stops all containers of the run.

## 3. GA4GH TES

```sh
iunctus run main.nf -executor tes -tes-server http://localhost:8000
```

Each attempt becomes one TES v1.1 task with the wrapper as its one executor. Its inputs are `file://` URLs of the store, the wrapper and the staged inputs; its outputs are the outbox of the wrapper, which it copies back into the work directory, and the control folder. So the TES server must see the store and the scratch directories of this machine at the same paths: Funnel on this machine, or a server on a cluster with a shared file system. Inputs are copied into the store first (`Settings::import_inputs`).

- `-tes-server <address>` names the server; Funnel's `http://localhost:8000` is the default.
- `IUNCTUS_TES_TOKEN` gives a bearer token, when the server needs one.
- `[executor.tes.sites.<name>]` names more TES servers, each with the address prefixes or folders of the data near it (`near`); each task goes to the site near most of its input files, and the others to the server above ([efficiency](efficiency.md), section 3).
- Before the first task, iunctus asks the server once whether it answers (`GET /service-info`). A server that does not answer, or that refuses the token, stops the run at once with exit code 3, instead of after the retries of the first task.
- Images are pinned through their registry, which needs no container engine on this machine.
- The command of a task runs through the entrypoint of its image, as with the local executor; the entrypoint and the default command come from the config of the image at its registry, read once per image (`executor::registry::ImageConfigs`). A task without a command runs the default command of the image.
- The state `SYSTEM_ERROR` and failed requests are failures of the machine. A cancelled run cancels its TES tasks.

A task of several containers becomes one TES task with one executor per container (section 1).

The TES backend is tested against a small fake TES server (`tes-executor/tests/fake_server.rs`) and against Funnel 0.11.14 on this machine: `just funnel-server` starts Funnel from `~/.cache/iunctus/tools/funnel` with the config of the tests (`tes-executor/tests/fixtures/funnel.yaml`: a memory limit and no network for tasks, and images are never pulled), and `just tes-test` runs the tests against it. A whole Nextflow workflow ran on it too: two processes with an input file and `publishDir`, Ctrl-C cancelled the TES tasks, and a task whose image was missing failed as a failure of the machine, twice retried. A test with TESK is still to come.

## 4. SLURM

```sh
iunctus run main.nf -executor slurm -slurm-container apptainer -slurm-options '--account=lab'
```

Each attempt becomes one batch job (`slurm-executor`). The backend places the inputs in the scratch directory, writes the wrapper and the job script into its `control` folder, and submits the job with `sbatch`. The job runs the wrapper in the task's container on a node and then records the exit code of the container in `control/container-exit`. So the nodes must see the store and the scratch directories at the same paths. The defaults, `store` and `scratch/slurm` in the cache directory (`$XDG_CACHE_HOME/iunctus`, else `~/.cache/iunctus`), are in the home folder, which clusters usually share. Inputs are copied into the store first.

The options of `sbatch` come from the task: `--cpus-per-task`, `--mem`, `--time`, `--gpus`, and `--partition` from the queue of the task. `-slurm-options` adds more, like `--account=lab --qos=low`; they come last and so win. The output of a job goes to `control/slurm.log`.

`-slurm-container` says how the nodes run containers:

| Runtime | How the job runs the task |
|---|---|
| `apptainer` (default), `singularity` | `apptainer exec --containall` with the scratch directory, the work directory and the mounts bound, and `--nv` for tasks with GPUs. An image pinned to its digest is pulled once into a SIF file in `<cache>/scratch/slurm/images`, by a small job of its own (`iunctus-image-...`, 2 CPUs, 4 GB, one hour) before the first task that needs it; every job then runs that file. Jobs that each pull the same image at once spoil Apptainer's shared cache. Other images of registries get `docker://`; a path to a `.sif` file stays as it is. The container runs inside the job, so SLURM limits and stops it. |
| `podman` | `podman run --init` as the user (`--userns=keep-id`), with the same limits as the job. The init process hands the signal that stops the job to the task, so the container ends with the job. |
| `docker` | as Podman, as the user with `--user`. |

The SLURM executor uses the tools that every SLURM node has, `sbatch`, `squeue`, `sacct` and `scancel`, so it needs no config (decision 26). A run on a machine without `sbatch`, `squeue` or `scancel` on the PATH stops before its first task, with exit code 3 and the advice to run iunctus on a node of the cluster. The backend asks `squeue` about all jobs of the run at once, at most every 10 seconds, so that many tasks do not load the scheduler. It reads `squeue --json` and `sacct --json` where SLURM writes JSON (since 21.08; the state is a word before 23.02 and a list of words since), and their text where it does not. `squeue --states=all` also shows jobs that ended lately; for a job that SLURM has forgotten, `sacct` tells how it ended, when the cluster keeps accounting. How a job ends decides the outcome:

- The job recorded the exit code of its container, and the wrapper recorded the exit code of the tool: that is the exit code of the task.
- A container stopped by a signal, like one killed for lack of memory, ends the task with its own exit code and a note on standard error.
- `TIMEOUT` ends the task with 143 and `OUT_OF_MEMORY` with 137, each with a note, as a stopped container would. SLURM counts the time of a job in whole minutes, so the job script also stops the container at the task's time to the second (`timeout`, then a kill after 10 seconds), which ends the task with 143 as well.
- Every other end is a failure of the machine, which the engine retries: `NODE_FAIL`, `PREEMPTED`, `BOOT_FAIL`, a job cancelled by someone else, a container runtime that could not start the image, or a job that left no record. The error quotes the end of the job's log. A job that left no log at all usually means that its node does not see the scratch directory; the backend then asks SLURM for that node and leaves it out of the later jobs of the run (`--exclude`), so that the retries go elsewhere.

A cancelled run cancels its jobs with `scancel` and waits until they leave the queue.

A task of several containers is one job, whose script starts a container for each image at once, waits for all of them, and records the exit code of the first container that failed, else 0 (section 1).

The tests run the backend against a simulated SLURM on this machine (`slurm-executor/tests/simulated_slurm.rs`): shell scripts in place of `sbatch`, `squeue`, `scancel` and `sacct`, and a simulated Apptainer that runs the wrapper without a container. One test runs a real container with Docker. A test on a SLURM cluster in containers is still to come.

## 5. Kubernetes

```sh
iunctus run main.nf -executor kubernetes -kubeconfig kind.kubeconfig [-kubernetes-namespace <name>]
```

Each attempt becomes one Kubernetes Job with one pod (`kubernetes-executor`). The backend places the inputs in the scratch directory and writes the wrapper, then creates the Job through the Kubernetes API, with its own client (kube-rs), so no `kubectl` is needed. The pod runs the wrapper in the task's image as the user who owns the scratch directory, with the scratch directory, the work directory and the other mounts as `hostPath` volumes at the same paths. So the nodes must see the store and the scratch directories (`scratch/kubernetes` in the cache directory) at the same paths: one node that mounts them, like `kind`, or nodes with a shared file system. Inputs are copied into the store first.

The client reads only the kubeconfig that `-kubeconfig` names and uses its current context. iunctus never takes the default kubeconfig or `KUBECONFIG`, so that no run reaches a cluster by accident; without `-kubeconfig` the run stops at once. The Jobs go to `-kubernetes-namespace`, else to the namespace of the kubeconfig's context.

The command of a task runs through the entrypoint of its image, as with the local executor; the backend reads the entrypoint and the default command from the config of the image at its registry, once per image, since the pod runs the wrapper as its own command. A task without a command runs the default command of the image.

The pod asks for the CPUs and memory of the task, and gets the memory as its limit and the GPUs as `nvidia.com/gpu`. A task of several containers is one pod with a container for each image (section 1): the first asks for the CPUs and memory of the task, and each may use all of its memory; the pod ends when all of them ended, or when one ended badly, like one killed for lack of memory. The time of the task is the pod's `activeDeadlineSeconds`, which counts from the start of the pod, so that waiting for a node does not count. A Job has no retries of its own (`backoffLimit: 0`), since the engine retries failures of the machine, and the backend deletes it once it ended; `ttlSecondsAfterFinished` removes it too.

The backend lists the pods of all Jobs of the run at once, by a label, at most every 5 seconds. How the container ends decides the outcome, as for the other executors: the exit code that the wrapper recorded; a container killed for lack of memory (`OOMKilled`) or by a signal ends the task with its own exit code and a note; a pod past its time (`DeadlineExceeded`) ends it with 143. A pod whose image cannot be pulled, a Job without a pod, each for 2 minutes, an evicted pod, and a container that could not run the wrapper are failures of the machine, which the engine retries; the error quotes the end of the pod's log. A cancelled run deletes its Jobs and waits until their pods are gone.

A local cluster with `kind`, whose node sees the cache of iunctus at the same path:

```sh
cat > kind.yaml <<END
kind: Cluster
apiVersion: kind.x-k8s.io/v1alpha4
nodes:
- role: control-plane
  extraMounts:
  - hostPath: $HOME/.cache/iunctus
    containerPath: $HOME/.cache/iunctus
END
kind create cluster --name iunctus --config kind.yaml --kubeconfig kind.kubeconfig
kind load docker-image debian:stable-slim --name iunctus   # images already on this machine
iunctus run main.nf -executor kubernetes -kubeconfig kind.kubeconfig
kind delete cluster --name iunctus --kubeconfig kind.kubeconfig
```

The tests run the backend against a small fake Kubernetes API server on this machine (`kubernetes-executor/tests/fake_api.rs`), which runs the command of the container without a container and refuses every request without the token of the test's kubeconfig. A test on a `kind` cluster is still to come.
