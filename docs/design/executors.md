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
- **A store elsewhere**, like `s3://lab-cache/iunctus` ([storage](storage.md)), works with every real executor. The runner downloads inputs for local preparation and uploads outputs after the attempt. Shared-path TES, SLURM and Kubernetes need matching paths for those downloads and scratch folders. TES object-store staging (section 3) sends S3 input addresses to workers and reads their S3 outputs, so workers need no shared filesystem. With a folder in front of the store (`local` in `[store]`, [the cache](cache.md), section 8), what this machine reads once stays in that folder.
- It adopts the outputs into the store and fills the outcome: the exit code, the output files with size and digest, the attempt, the pinned image and the times.
- It sorts errors: a failure of the machine (`ExecutorError::Engine`) lets the engine run the task again, twice by default, and a cancelled attempt ends quietly. An image that its registry does not have, or does not let this machine pull, fails the task at once (`ExecutorError::MissingImage`), with the advice to check its name and tag or to log in to the registry, since another attempt would pull in vain.

Each attempt gets a scratch directory, `<scratch>/<run>-<task>-<attempt>`, with the folders `work` (the work directory of the task), `control` (the wrapper and its records) and `home` (the home folder of the task, which the wrapper sets as `HOME`). The wrapper (`executor::wrapper`) captures streams in `control`, records the exit code, and lists outputs on success. TES object-store mode uses this wrapper for shell tasks and runs other commands directly, with TES logs (section 3).

Inputs that the task only reads are symbolic links to their sources, and the container sees the folders of the sources read-only at the same paths (`executor::place`). Inputs that the task may change or run are copies.

**Tasks of several containers**, like an Argo container set, run their containers at once in one place: they share the scratch directory, the inputs and the mounts. Each container runs a wrapper of its own (`executor::containers`) that waits until the containers it comes after recorded their exit codes, runs its command only when they ended with 0, runs it again while it fails and has attempts left, and records its exit code and streams in `control/containers/<name>/`. The main container uses `control/` itself and collects the outputs. The exit code of the task is that of the main container when it failed, else that of the first other container that failed. The local executor, Kubernetes and SLURM start one container for each image, all at once; containers of one image run in one container, whose wrapper starts theirs and waits for all. A container that dies without its wrapper, as when it is killed for lack of memory, records nothing itself, so the others would wait for it until the time limit: the script that started it (the wrapper of its group, or the SLURM job script) records its exit code for it when it fails, so that the containers that wait for it go on and the task fails. The local executor stops the other containers when one ends without its wrapper, and Kubernetes ends the pod when one of its containers fails. TES refuses tasks with several containers or container retry dependencies because it submits exactly one executor.

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

[executor.slurm.pull]                # the job that pulls an image into a SIF file
cpus = 2                             # the defaults: 2 CPUs, 4 GB and one hour
memory_mb = 4096
minutes = 60

