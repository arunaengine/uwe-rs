<!-- Supported execution behavior, qualification gates and operational guidance. -->
<!-- Copyright (c) 2026 The iunctus Contributors -->
<!-- SPDX-License-Identifier: MIT or Apache-2.0 -->

# Support and qualification

This matrix describes the 0.1.0 development line as of 2026-10-03. It is a capability boundary, not a production qualification certificate. Qualification must identify the exact source revision, binary digest, workflow/input identities, container digests, platform, backend configuration and observed results. Historical corpus counts in the [roadmap](roadmap.md) are separate evidence.

## Workflow languages

All four languages use the shared task graph and runner. Simulation produces synthetic files and can exercise planning; it does not establish tool output correctness, resource behavior or full compatibility with a native engine.

| Language | Implemented planning and execution subset | Deliberate differences and limits | Qualification still needed |
| --- | --- | --- | --- |
| Nextflow | DSL2 processes, workflows, named workflow entry selection with `-entry`, channels and supported operators including `subscribe(onError:)`; container scripts, declared outputs and delivery | Native Rust Groovy subset, no JVM; DSL1 and class inheritance unsupported; only implemented plugins are available. Active unsupported process directives fail, including `storeDir`, `maxForks`, `containerOptions`, `clusterOptions`, `queue`, `module`, `arch`, `stageOutMode` other than `copy`, `secret` and `executor`. This applies to effective config settings too: the iunctus backend is selected separately, so executor directives are refused even when they name another executor. Reads of `secrets` values are unsupported in workflows and config. `publishDir` refuses `move` because outputs must remain in the content store, `failOnError: false`, and active `contentType` or `tags` options. `collectFile` sorting supports natural, index, none and closure keys; default hash and deep sorting are unsupported | Full real pipeline results against pinned Nextflow, including dynamic channels, retries, failure hooks and delivery |
| Snakemake | Rules, wildcards, supported checkpoints, `-until`/`-omit-from` target selection, `-forcerun`/`-forceall` reruns that bypass cache and resume reuse, container shell tasks, native Python lifecycle hooks, isolated `shadow: "minimal"` tasks and supported embedded code | Native Python subset with container package helpers; arbitrary native Python objects cannot cross the helper boundary; lifecycle hooks receive a log path with hook messages and failure diagnostics, while shell/subprocess commands in hooks are refused; minimal shadow stages declared inputs and task code without extra workflow sources, and refuses relative paths escaping the workdir; other shadow modes are unsupported; `cache: True` and `"omit-software"` use the native task cache, whose keys always include software identity; profiles support config, resource/thread overrides and `retries`/`restart-times`, ignore display settings and settings that only choose how jobs run (executor, cores, jobs, Conda or container use, cluster commands, rerun triggers), and refuse other settings with code 33; bounded same-image `service()` groups include internal dependency ordering and group retries; real FIFO `pipe()` work is refused before task publication | Real outputs against pinned Snakemake, package/environment identity and supported checkpoint behavior |
| CWL | CommandLineTool and workflows; scatter methods, `when`, `valueFrom`, `pickValue`, file arrays with `loadContents`, secondary files and `outputEval`; resources, environment, initial workdir, tool time limits, network access and work reuse | Supported JavaScript subset; image build/load/import requirements, ontology format relationships beyond exact matches and in-place Directory updates unavailable. Required `Secrets`, `ShmSize` and `RuntimeConstraints` extensions fail with code 33; required CUDA settings support static device counts only. Invalid resource amounts and nonboolean policy or secondary-file expressions fail explicitly; hints may be ignored | Real conformance outputs, writable input modes, failure/output collection and directory fidelity |
| Argo | Supported steps/DAGs, conditions, loops, local artifacts and configured storage downloads, container/containerSet templates, child workflows, ConfigMap parameters, exit/lifecycle hooks, immediate retries and container/script/containerSet `activeDeadlineSeconds` without init containers | Translated into the shared runner. Known fields follow the [Argo v4.1.4 spec](https://github.com/argoproj/argo-workflows/blob/v4.1.4/pkg/apis/workflow/v1alpha1/workflow_types.go). `podGC`, workflow/artifact `artifactGC` and `volumeClaimGC` are no-ops because cluster cleanup does not apply. `memoize` is a no-op: tasks use the native content-addressed cache and may recompute instead of using a user-keyed entry. Retry backoff `duration`, `factor` and `cap`, containerSet retry `duration` and retry host affinity are ignored: retries run immediately on the configured executor without a node-diversity guarantee. CPU and memory limits take precedence over requests for both reservation and cap; requests are used when a limit is absent, and requests above limits are invalid. Local CPU amounts round up to whole cores. Disk and GPU requests and limits must still agree; unknown resource kinds are refused. Controls that can change outcomes fail with code 33: synchronization can protect shared external state; suspension gates execution; workflow `activeDeadlineSeconds`, template `timeout`/`pendingTimeout`, deadlines across init containers and retry `backoff.maxDuration` can change failures and retries. Container lifecycle hooks, probes, restart/resize policies and restart rules can run or stop commands; pod patches, pod resources and resource claims can change execution. These controls remain refused, as does transient-error retry classification. Workflow lifecycle/exit hooks remain supported; invalid hook/retry expressions fail instead of skipping work. Artifact repository selection, declared repository uploads, Git/HDFS/OSS/plugin transfers, inline transfer options (including explicit S3 endpoints), archive locations, artifact permissions/compression and event/supplied parameter sources are refused because they can change sources or outputs. Cluster resource templates other than supported claim/child-workflow operations, daemons, sidecars and data templates remain unavailable (decision 18) | Real loop/aggregation/parallelism and artifact outcomes against pinned reference cases |
| Plain host scripts | No separate workflow-language contract | No plain host task execution path; supply a supported workflow and container image | A separate language contract would require an explicit design decision |

Unsupported constructs should produce a diagnostic, typically exit code 33; parser acceptance alone is not a parity guarantee. See [checks](design/checks.md), [language decisions](decisions.md) and the language-specific runtime pages under [design](README.md#design). Workflow interpretation is native; task and package execution requires containers. Conda uses rattler metadata and match specs, resolvo solving, cached HTTPS acquisition or offline inputs, and prepared container images for local Snakemake `conda:` rules. Target capabilities must describe the pinned container base; pip requirements, remote environment sources and Conda image preparation on remote executors are unsupported.

## Executors and platforms

| Executor | Implemented behavior | Deployment prerequisites and limits | Qualification status |
| --- | --- | --- | --- |
| Simulator (default) | Plans and makes synthetic outputs without running task containers | No real task result, cancellation or ownership evidence | Planning/corpus checks only |
| Local Docker/Podman | Parallel container execution through the engine API | Reachable engine socket; usable images; explicit host inputs and writable paths operate under caller/engine permissions | Selected current rootful Docker cases passed; rootless Podman, broader mixed UID and platform qualification remain separate gates |
| TES | One executor per task; shared-path or S3 FILE staging; verified output import; tagged submission recovery; poll and cancel | Object-store mode needs a bucket accessible to engine and workers; directory capture needs task shells; stdin, symbolic-link inputs, several containers, offline tasks and GPUs are refused | Strict fake TES and memory S3 checks pass; historical shared-path Funnel evidence; current real TES/S3 and restart qualification outstanding |
| SLURM | Container batch tasks with Apptainer or Podman | Scheduler tools and shared store/scratch paths; backend-specific image, network and GPU limits | Simulated scheduler and historical cluster evidence; current real scheduler qualification outstanding |
| Kubernetes | Container Jobs through the Kubernetes API | Configured namespace and nodes with matching shared host paths; backend-specific network and GPU limits | Fake API checks; current live-cluster qualification outstanding |

TES services integrate through their native TES 1.1 endpoint and S3 interface. See [Using a TES + S3 service](design/executors.md#using-a-tes--s3-service); no server-specific backend is needed.

Linux x86_64 is the initial locally tested build platform. That host evidence does not establish production support. Other host architectures and macOS/Windows have no qualification claimed here. Container platform compatibility must be checked for each image and executor; a host build alone does not establish it. See [executor configuration and shared paths](design/executors.md) and [storage configuration](design/storage.md).

Real backend acceptance must cover exact output bytes, missing/failed outputs, cancellation and timeout, cleanup after interruption, restart ownership, rootless/mixed UID access, resource limits, image platform selection and supported storage configurations. Ordinary green Rust tests and fake services do not replace these checks. Corpus gates must use complete pinned inputs and oracle results. Ignored container and corpus tests are external gates until explicitly invoked with their prerequisites.

## Installation and operation

Use the [root installation instructions](../README.md#build-and-install-from-source). `iunctus help` lists options; `iunctus check-machine` checks executor prerequisites and `iunctus check <workflow>` diagnoses supported planning constraints. These checks do not run tools or prove successful real results. A task without a workflow image requires `-default-image`; host-installed packages are not a fallback.

The config defaults to `~/.config/iunctus/config.toml`. The cache defaults to `$XDG_CACHE_HOME/iunctus`, else `~/.cache/iunctus`; `runs.sqlite` is stored there. Preserve the actual configured store, scratch and download paths when moving between machines or remote backends. The local run page binds to `127.0.0.1` by default; `-address` is an explicit choice to make its run data reachable elsewhere. See [runs and page options](design/runs.md).

Preview retention with `iunctus clean` before requesting deletion with `-remove`. Resolve retained remote ownership records before removing associated scratch. Do not manually erase unresolved attempt journals: they preserve information needed to determine whether submitted work is still running.

CWL JavaScript expressions require the versioned native helper image `iunctus-cwl-js:0.1.0`, including simulated runs. After building the trusted Linux `iunctus` binary, run `python3 scripts/cwl-js-image.py target/release/iunctus --engine docker` (use `--engine podman` for Podman). The script builds a scratch image from that exact binary, its `ldd`-resolved loader and libraries, and a writable `/tmp`; it does not pull a base image or install Node.js. Only use this script with a trusted executable because `ldd` invokes its loader. The image contains `/usr/share/iunctus/cwl-js.sha256` with binary and library identities. Qualify it on the binary's native Linux architecture before use elsewhere. CLI evaluation uses the configured local engine and run cancellation token. Library callers configure `JavaScriptSettings`; `IUNCTUS_CWL_JS_IMAGE` selects a replacement image, preferably by digest.

Candidate artifacts include a helper image archive. Verify the artifact checksums, then run `docker image load --input iunctus-cwl-js-0.1.0-linux-amd64.tar.gz` (or the Podman equivalent). The helper runs the same native Boa implementation through an internal command of `iunctus`; it is required separately from workflow task images. Evaluation has a 20-second operation budget, 256 MiB memory ceiling with no swap, 16 MiB message cap and nesting limit of 64; forced container removal has a separate grace of up to ten seconds. Worker entry verifies effective container limits before reading code. These limits do not establish full JavaScript parity or qualify rootless and remote backends.

## Upgrade, backup and recovery

Before upgrading, stop admitting new work, finish or explicitly reconcile active remote work, and stop all local writers. Record the existing binary/source digest and configuration. Back up the run database together with its WAL state using a SQLite-consistent backup, or copy it after all writers have stopped and the WAL has been checkpointed. Also preserve the configured store, retained scratch/journals, workflow/input snapshots and referenced image digests. A database-only backup cannot restore missing output bytes.

Database opening applies forward schema migrations and refuses newer schema versions. There is no downgrade migration or cross-version rollback guarantee. Test an upgrade on copies first. Rollback requires the previous binary and matching pre-upgrade database/store/configuration copies; replacing only the executable can leave incompatible state. Reconcile remote ownership before resuming restored state so live jobs are not duplicated.

Resume and repeat are implemented, but exact historical computation depends on retained input bytes, images and configuration. Retention, corruption recovery, multi-host filesystem behavior, restoration and browser qualification remain explicit acceptance work. The [run design](design/runs.md) and [reproducibility design](design/reproducibility.md) describe the mechanisms; they do not certify a completed recovery exercise.

## CI and candidate artifacts

[Source checks](../.github/workflows/source.yml) run on pushes and pull requests: pinned nightly formatting/Clippy and pinned stable workspace tests/doctests. Dependency fetching is an explicit online preparation step; checks use `--offline --locked`. This workflow has not yet been executed remotely or confirmed green. Repository settings must separately require its four checks before merge.

[Explicit qualification](../.github/workflows/qualification.yml) requires a corpus/oracle tar.gz URL and its exact SHA-256. The archive root must contain `nf-core-text/pipelines`, `snakemake`, `config-oracle` and `manifest.json`. The manifest uses `format: 1`, `nextflow_version: "26.04.6"`, `fixtures` mapping the three fixture paths listed in [the verifier](../.github/scripts/verify-corpus.py) to their SHA-256 digests, and `files` mapping every other bundle-relative file path to its SHA-256. Record oracle outputs with that reference version and pinned workflow revisions; include `.exit`, `.txt` and `.err` for every pipeline. The verifier checks fixture identities, every file digest and the exact pipeline inventory. Existing tests then enforce config parity and the exact Conda inventory/rejections. Missing resources fail this invocation; no public complete bundle is claimed to be available yet.

Qualification also explicitly invokes the real Docker Python package bridge, requiring a working engine and network for image/package preparation. It is one integration gate, not qualification of rootless or remote backends or the other ignored language corpora. [Candidate artifacts](../.github/workflows/artifacts.yml) is manually invoked with the same bundle identity, depends on those source/corpus/container checks, and produces a Linux x86_64 binary archive, license texts, documentation and SHA-256 checksums with source/toolchain identities. It uploads CI artifacts only; publication and production acceptance remain separate decisions. Verify `SHA256SUMS` before extracting and `BINARY.sha256` inside the extracted folder. Checksums identify bytes; they do not imply an independent signature, complete dependency license/advisory audit or backend qualification.

## Command and harness details

The following descriptions retain the detailed contracts and invocation notes from the crate source introductions.

### `iunctus/src/changes.rs`

What changed since a run: the files of its workflow and of its command line, the files it read on this machine and at addresses, and the images that its tags name now, each compared by digest with what the run recorded. `iunctus changes <run>` prints it, and `iunctus repeat <run>` prints it before it runs the run again with what the run pinned.

### `iunctus/src/check.rs`

`iunctus check <workflow>`: everything that would stop a real run, found at once and without running a task, in every language: unsupported features, syntax and load errors, broken references like missing includes, inputs that do not fit the schema of the workflow, local files that tasks read and that do not exist, and tasks without a container image.

The check plans the run with the simulator, in a scratch folder that it removes, so that it goes past the missing files that would stop a real run at the first one. It records nothing in the run database.

### `iunctus/src/clean.rs`

`iunctus clean`: removes from the store what no rule keeps, as `nextflow clean` removes work folders, but by rule and never what a run still refers to. It lists what it would remove first, and removes it only with `-remove`. It also removes the scratch folders that runs which ended left behind.

### `iunctus/src/failure.rs`

Why a command failed, and its exit code. Every error has one form, whichever part of iunctus finds it: `iunctus: <where>: <what went wrong>; <what to do>`. The place is a file and line, a task, an input or an address. docs/design/checks.md lists the exit codes.

### `iunctus/src/fetched.rs`

The remote files that a run read, as the storage providers noted them: task inputs at addresses, and files that workflow code read, like a sample sheet on the web. The run database records them by digest, and a real run keeps their contents in its store, so that a repeat of the run reads the same bytes, also when the file at the address has changed.

### `iunctus/src/inputs.rs`

The inputs and outputs of a workflow as JSON Schema (decision 24): `iunctus inputs` prints the schema of the inputs of any workflow, `iunctus outputs` that of its outputs, and every run checks its values against the schema of its inputs before any task starts.

### `iunctus/src/kubernetes.rs`

The Kubernetes executor: each task runs as a Kubernetes Job through the task runner. The nodes must see the store and the scratch directories at the same paths, as `hostPath` volumes. The cluster is the one of the kubeconfig that `-kubeconfig` names: iunctus never takes the default kubeconfig, so that no run reaches a cluster by accident.

### `iunctus/src/order.rs`

The order check, `iunctus check-order <workflow>`: runs a workflow twice, once with the results of its tasks handed to the workflow in the order it asked for them and once in the reverse order, and compares the steps of the two runs by the digests of what their tasks read and made. A step whose tasks or outputs change with the order depends on which task finished first, like a step that collects files in the order their tasks finish.

### `iunctus/src/page.rs`

The web page of runs (decision 23): `iunctus page [<run>]` serves it for any run, and `iunctus run -page` serves it for the run it starts. The page listens on this machine only, unless `-address` says otherwise.

### `iunctus/src/preview.rs`

The preview of a run: a simulated run that tells, for each task, whether a real run would take it from the cache, so that the user sees how much work is left before any compute is spent. A task whose result the store holds gets that result, with its real files, so that the tasks after it can be found in the cache too; every other task is simulated.

A real run pins each image to its digest, which is part of the cache key. The preview asks no registry: it takes the digest that the latest real run pinned the image to, from the run database. So a task whose image no real run used, or whose tag moved since, is simulated.

### `iunctus/src/progress.rs`

What `iunctus run` shows of a real run, as `nextflow run` shows its log: a line that names the run, then a line per step with its counts, and at the end how the run went and where its results went. In a terminal, the lines of the steps are drawn again in place as the run goes (`terminal.rs`); elsewhere, like in a log file, each task that ends prints its own line, like `[done] SAY (1)`, and the summary comes at the end. The same for every language.

### `iunctus/src/repeat.rs`

`iunctus repeat <run>`: runs a past run again, exactly: with the files of its workflow and of its command line as the run had them, its images at the digests it ran at, and its remote files as it read them, from its store. It first prints what changed since the run, and at its end compares the outputs of each step with those of the run by digest.

### `iunctus/src/report.rs`

The report of a run, one HTML file that needs no server (decision 23's page in a file): `iunctus report <run>` writes it for any run, and `iunctus run -report <file>`, or `file` of the `[report]` section of the config file, writes it when the run ends. It suits machines where no browser can reach the page, like the login node of a cluster: copy the file and open it.

### `iunctus/src/resources.rs`

Right-sized resources: what the tasks of each step used in earlier real runs of the workflow sets the CPUs and memory that the next run asks for, and a note after the run says what changed (see `docs/design/efficiency.md`).

### `iunctus/src/resume.rs`

Resume from the run database: a run that continues an earlier one takes the result of each task that finished there, instead of running it again. Real results additionally need their recorded input and output bytes and pinned image identity. Simulated results have no such identities to prove. The store's cache still answers tasks whose image is pinned, in any run.

### `iunctus/src/runs.rs`

The commands that read the run database: `iunctus runs` lists the runs, `iunctus export <run>` writes the Workflow Run RO-Crate of a run, and `iunctus why <run> <task>` tells why a task ran instead of taking an earlier result, and `iunctus origin <file>` tells where a file came from.

### `iunctus/src/setup.rs`

The config file of iunctus (decision 20): the site-wide file, the user's file and the file of `-iunctus-config`, read once per command, with the storage providers and the store that they name. Without any file, everything works with its defaults.

### `iunctus/src/slurm.rs`

The SLURM executor: each task runs as a SLURM batch job through the task runner. The nodes must see the store and the scratch directories at the same paths, through a shared file system, as the cache directory in a home folder usually is.

### `iunctus/src/sources.rs`

The sources of a run: the files of its workflow and the files that its command line names, like config files, params files and CWL job files, each by digest. A real run keeps their contents in its store, so that a repeat of the run has the same workflow and the same config years later, also when the folder of the workflow has changed since.

### `iunctus/src/terminal.rs`

The live drawing of a run in a terminal, as `nextflow run` draws its log: the lines of the steps stay at the bottom and are drawn again in place, at each event and every second. What else the run prints, like Nextflow's `view` or a warning, goes above them: while the drawing lasts, standard output (and standard error, when it is the same terminal) go into pipes, whose lines a thread each prints above the steps.

### `iunctus/src/tes.rs`

The TES executor runs each task through one GA4GH TES executor. Shared-path mode uses matching local paths and `file://` URLs. Object-store mode transfers FILE inputs and outputs through S3, with directory layout preserved by shell capture and a manifest, so workers need no shared filesystem.

### `iunctus/src/tracked.rs`

The files that Git tracks in a repository, read from its index (`.git/index`) without a Git program, so that a run can record the files of its workflow by digest. The index of Git versions 2, 3 and 4 is read; the paths of version 4 are compressed against the path before.

### `iunctus/src/insight.rs`

The commands that show what runs did and why, from the run database: `iunctus time` tells where the time of a run went, `iunctus compare` what differs between two runs, and `iunctus replay` runs one task again and tells what differs, and `iunctus telemetry` writes and sends the trace of a run in the OpenTelemetry format. `time` and `compare` write plain text, or JSON with `-format json`.

### `iunctus/src/replay.rs`

`iunctus replay <run> <task>`: runs one recorded task again, alone, from its description in the run database: in the image it ran in, pinned to its digest, with the same inputs, taken from the store by their digest where their files are gone. It runs on this machine, or on the executor that `-executor` names, whatever the cache holds, and is recorded as a run of its own. Then it tells how the replay compares with the task as it ran: its exit code, time and outputs by digest, and the standard output and error of both. A failed task replays too.

### `iunctus/src/telemetry.rs`

OpenTelemetry traces of runs: `iunctus telemetry <run>` writes the trace of a run as OTLP JSON lines to a file, and sends it to the OTLP HTTP endpoint of the `[telemetry]` section of the config file when there is one. Every real run sends its trace there when it ends.

```toml
[telemetry]
endpoint = "http://localhost:4318"
headers = { authorization = { environment = "OTLP_AUTHORIZATION" } }
```

### `iunctus/src/time.rs`

`iunctus time <run>`: where the time of a run went. The critical path, the chain of tasks that set the length of the run, with how long each waited and ran; how long all tasks ran and waited together; and the steps that ran the longest and used the most CPU time, memory and I/O.

### `iunctus/src/machine.rs`

`iunctus check-machine`: what is ready on this machine for real runs, and what is missing with what to install or set: the config file and its secrets, the container engine, the store and its free space, the run database, and each executor that the config file or the command line sets up.

The check asks no server outside this machine, except those that the config file or the command line names, each once: the store at an address, and the TES server.

### `iunctus/tests/cache.rs`

The global cache with real runs in Docker: a task that any run, pipeline or language asked for before comes from the store, and two runs that ask for the same task at the same moment run it once. The tests need Docker and the `debian:stable-slim` image; run them with `cargo test -p iunctus --test cache -- --ignored --test-threads 1`.

### `iunctus/tests/config_files.rs`

Which config files the binary reads: the file that `IUNCTUS_SITE_CONFIG` names in place of the site-wide file, and none at all for the commands of the other tests, which run in an empty config folder.

### `iunctus/tests/config_test_workflows.rs`

Compares `iunctus config -profile test` with `nextflow config -flat -profile test` for every nf-core pipeline of the test workflows. `just config-oracle` records the Nextflow output first; run with `just config-test-workflows-test`.

### `iunctus/tests/efficiency.rs`

Right-sized resources and short tasks together in real runs: a run after an earlier one asks for the memory that its tasks used, not for what the workflow names, and says so; `-no-right-sizing` keeps what the workflow names. Short tasks run one after another in one container, each with its own outcome, log and outputs. `cargo test -p iunctus --test efficiency -- --ignored` (needs Docker and the debian:stable-slim image).

### `iunctus/tests/errors.rs`

Checks that every error has the same form, `iunctus: <where>: <what>; <what to do>`, and the exit code of its kind: 1 for the workflow, 2 for the command line, 3 for the machine or its config, 33 for a feature that iunctus does not support. No test needs a container engine or the network: servers are at a local port that nothing listens on.

### `iunctus/tests/inputs_test_workflows.rs`

Describes the inputs of every Nextflow workflow of the test workflows, the nf-core pipelines and the Nextflow workflows of WorkflowHub, as a run does before its first task (decision 24), and compares the counts with `tests/fixtures/inputs_test_workflows.txt`: the inputs of each schema and how many are required, files and sample sheets, the errors and warnings of a check of a simulated run with `-profile test` (without a profile where a WorkflowHub workflow has none), and where the schema and the params that the code reads differ. Every schema must be valid JSON Schema of draft 2020-12. No task runs and nothing comes from the network: the driver reads local files only, and a remote file is an `IOException`, as Nextflow throws when it cannot reach one; nf-core pipelines leave out their remote config files, as with `NXF_OFFLINE`. Each workflow is described in a child process that starts in its folder, as a run does. Run with `just inputs-test-workflows-test`; `UPDATE_FIXTURE=1` writes the fixture again.

### `iunctus/tests/insight.rs`

The insight commands on recorded runs: `iunctus time`, `iunctus compare` and `iunctus telemetry` on simulated runs, with a small OTLP HTTP receiver of the test, and `iunctus replay`, which refuses simulated runs and, in the ignored test, runs a task of a real run again in its container: `cargo test -p iunctus --test insight -- --ignored` (needs Docker and the debian:stable-slim image).

### `iunctus/tests/machine.rs`

Runs `iunctus check-machine`: it says what is ready and what is missing for real runs, with what to do, and exits with 3 when something is missing. The container engine is a socket that is not there, and the only server is at a local port that nothing listens on.

### `iunctus/tests/reproducibility.rs`

Reproducibility in whole runs: what a run records to pin it, what changed since, the order check, and, with Docker, the reproducibility check of cached tasks and a repeat of a run whose workflow and remote file changed since. The Docker tests need the `debian:stable-slim` image; run them with `cargo test -p iunctus --test reproducibility -- --ignored --test-threads 1`. A repeat asks the registries whether the tags of its images moved; inside `unshare --map-current-user -n --keep-caps` (with `ip link set lo up`) it cannot, and says so.

### `iunctus/tests/run.rs`

Runs `iunctus run` on small scripts with the simulator, and with Docker in the ignored test, which needs the `debian:stable-slim` image; run it with `cargo test -p iunctus --test run -- --ignored`.

### `iunctus/tests/run_test_workflows.rs`

Runs every nf-core pipeline of the test workflows with `-profile test` and the simulator, and compares the outcome with `tests/fixtures/run_test_workflows.txt`: `ok`, or the exit code and the first line of the error. The Nextflow workflows of WorkflowHub run the same way, and compare with `tests/fixtures/run_workflowhub.txt`. The runs are stub runs, as pipelines then do not read the content of the outputs that the simulator makes up, and up to 8 tasks finish in a seeded shuffled order, so that the Nextflow driver cannot depend on the order of outcomes. Each run starts from a copy of the folder of its workflow, as a user runs a workflow after a clone. The execution graph of each run that succeeds must be valid. A workflow may only fail with a record in `tests/fixtures/run_evidence.txt` of why it cannot run, and a record is only kept while its workflow fails. Run with `just run-test-workflows-test`; on a difference the test prints the lines of the new fixture.

### `iunctus/tests/runs.rs`

Every `iunctus run` records itself in the run database: `iunctus runs` lists it, `-resume` continues it without running its finished tasks again, and `iunctus export` writes its RO-Crate. The runs use the simulator, so they need no container engine, except the ignored test of resume with real containers: `cargo test -p iunctus --test runs -- --ignored`.

### `iunctus/tests/storage.rs`

Storage providers of the config file in whole runs: a private web server whose token comes from an environment variable, and whose secret shows nowhere, not in the output, the run database, the RO-Crate, the files of the simulator or the cache. The ignored test reads the inputs of a real run from one S3 server, keeps the store on another, as `just storage-test` starts them, and publishes the results to a third provider, a folder.

### `iunctus/tests/stub_test_workflows.rs`

Runs nf-core pipelines of the test workflows with `-stub` and the simulator, and compares each task's script in the execution graph with the one Nextflow wrote, as `tests/fixtures/oracle/stub/record.sh` recorded them. Run with `just stub-test-workflows-test`.

### Corpus records and TES routing

Sends tasks to the site near their inputs: two Funnel servers on this machine, the second near one folder. Start them with `just funnel-server` and the same with `--Server.HTTPPort 18001 --Server.RPCPort 19091` and other BoltDB and work folders. It counts the tasks of each server, so run it with `--test-threads 1`.

The reason of each pipeline that a record names. A record is a block of lines between empty lines: the pipelines, then `= <kind>: <why>` with one plain sentence, then the evidence that Nextflow fails the same way: `$` the command, and `>` the lines of its output. The kind is `invalid` (the pipeline itself is broken), `missing` (it needs something that is not public or not in its repository), or `unsupported (decision N)` (iunctus leaves the feature out on purpose, as decision N of docs/decisions.md says). Panics on a record without a valid reason or without evidence, so that the record file stays complete.

The outcome of one run: `ok`, or the exit code and the first error line without the parts that differ between machines and runs. A run that takes more than ten minutes is stopped with exit code 124. A run that succeeds with an invalid graph is `graph` and the problem. `target` is the folder of the workflow, or its main script, and `project` its folder, which the run starts from; `profile` runs it with `-profile test`.

## Evidence harness invocation

### `iunctus/tests/fixtures/docker_no_pull.py`

A Docker socket in front of the real one that refuses to pull images: each request to `/images/create` gets 403, every other request goes through. The evidence scripts of real runs point iunctus at it with DOCKER_HOST, so that a run without network cannot make the Docker daemon, which has network, fetch an image.

Use it as a context manager: `with NoPull(folder) as socket: ...` gives the path of the socket.

### `iunctus/tests/fixtures/real_runs.py`

Runs workflows for real, with iunctus and with the native tool, and compares what each delivers: the names and the SHA-256 digests of the result files. It records the comparison of each case in real_runs.txt next to this script.

Run it from the repository root: iunctus/tests/fixtures/real_runs.py [case...] With cases, it runs only them and keeps the other records. It needs Docker, the release binary of iunctus (target/release/iunctus, or IUNCTUS), and the native tool of each case on the PATH: nextflow 26.04.6 for Nextflow, cwltool for CWL, snakemake (through uvx) for Snakemake. Argo has no native tool without a cluster: its cases compare with the files that the workflow documents. Each case runs in ~/.cache/iunctus/real-runs/<case> (REAL_RUNS).

The native tool runs twice, into two folders, so that the files whose content differs between two native runs, like reports with dates, are compared by their names only. Names with a date and time, like `params_2026-09-30_12-02-36.json`, are compared with the date and time replaced by `<time>`. The native runs are kept and not repeated; `NATIVE=1` runs them again. At most 4 containers run at a time: Nextflow gets `executor.cpus = 4`, iunctus `-max-pending 4`.

The cases that end in `-slurm` run on a SLURM cluster with Apptainer: run the script on a node that may submit jobs, not on a login node, with nextflow, java and apptainer on the PATH. Nextflow gets `queueSize = 4` and names its jobs `iunctus-native-...`, iunctus `-max-pending 4`.

### `iunctus/tests/fixtures/real_runs_argo.py`

Runs Argo workflows of the Argo test workflows for real with iunctus, and compares how each ends with what Argo expects of it. Argo has no native run without a Kubernetes cluster, so the reference is Argo's own test suite: the workflows of argo-workflows' `expectedfailures` must fail, every other workflow must succeed, as Argo's end-to-end tests run its examples. `argo lint --offline` (argo v4) checks each workflow first; a workflow that it rejects counts as failing in Argo. It records the result in real_runs_argo.txt next to this script.

Run it from the repository root: iunctus/tests/fixtures/real_runs_argo.py [workflow...] With workflows (paths as in the record), it runs only them and keeps the other records. It needs Docker, `argo`, the release binary of iunctus (target/release/iunctus, or IUNCTUS) and the Argo test workflows (ARGO_TEST_WORKFLOWS).

It runs only workflows that run with the simulator (argo-runtime's outcomes.txt) and whose images are all on this machine; the others are left out with the reason. The runs use no network: iunctus runs in a network namespace of its own, gives its containers none (`-no-network`), and talks to Docker through a socket that refuses pulls (docker_no_pull.py), since the Docker daemon has network; a workflow that needs an image that is not on this machine, like one of a template that it calls, fails. One workflow runs at a time, with at most 3 containers at once.

### `iunctus/tests/fixtures/real_runs_cwl.py`

Runs the CWL conformance tests (v1.2) for real, with iunctus and with cwltool, and compares their output objects: the values, and for each file and directory its class, basename, checksum, size, contents, secondary files and listing, except what the expected output of the test leaves open with `Any`, like random file names. It records the comparison in real_runs_cwl.txt next to this script.

Run it from the repository root: iunctus/tests/fixtures/real_runs_cwl.py [test id...] It needs Docker, cwltool, the release binary of iunctus (target/release/iunctus, or IUNCTUS), the CWL test workflows (CWL_TEST_WORKFLOWS) and their data (CWL_CONFORMANCE_DATA).

The runs use no network: both tools run in a network namespace of their own, a `docker` in front of the real one refuses to pull, and a test runs only when every image it names is on this machine already; tests that need the network are left out. Tools without an image run in debian:stable-slim with both. One test runs at a time, so at most one container at a time.

### `iunctus/tests/fixtures/real_runs_slurm_argo.py`

Runs Argo workflows for real on a SLURM cluster with `-executor slurm` and Apptainer, and checks that each ends as Argo's tests expect: real_runs_argo.txt next to this script says which workflows succeed and which fail in Argo (from argo lint and Argo's expected failures), and records those that ran the same way with iunctus on this machine. It records one line per workflow in real_runs_slurm_argo.txt next to this script (or RECORD).

Run it on a node of the cluster that may submit jobs, not on a login node: real_runs_slurm_argo.py <Argo test workflows folder> [workflow...] It needs the iunctus binary (IUNCTUS, else `iunctus` on the PATH). Without workflows it takes every workflow that ran the same way here and names only the images in IMAGES, so that the cluster pulls a few small images. Tasks get no network (`-no-network`), as in real_runs_argo.py. At most PARALLEL workflows run at a time, each with 3 jobs at once. The folder may hold only some files of each repository; each repository folder then needs a `.git` folder, so that iunctus finds the WorkflowTemplates of the other files there.

### `iunctus/tests/fixtures/real_runs_slurm_cwl.py`

Runs CWL conformance tests (v1.2) for real on a SLURM cluster with `-executor slurm` and Apptainer, and compares each output object with the output that the test expects, as cwltest does: only the keys that the expected output names count, `Any` matches everything, and a location counts by its last part. It records one line per test in real_runs_slurm_cwl.txt next to this script (or RECORD).

Run it on a node of the cluster that may submit jobs, not on a login node: real_runs_slurm_cwl.py <conformance folder> [test id...]   (all tests without ids) The folder holds the CWL test workflows with their data (`v1.2/conformance_tests.yaml`). It needs the iunctus binary (IUNCTUS, else `iunctus` on the PATH). Tools without an image run in debian:stable-slim. Each test runs without the cache (`-no-cache`), as cwltool does, so that a result of an earlier run never answers it. Tests that need the network or name other images are left out, so that the cluster pulls only a few small images. At most 4 tests run at a time (PARALLEL).

### `iunctus/tests/fixtures/real_runs_slurm_snakemake.py`

Runs the tests of Snakemake (the `tests/` folder of its repository) for real on a SLURM cluster with `-executor slurm` and Apptainer, and checks the files of each run against the `expected-results` folder of the test. It records one line per test in real_runs_slurm_snakemake.txt next to this script (or RECORD).

Run it on a node of the cluster that may submit jobs, not on a login node: real_runs_slurm_snakemake.py <tests folder> [test...] It needs the iunctus binary (IUNCTUS, else `iunctus` on the PATH), git, and next to it real_runs_snakemake.py (whose helpers it uses) and run_test_workflows_launches.txt (LAUNCHES), which says how each test runs. Jobs run in IMAGE, which has Bash and Python without extra packages, so that the cluster pulls no new image. Tests without expected results, tests that name other images or Python packages, and those that real_runs_snakemake.py leaves out, are left out with the reason. At most PARALLEL tests run at a time, each with 3 jobs at once. A test that fails, or misses its expected results, the same way in Snakemake 9.27 on a machine with Docker (real_runs_snakemake.txt next to this script) counts as the same.

### `iunctus/tests/fixtures/real_runs_snakemake.py`

Runs the tests of Snakemake (the `tests/` folder of its repository, from the Snakemake test workflows) for real, with iunctus and with Snakemake, and compares the files that each run made, changed or removed in the folder of the test, by name and SHA-256 digest. It also checks both runs against the `expected-results` folder of the test. It records the comparison in real_runs_snakemake.txt next to this script.

Run it from the repository root: iunctus/tests/fixtures/real_runs_snakemake.py [test...] With tests, it runs only them and keeps the other records. It needs Docker, Snakemake 9.27.0 (SNAKEMAKE, else `snakemake` on the PATH), the release binary of iunctus (target/release/iunctus, or IUNCTUS) and the Snakemake test workflows (SNAKEMAKE_TESTS).

Each test runs the way run_test_workflows_launches.txt says (targets, config files and values), with 3 jobs at once, as Snakemake's own tests do, in a git repository of its files, as the tests are in Snakemake's repository. Snakemake gets the CPUs of the machine as cores, as iunctus sees them, where Snakemake's tests give 3. Snakemake runs the jobs on this machine, iunctus in containers of IMAGE, which has Bash, Python with pandas, and the usual tools. Both run in the same folder, one after the other, so that paths in outputs agree.

The runs use no network: both tools run in a network namespace of their own, and iunctus gives its containers none (`-no-network`). Tests that read remote files, need Conda environments, wrappers or modules, or name an image that is not on this machine, are left out with the reason. iunctus talks to Docker through a socket that refuses pulls (docker_no_pull.py), since the Docker daemon has network. When the runs differ, Snakemake runs once more, and files that differ between its two runs count by name only. Snakemake's own files (`.snakemake`, the `.snakemake_timestamp` of directory outputs, Python's `__pycache__`) are left out of the comparison.

### `iunctus/tests/fixtures/run_evidence.py`

Records how Nextflow 26.04.6 fails on the nf-core pipelines that fail in run_test_workflows.txt and on the WorkflowHub workflows that fail in run_workflowhub.txt, into run_evidence.txt next to this script. The run test checks that every pipeline that fails there is listed here, so a pipeline may only fail when Nextflow fails on it too, or when iunctus leaves the feature out on purpose.

Run it from the repository root: iunctus/tests/fixtures/run_evidence.py [pipeline...] With pipelines, it records only their cases and keeps the other records. It reads the pipelines of `just test-workflows` (NF_CORE_TEST_WORKFLOWS) and the workflows of `just workflowhub-test-workflows` (WORKFLOWHUB_TEST_WORKFLOWS), and needs `nextflow` 26.04.6 on the PATH. Each pipeline runs in a copy, from the folder of the copy, as the run test runs it: `nextflow run <pipeline> -profile test -stub-run --outdir <out>`. The machine has none of the tools of the pipelines, so a shell function stands in for each missing command: it prints `1.0`, which stubs take for a version, and succeeds. `SHOW=1` prints the whole output of each run.

### `iunctus/tests/fixtures/oracle/stub/record.sh`

Records the stub scripts of nf-core pipelines with Nextflow 26.04.6 and Docker: `sh record.sh <test-workflows>/pipelines <pipeline>...`. Each task's `.command.sh`, up to the capture block of `eval` outputs, goes to `<pipeline>/<task name>.sh`, with `:` and spaces in the name replaced by `_`.