[executor.kubernetes]
kubeconfig = "/home/me/kind.kubeconfig"    # like -kubeconfig
namespace = "iunctus"                # like -kubernetes-namespace
```

## 2. Local containers

```sh
iunctus run main.nf -executor local
```

The local executor needs only Docker or Podman, and no program of theirs: it talks to the engine through the Docker Engine API on its socket (bollard). It takes the engine that `IUNCTUS_ENGINE` names (`docker`, `podman`, or a socket like `/run/user/1000/podman/podman.sock`), else Docker when its socket answers (`DOCKER_HOST`, `/var/run/docker.sock`, or the socket of Docker without root), else Podman through its Docker-compatible socket (`$XDG_RUNTIME_DIR/podman/podman.sock`, `/run/podman/podman.sock`). When none answers, the error says how to turn one on; for Podman that is `systemctl --user enable --now podman.socket`. `iunctus check-machine` tells which engine answers, and checks the other executors of the config file too ([checks](checks.md), section 4). Each task runs as the current user, with the CPUs and memory it asks for; an engine without root runs it as root inside, which is the user outside. An Argo task runs as its image says, and every container gets the least permissions (section 6). A task without network, like a CWL tool that does not ask for it, or any task whose workflow does not say with `-no-network`, gets none ([reproducibility](reproducibility.md), section 6); SLURM jobs with Apptainer get a network with only loopback (`--net --network none`, which Apptainer 1.1 and later allows any user); TES, Kubernetes and SLURM with Singularity cannot cut a task off the network yet, and a run that asks for it there stops before its first task. The engine pulls an image only when the runner pins it; the container itself never pulls one. The command of a task runs through the entrypoint of its image, as `docker run` runs it, which images like those of Biocontainers need to set up their tools. The wrapper starts in the `sh` of the image and goes on in Bash where the image has it, wherever its `PATH` has it; in an image without Bash, like plain Alpine, it runs in `sh`, whose output patterns know no `**` and leave hidden files out of `*`. An image without any shell stops the task with an error that says so. Tasks wait for their share of the CPUs and memory of this machine, so parallel tasks never ask for more than the machine has. A task that runs longer than its time is stopped. Ctrl-C stops all containers of the run.

## 3. GA4GH TES

```sh
iunctus run main.nf -executor tes -tes-server http://localhost:8000
```

Each attempt becomes one TES v1.1 task with exactly one executor. The default `staging = "shared-path"` keeps the existing wrapper and `file://` inputs and outputs. This mode needs matching store and scratch paths on the worker. `staging = "object-store"` instead transfers FILE objects through S3 and needs no shared filesystem.

- `-tes-server <address>` names the server; Funnel's `http://localhost:8000` is the default.
- `IUNCTUS_TES_TOKEN` or `token` in the config gives a bearer token. `user` and `password` enable HTTP Basic authentication; both accept environment references. A bearer token takes precedence. Credentials are omitted from task requests and journal records, and hidden in client diagnostics.
- `[executor.tes.sites.<name>]` names more TES servers, each with the address prefixes or folders of the data near it (`near`); each task goes to the site near most of its input files, and the others to the server above ([efficiency](efficiency.md), section 3).
- Before the first task, iunctus asks the server once whether it answers (`GET /service-info`). A server that does not answer, or that refuses the token, stops the run at once with exit code 3, instead of after the retries of the first task.
- Images are pinned through their registry, which needs no container engine on this machine.
- The command of a task runs through the entrypoint of its image, as with the local executor; the entrypoint and the default command come from the config of the image at its registry, read once per image (`executor::registry::ImageConfigs`). A task without a command runs the default command of the image.
- `EXECUTOR_ERROR` is a permanent task failure, with the last executor log's exit code when present, else 1. `SYSTEM_ERROR` is retryable through the engine's machine retries. Status reads tolerate transient failures within the existing grace period. Cancellation waits through `CANCELING` until a terminal state.

TES refuses tasks of several containers and container retry dependencies with unsupported exit code 33.

### Using a TES + S3 service

```toml
[executor.tes]
server = "https://tes.example.org"
token = { environment = "TES_TOKEN" }
staging = "object-store"
staging_root = "s3://task-data/iunctus-tes"
# Set only when the server supports deduplication by this tag.
idempotency_tag = "example.org/idempotency-key"
max_inputs = 512
max_outputs = 1024

[storage.tasks]
type = "s3"
prefixes = ["s3://task-data/"]
endpoint = "https://objects.example.org"
region = "us-east-1"
access_key_id = { environment = "S3_ACCESS_KEY" }
secret_access_key = { environment = "S3_SECRET_KEY" }
```

The engine and TES workers need access to this bucket. Configure the worker's S3 endpoint and credentials on the TES service too. The cache store may stay local or use `[store] location = "s3://task-data/cache"`. Site tables accept the same TES keys and configure their own staging mode and authentication.

Inputs are uploaded under `cas/sha256/<2 hex>/<62 hex>` in the staging root. Existing content in the same bucket is referenced directly, including a shared S3 layer behind a local cache. Directory inputs expand into FILE entries with their relative layout. The limit includes the wrapper input. Outputs live under `attempts/<digest>/`, derived from the task specification and unique attempt name. Completion imports output bytes into the content store, checks reported sizes, and computes SHA-256 digests. A nonempty immutable `versionId` in an output log pins the S3 read to that version; the mutable S3 null version and other query parameters are refused.

Tasks whose argv or image default command starts with `sh`, `bash`, `dash`, `ash`, `ksh` or `zsh` use the existing task wrapper, including through an image entrypoint. It creates empty input directories, sets executable permissions and captures full stdout/stderr. Directory outputs and shell globs are collected into flat FILE objects under a wildcard output with `path_prefix`; a manifest restores nested paths, hidden files and empty directories. The container needs the shell and standard utilities used by the existing wrapper, including `mkdir`, `cp` and `chmod`. Other commands retain their full argv and environment, use POSIX output globs with `path_prefix`, and obtain stdout/stderr from the last TES executor log. TES logs may be absent, truncated or URL references, so they do not promise full stream bytes. This follows the [TES log and output contract](https://github.com/ga4gh/task-execution-schemas/blob/develop/openapi/task_execution_service.openapi.yaml).

Object-store mode refuses stdin, explicit user/group identity, symbolic-link inputs, updates to inputs in place, overlapping input/output paths, input wildcards, wildcard outputs without a non-root literal ancestor and excessive declarations. Empty directories, executable inputs, directory outputs and shell glob extensions require a task shell. GPU placement and offline tasks remain unsupported. These cases stop with unsupported exit code 33; malformed output records or missing transfer objects fail collection instead of publishing incomplete results.

`idempotency_tag` has no default. When set, its value hashes the original task cache key and unique attempt name, which includes the attempt number. The journal retains the exact request before submission. Recovery requires the original authentication and replays an ambiguous request with the same tag and destinations, then confirms termination of the orphan under the existing recovery policy. The server must implement atomic deduplication for the configured tag. Without it, a lost submission response remains unresolved for manual recovery. S3 attempt prefixes are retained; apply bucket lifecycle rules when retention is no longer needed.

The non-ignored strict TES and memory S3 tests cover transfers, direct argv, error classification, cancellation, duplicate submissions, lost-response recovery and refusal limits. They simulate server execution. The opt-in deployment test runs containers on a real TES worker and checks actual output bytes:

```sh
IUNCTUS_TES_SERVER=http://localhost:8000 \
IUNCTUS_TES_STAGING_ROOT=s3://task-data/iunctus-test \
IUNCTUS_S3_ENDPOINT=http://localhost:9000 \
AWS_ACCESS_KEY_ID=minio-user AWS_SECRET_ACCESS_KEY=minio-password \
nice cargo test -p tes-executor -j 8 --locked --test funnel transfers_s3_outputs -- --ignored --test-threads=1
```

Provision the bucket and make the same S3 service reachable from the worker, with its own credentials. Funnel or another TES service must support S3 FILE transfers and TES 1.1 wildcard outputs. The test uses `debian:stable-slim`, optionally `IUNCTUS_TES_TOKEN`, and leaves attempt objects in S3. It is ignored by default; real S3/TES versions, authentication, wildcard behavior, permissions and restart behavior still need deployment qualification.

Historical shared-path evidence covers a small fake TES server (`tes-executor/tests/fake_server.rs`) and Funnel 0.11.14 on this machine: `just funnel-server` starts Funnel from `~/.cache/iunctus/tools/funnel` with the config of the tests (`tes-executor/tests/fixtures/funnel.yaml`: a memory limit and no network for tasks, and images are never pulled), and `just tes-test` runs the tests against it. A whole Nextflow workflow ran on it too: two processes with an input file and `publishDir`, Ctrl-C cancelled the TES tasks, and a task whose image was missing failed as a failure of the machine, twice retried. A test with TESK is still to come.

## 4. SLURM

```sh
iunctus run main.nf -executor slurm -slurm-container apptainer -slurm-options '--account=lab'
```

On a cluster, the config file can say all of this once, so that `iunctus run main.nf` runs there with no options ([the config file](storage.md), section 1; `demo/bcf-config.toml` is an example):

```toml
[executor]
default = "slurm"

[executor.slurm]
container = "apptainer"
options = ["--account=lab"]

[cache]
folder = "/shared/project/iunctus-cache"
```

Each attempt becomes one batch job (`slurm-executor`). The backend places the inputs in the scratch directory, writes the wrapper and the job script into its `control` folder, and submits the job with `sbatch`. The job runs the wrapper in the task's container on a node and then records the exit code of the container in `control/container-exit`. So the nodes must see the store and the scratch directories at the same paths. A shared file system like CephFS may show a file that another machine has just written as empty or short for a while; on the BCF cluster, while other jobs loaded CephFS, a node sometimes ran an empty wrapper, so the task did nothing and the job still ended well. So the backend syncs the files of the attempt before it submits the job; the job waits until the node sees the whole wrapper (60 s at most, else it fails with a note in its log), and syncs what the task wrote before it records the end; and when the container ended well, the backend waits up to 30 s for the record of the task. The defaults, `store` and `scratch/slurm` in the cache directory (`$XDG_CACHE_HOME/iunctus`, else `~/.cache/iunctus`), are in the home folder, which clusters usually share. Inputs are copied into the store first.

The options of `sbatch` come from the task: `--cpus-per-task`, `--mem`, `--time`, `--gpus`, and `--partition` from the queue of the task. `-slurm-options` adds more, like `--account=lab --qos=low`; they come last and so win. The output of a job goes to `control/slurm.log`.

`-slurm-container` says how the nodes run containers:

| Runtime | How the job runs the task |
|---|---|
| `apptainer` (default), `singularity` | `apptainer exec --containall` with the scratch directory, the work directory and the mounts bound, and `--nv` for tasks with GPUs. An image pinned to its digest is pulled once into a SIF file in `<cache>/scratch/slurm/images`, by a small job of its own (`iunctus-image-...`, 2 CPUs, 4 GB and one hour unless `[executor.slurm.pull]` of the config file says otherwise) before the first task that needs it; every job then runs that file. Jobs that each pull the same image at once spoil Apptainer's shared cache. A SIF file at an `http://` or `https://` address, like those that nf-core modules name for Apptainer, is pulled once the same way. Nextflow workflows see `apptainer` (or the runtime that `-slurm-container` names) in `workflow.containerEngine`, as in Nextflow, so that their modules choose the images they name for it. An image named by a tag, like `debian:stable-slim`, is pulled the same way into `tag-<digest of its name>.sif`, but once per run, since the tag may move. A path to a `.sif` file stays as it is. The tasks of an image share its pull and what came of it: a failed pull is tried again twice, after 20 and 40 s, and then every task of the image fails with the same error, which the engine does not retry. The container runs inside the job, so SLURM limits and stops it. |
| `podman` | `podman run --init` as the user (`--userns=keep-id`), with the same limits as the job, without capabilities and with `no-new-privileges` (section 6). The init process hands the signal that stops the job to the task, so the container ends with the job. |
| `docker` | as Podman, as the user with `--user`. |

The SLURM executor uses the tools that every SLURM node has, `sbatch`, `squeue`, `sacct` and `scancel`, so it needs no config (decision 26). A run on a machine without `sbatch`, `squeue` or `scancel` on the PATH stops before its first task, with exit code 3 and the advice to run iunctus on a node of the cluster. The backend asks `squeue` about all jobs of the run at once, at most every 10 seconds, so that many tasks do not load the scheduler. It reads `squeue --json` and `sacct --json` where SLURM writes JSON (since 21.08; the state is a word before 23.02 and a list of words since), and their text where it does not. `squeue --states=all` also shows jobs that ended lately; for a job that SLURM has forgotten, `sacct` tells how it ended, when the cluster keeps accounting. How a job ends decides the outcome:

- The job recorded the exit code of its container, and the wrapper recorded the exit code of the tool: that is the exit code of the task.
- A container stopped by a signal, like one killed for lack of memory, ends the task with its own exit code and a note on standard error.
- `TIMEOUT` ends the task with 143 and `OUT_OF_MEMORY` with 137, each with a note, as a stopped container would. SLURM counts the time of a job in whole minutes, so the job script also stops the container at the task's time to the second (`timeout`, then a kill after 10 seconds), which ends the task with 143 as well.
- Every other end is a failure of the machine, which the engine retries: `NODE_FAIL`, `PREEMPTED`, `BOOT_FAIL`, a job cancelled by someone else, a container runtime that could not start the image, or a job that left no record. The error quotes the end of the job's log. A job that left no log at all usually means that its node does not see the scratch directory; the backend then asks SLURM for that node and leaves it out of the later jobs of the run (`--exclude`), so that the retries go elsewhere.

A cancelled run cancels its jobs with `scancel` and waits until they leave the queue.

The command of a task in an image of a registry runs through the entrypoint of the image, or the image's default command runs when the task has none, as with the other executors; the backend reads both from the registry, and keeps those of an image pinned to its digest in a file next to its SIF file, so that it asks for each image once: Docker Hub counts each request for a manifest against a limit of 100 per hour, which all nodes of a cluster may share. A SIF file or an address runs the command as it is, as `apptainer exec` does.

A task of several containers is one job, whose script starts a container for each image at once, waits for all of them, and records the exit code of the first container that failed, else 0 (section 1).

The tests run the backend against a simulated SLURM on this machine (`slurm-executor/tests/simulated_slurm.rs`): shell scripts in place of `sbatch`, `squeue`, `scancel` and `sacct`, and a simulated Apptainer that runs the wrapper without a container. One test runs a real container with Docker.

### Tested on a real cluster (2026-10-01)

iunctus ran on a SLURM cluster of a university: SLURM 24.11.5, Debian 13 (glibc 2.41), Apptainer 1.3.6, and a CephFS file system that all nodes mount at the same path. The compute nodes reach the internet directly; the login node only through a web proxy, and it must do no work.

- The binary is one static file, built with `cargo build --release --target x86_64-unknown-linux-musl -p iunctus` on any Linux machine. It needs nothing on the cluster.
- The nodes may call `sbatch`, so iunctus itself runs as a small job (2 CPUs, 4 GB), and its tasks become jobs of their own. The cache folder (`XDG_CACHE_HOME`) is on the shared file system.
- `iunctus check-machine -executor slurm` is ready there; the container engine is `not set up`, which SLURM runs do not need.
- Real runs with `-executor slurm` and Apptainer: a small Nextflow chain, a Snakemake workflow, two Argo examples, and these, each against what the native tool or the test expects:
  - CWL: 280 of the 287 conformance tests v1.2 give the output that they expect (`iunctus/tests/fixtures/real_runs_slurm_cwl.txt`); 7 are left out, as they name images that the run does not pull or need the network. The tools that do not ask for the network run without it.
  - Snakemake: a sample of 60 of Snakemake's own tests (every fifth test that needs no Conda, remote files or other images; `real_runs_slurm_snakemake.txt`): 41 give their expected results, 3 fail as the test expects, and 14 fail or miss an expected file the same way as Snakemake 9.27 does on a machine with Docker (`real_runs_snakemake.txt`). Of the other two, test05 asks for a GPU, which the nodes do not have, and test_access_patterns reads where its input links point (known, on every executor).
  - Argo: 58 of a sample of 59 workflows from the Argo test workflows end as Argo's tests expect (`real_runs_slurm_argo.txt`); the other asks for more memory than any node has, on purpose, and `sbatch` refuses it.
  - nf-core with `-profile test,apptainer` against Nextflow 26.04.6 on the same cluster (`real_runs.txt`, cases `*-slurm`; two native runs, files that differ between them count by name): demo (41 files with the same digest, 34 by name; 41 s against 50 s), fastqrepair (32 and 38; 70 s against 60 s), datasync (21 and 11; 59 s against 45 s), phyloplace (16 and 11, and 5 files with the same lines in another order, as epa-ng writes its placements in the order its threads end; 60 s against 78 to 84 s) and isoseq (165 and 76, and MultiQC's table of lima filters with its columns in another order; 1217 s against 825 to 1053 s). Each also differs in `pipeline_dag`, the graph page of iunctus. nf-core/bamtofastq at daf99df fails its own test profile in Nextflow too, and iunctus fails at the same task.
- What the cluster showed, and what changed for it: jobs that each turned the same image into a SIF file spoiled Apptainer's cache (now one pull job per image); a node that did not see the shared file system failed every job (now left out of the later jobs of the run); SQLite's WAL mode does not work across nodes, and each commit is slow on CephFS (now the rollback journal with a longer wait there, and the sources of a run in one commit); SLURM counts time in minutes (now `timeout` in the job); tasks that read the same remote file at once spoiled its download (now one download); a node sometimes ran an empty wrapper while other jobs loaded CephFS (now synced files, and a job that waits for the whole wrapper); a failed pull was tried again by every task of the image (now one pull per run, tried three times). Later: Argo steps of images with an entrypoint, like `argoproj/argosay`, ran without it (now through the entrypoint, as on the other executors); tasks without network had it under Apptainer (now `--net --network none`, which Apptainer 1.4.5 allows without setuid); asking each run for the configs of its images used up Docker Hub's 100 requests per hour of the cluster's address (now kept per pinned image); Nextflow's `collectFile` with `newLine` and a seed, an output `path("./.")` (the work directory), and CWL file literals in the output object differed from the native tools (fixed for all executors).


## 5. Kubernetes

```sh
iunctus run main.nf -executor kubernetes -kubeconfig kind.kubeconfig [-kubernetes-namespace <name>]
```

Each attempt becomes one Kubernetes Job with one pod (`kubernetes-executor`). The backend places the inputs in the scratch directory and writes the wrapper, then creates the Job through the Kubernetes API, with its own client (kube-rs), so no `kubectl` is needed. The pod runs the wrapper in the task's image as the user who owns the scratch directory (an Argo task as its image says, section 6), without capabilities and without a way to gain privileges, with the scratch directory, the work directory and the other mounts as `hostPath` volumes at the same paths. So the nodes must see the store and the scratch directories (`scratch/kubernetes` in the cache directory) at the same paths: one node that mounts them, like `kind`, or nodes with a shared file system. Inputs are copied into the store first.

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

## 6. Users and permissions (decision 28)

Every container gets the least permissions that let its task run, and so do the containers that answer lines, like the Python process of decision 19 (`executor::process`):

| Setting | Local (Docker, Podman) | Kubernetes | SLURM with Podman or Docker | SLURM with Apptainer | TES |
|---|---|---|---|---|---|
| Linux capabilities | none (`cap_drop: ALL`) | none (`capabilities.drop: [ALL]`) | none (`--cap-drop ALL`) | none: the user's own | as the server sets them |
| Gaining privileges | off (`no-new-privileges`) | off (`allowPrivilegeEscalation: false`) | off (`--security-opt no-new-privileges`) | off: Apptainer sets `no_new_privs` | as the server sets it |
| Privileged mode, host PID and IPC | never | never (`privileged: false`) | never | never (`--containall`) | as the server sets them |
| Network | only when the run allows it | the pod network | only when the run allows it | only when the run allows it (`--net --network none`) | as the server sets it |

The only capability that a container ever keeps is `CHOWN`, and only an Argo container that runs as root on an engine with root: its wrapper needs it to give the files that the task made back to the user who runs iunctus. The Argo test workflows and the other test workflows ran with these settings; none needed more (see `real_runs_argo.txt`). A container has no `DAC_OVERRIDE`, so root in a container may write only where its user may: in the folders of the image that root owns, in `/tmp`, and in the folders of the task.

**Who a container runs as.** A task of Nextflow, Snakemake or CWL runs as the user who runs iunctus, in its work directory, as before. An Argo container runs as Kubernetes runs it (`TaskSpec::run_as`): as the user of its image, or the `runAsUser` and `runAsGroup` of its `securityContext` (the container's, else the template's, else the workflow's); in the working directory of its image, or its `workingDir`, which is then only where it starts; with the `HOME` of its user; with a file system that it may write. The work directory of the task stays in its scratch directory. Other fields of a `securityContext`, like `privileged` or `capabilities`, are left out with a warning, so a workflow never gets more permissions than these.

**Files belong to the user.** When a container may run as another user than the one who runs iunctus, the folders of its scratch directory are open to every user (mode 0777) while it runs, and the folder of all scratch directories is private to the user (mode 0700), so that no other user of the machine reaches them. At its end:

- a container that runs as root gives what the task made back to the user (`chown`, with `CHOWN`);
- a container of another user lets every user change what it made; the local executor then gives the files back with a short container of the task's image that runs only `chown`, as root with only `CHOWN` and `DAC_READ_SEARCH`, without network;
- so outputs reach the store as files of the user, and the scratch directory can be removed.

**Without root.** An engine without root, like rootless Podman or rootless Docker, maps root of the container to the user who runs iunctus, so tasks of the user run as root inside (`0:0`), and an Argo container whose image runs as root needs no capability and no hand-back. A container of another user of the image gets an id of the user's subordinate range; its files are handed back to root of the container, which is the user. Apptainer always runs as the user who calls it, without setuid or fakeroot. What Apptainer cannot copy:

- An Argo task starts in its `workingDir`, else in `/`: the working directory of the image is not known in a job, and `/` is that of most images.
- Its file system is writable in memory (`--writable-tmpfs`), which Apptainer limits to 64 MiB by default (`sessiondir max size` in `apptainer.conf`).
- It runs as the calling user, not as root or the `runAsUser` of the workflow. Where Apptainer runs without setuid, in a user namespace, it shows the files of the image as the user's own (`fuse-overlayfs`), so a step may write anywhere in the image, like `/result.txt`, as root may in Kubernetes. Where Apptainer is installed with setuid, the files of the image keep root as their owner, and the step may write only where the user may: `/tmp`, its work directory and its folders.

On SLURM with Podman or Docker, the user of an image is not known in a job, so it is taken as root; a container of another user then only lets every user change its files.

TES servers decide the user and the permissions of their containers themselves; iunctus cannot set them, and the wrapper lets every user change the files of the task, as before.

**Tested.** Docker as root on this machine: `executor/tests/local.rs` checks that a container of the user has no capabilities and `NoNewPrivs`, that root of an Argo container has only `CHOWN`, that a step writes `/result.txt` and `/data` as root in `/` with `HOME=/root`, that a step of user 4242 starts in its `workingDir` and its files come back to the user, and that the containers of a set do the same. Rootless Podman and rootless Docker are not set up on this machine; the settings that iunctus sends for them are tested as data (`executor::access`, `slurm-executor` script tests). Apptainer 1.4.5 without setuid on the BCF cluster, in a SLURM job (`-slurm-container apptainer`): an Argo step of `alpine` ran as the user (`Uid` of the user, `CapEff` and `CapPrm` 0, `NoNewPrivs` 1), started in `/`, and wrote `/tmp` and `/result.txt`; its artifacts and the scratch folder belong to the user. Rootless Podman is not installed on the nodes of that cluster, so it is not tested.

Aruna integration uses the generic TES executor plus S3 storage; TES-over-S3 staging is still being implemented.

## Construction details

The default simulator writes one TES task document per task, keeps files in cache scratch and synthesizes outputs. `local` runs containers, `tes` submits to a TES server, `slurm` submits container batch jobs and `kubernetes` creates Jobs. Disabling reuse makes real executors run every task regardless of cached results, including task replay.

Nextflow's `workflow.containerEngine` and engine config scopes name the runtime that runs tasks. SLURM uses its selected job runtime, such as Apptainer. Local execution reports Docker or Podman; a socket path naming Podman selects Podman. Remote inputs download once into the cache. Local digests remain cached while files are unchanged; importing inputs also copies local bytes into the store for remote executors.
