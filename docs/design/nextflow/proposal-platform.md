<!-- Design of the native Nextflow evaluator and runtime, built as a front end on the task layer that all languages share. -->
<!-- Copyright (c) 2026 The iunctus Contributors -->
<!-- SPDX-License-Identifier: MIT or Apache-2.0 -->

# Running Nextflow on a shared task layer

> Input to the chosen [Nextflow runtime design](runtime.md), which takes precedence where the two differ. Written before the decisions to start with TES and to leave Aruna for later.

Status: proposal, 2026-09-27.

This design starts from the execution platform. It first defines the task layer that Nextflow, Snakemake, CWL, Argo Workflows and plain script workflows will all use. It then describes the Nextflow evaluator and dataflow runtime as a front end that plugs into that layer.

It builds on the [decisions](../../decisions.md), the [feasibility report](../../feasibility-report.md) and the research notes, in particular [Nextflow channels](../../research/nextflow-channels.md), [Nextflow Groovy subset](../../research/nextflow-groovy.md), [Nextflow config](../../research/nextflow-config.md), [nf-core script inventory](../../research/nf-core-inventory.md), [nf-core config inventory](../../research/nf-core-config.md), [IR design models](../../research/ir-design-models.md) and [TES and executors](../../research/tes-and-executors.md). The parser is done. `nextflow-parser` agrees with `nextflow lint` 26.04.6 on all 14,624 nf-core files.

## Summary

- **The task layer comes first, and it knows no language.** Every driver turns its program into concrete tasks. A task is a record with three parts: a hashed specification, a placement and an attempt record. The shared layer never sees Groovy values, channels or process directives.
- **One task shape on every executor.** A shared wrapper script runs each task inside its container: on the local machine, under Docker or Podman, and on TES. The shape fits Aruna's narrow TES route: one executor, `s3://` inputs and outputs, no inline content.
- **Files are identified by content.** The identity of a file is its SHA-256 digest. When no digest is known, an immutable object version, as Aruna provides, stands in for it.
- **The cache key is the SHA-256 digest of the canonical hashed specification.** Resources, locations and the executor stay out of the key. So a result can be reused across runs, users and executors.
- **Reuse depends on whether the specification is complete.** If the specification captures everything the task depends on, the result is shared. If the task depends on something outside it, such as an unpinned image, the network or a mutable file, the result is only reused when the same run is resumed.
- **The Nextflow driver is deterministic.** It runs on one thread. Its only inputs are ordered events and recorded facts. The run log keeps both, so a resumed run can replay the original order and request the same tasks again.
- **The evaluator lowers the syntax tree once, then walks the lowered tree.** Lowering resolves names, applies the compile-time rewrites that Nextflow makes, and reports unsupported features before anything runs.
- **Values model the Groovy runtime classes exactly.** `Integer`, `Long` and `BigDecimal` stay distinct, maps keep insertion order, and formatting follows Groovy. Any difference can change a task command without an error.
- **The first milestone runs the nf-core FASTQC module end to end.** It uses local Docker, produces the same `.command.sh` as Nextflow 26.04.6, and hits the cache on the second run.

## 1. Scope and terms

### 1.1 Goals

- Run lint-clean, strict-syntax nf-core modules and pipelines natively, in one Rust binary ([decisions](../../decisions.md), items 1, 4 and 5).
- Keep the task layer usable by the other four languages without new fields in its record.
- Content-addressed data, a cache that works across runs, resume, TES and Aruna from the first milestones ([decisions](../../decisions.md), item 7).

### 1.2 Non-goals

- Nextflow's work directory layout, its task hashes, its `.nextflow` cache, and resuming a run that Nextflow started.
- Legacy syntax, Groovy classes in `lib/`, and JVM plugins that have no native version.
- Byte compatibility of the wrapper (`.command.run`). Only the task script (`.command.sh`) must match Nextflow byte for byte.

### 1.3 Terms

| Term | Meaning in this document |
|---|---|
| Driver | The part of iunctus that runs one language. It turns a program into task requests. The Nextflow driver is the front end in this design. |
| Task | One concrete unit of work. For Nextflow, one process task. |
| Attempt | One try to run a task. A retry is a new attempt of the same task. |
| Execution | One run of a hashed specification on an executor. Several tasks with the same cache key can share one execution. |
| Run | One invocation of `iunctus run`. |
| Lineage | A run and every run that resumes it, directly or indirectly. |
| Store | Content-addressed storage for files: a local directory, an S3-compatible bucket, or Aruna. |
| Cache key | The SHA-256 digest of the canonical hashed specification. |
| Run log | The append-only record of one run: requests, executions, completions and recorded facts. |
| Oracle | The real Nextflow 26.04.6 jar at `~/.cache/iunctus/oracle/nextflow-26.04.6-all.jar`, used for differential tests during development only. |

## 2. Architecture

```text
              iunctus run main.nf -profile test,docker
                                |
  +-----------------------------v-------------------------------+
  | Nextflow driver (one thread, deterministic)                 |
  |   nextflow-parser -> lowering -> evaluator                  |
  |   nextflow-config, nextflow-plugins                         |
  |   dataflow network: channels, operators, processes          |
  +---------------+---------------------------------^-----------+
    task requests, publish requests,                | ordered events
    synchronous host calls (read, list, time, put)  |
  +---------------v---------------------------------+-----------+
  | runtime: scheduler, task cache, run log, resume, publishing |
  +----------+----------------------------------+---------------+
             |                                  |
  +----------v------------+        +------------v--------------+
  | executors             |        | content store             |
  | local processes,      |        | local directory,          |
  | Docker, Podman, TES   |        | S3-compatible, Aruna      |
  +-----------------------+        +---------------------------+
```

The flow of one run:

1. The command line reads the runtime settings (executors, store, cache) and starts the Nextflow driver on its own thread.
2. The driver loads the pipeline, evaluates the config, and builds the dataflow network. It calls the runtime synchronously for file reads, directory listings and the current time.
3. When a process fires, the driver builds a task request and sends it to the runtime.
4. The runtime computes the cache key. It serves the task from the cache, or it hands the task to an executor.
5. The executor stages inputs from the store, runs the wrapper, and returns a result manifest.
6. The runtime writes the result to the run log and the cache, then delivers a completion event to the driver.
7. The driver collects the outputs as Nextflow values and continues the dataflow.

The boundary between steps 3 and 7 is the whole interface between a language and the platform. CWL, Snakemake, Argo Workflows and plain script drivers will use the same boundary.

## 3. Crate layout

Crates are named for what they are. They sit at the workspace root, like `nextflow-parser`.

| Crate | What it is | Depends on | Used by |
|---|---|---|---|
| `task-model` | Task record types, file references, digests, canonical encoding, cache key. No I/O, no async. | none | all languages |
| `content-store` | Store trait and implementations: local directory, S3-compatible (MinIO, cloud storage, Aruna), import from HTTPS. Upload by digest, object versions. | `task-model` | all languages |
| `executor` | Executor trait, capability model, the shared wrapper script, the result manifest. | `task-model`, `content-store` | all languages |
| `local-executor` | Local processes, and Docker and Podman through their command-line tools. | `executor` | all languages |
| `tes-executor` | GA4GH TES 1.1 client side, with server profiles for Funnel, Aruna and a generic server. | `executor`, the `tes` crate | all languages |
| `runtime` | Driver interface, scheduler, task cache, run log, resume, publishing service, image digest resolution. | `task-model`, `content-store`, `executor` | all languages |
| `nextflow-parser` | Exists. Lexer, parser and syntax trees for scripts and config files. | none | Nextflow |
| `nextflow-evaluator` | Lowering, value model, evaluator, standard library, stand-ins for Java classes. | `nextflow-parser` | Nextflow |
| `nextflow-config` | Config files, includes, profiles, params, process selectors, per-task settings. | `nextflow-evaluator` | Nextflow |
| `nextflow-plugins` | Native versions of plugin functions, starting with nf-schema. | `nextflow-evaluator`, `jsonschema` | Nextflow |
| `nextflow-runtime` | Module loading, dataflow network, operators, process-to-task translation, the Nextflow driver. | the Nextflow crates above, `runtime`, `task-model` | Nextflow |
| `iunctus` | The binary: command line, runtime settings, wiring of executors and stores. | all | |
| `nextflow-conformance` | Test harness only: oracle runs, module test generation, snapshot comparison. Never linked into the binary. | development only | |

### 3.1 Dependency rules

- `task-model` has no I/O. Its canonical encoding changes only together with its format version (section 4.3).
- No shared crate depends on a Nextflow crate. A workspace test checks this with `cargo metadata`.
- Nextflow crates never call an executor or a store directly. They go through the driver interface of `runtime` (section 4.12).
- `nextflow-evaluator` defines a small file system trait for `Path` methods such as `exists()` and `text`. `nextflow-runtime` implements it on top of the runtime's host calls. So the evaluator and the config crate can be tested without the runtime.
- `runtime` depends on the executor trait, not on its implementations. The binary chooses and wires the implementations.

### 3.2 Modules inside the Nextflow crates

- `nextflow-evaluator`: `lower` (resolution and rewrites), `value`, `number` (Java number types, including an own `BigDecimal`), `text` (Java string rules), `compare` (equality, ordering, hashing), `format` (Groovy string conversion), `dispatch`, `interpreter`, `stdlib` (one file per receiver type), `java` (class stand-ins such as `JsonOutput`, `Yaml`, template engines, `Date`), `regex`, `error`.
- `nextflow-runtime`: `program` (include graph and module instances), `dataflow` (channels and the scheduler loop), `operators` (one file per operator family), `process` (firing, directives, staging, rendering, output collection), `workflow` (take, emit, publish, the output block), `metadata` (the `workflow` and `nextflow` namespaces), `driver`.

### 3.3 What is shared and what is not

| Part | Shared now | Nextflow only | Share later |
|---|---|---|---|
| Task record, cache key, file identity | yes | | |
| Store, import of remote files, image digest resolution | yes | | |
| Wrapper, manifest, executors | yes | | |
| Scheduler, cache, run log, resume, publishing service | yes | | |
| Evaluator, value model, standard library | | yes | |
| Config semantics, selectors, params rules | | yes | |
| Dataflow network and operators | | yes | Argo `steps` could reuse the channel core |
| Glob dialect and output rules | | yes | a CWL glob module next to it |
| Task script rendering, env and eval capture | | yes | |
| Trace file, Workflow Run RO-Crate export | | | yes, built from attempt records |

The [feasibility report](../../feasibility-report.md) proposed one shared value model for all languages. This design shares only file references and plain JSON for metadata. Groovy values are too specific to share: `Integer` versus `Long`, `BigDecimal` scale and Groovy formatting all change task commands. CWL needs exact JSON numbers instead. Each driver keeps its own values and meets the others at the task record.

## 4. The shared task layer

### 4.1 File references and identity

A file reference has an identity and one or more locations.

- **Content identity.** A digest written as `sha256:<hex>`. This is the normal case.
- **Version identity.** A store name, an object key and an immutable version id. Aruna reports exact version ids for task outputs and reads inputs at a pinned version ([TES and executors](../../research/tes-and-executors.md), section 4). A version id is enough to reuse results. It only gives fewer cache hits than a digest, because the same bytes under two versions look different.
- **No identity.** A mutable location, for example a local file that was not hashed yet, or an `https://` URL. Such a file must get an identity before it can be part of a task specification. The runtime hashes it, or imports it into the store and hashes it on the way.

Locations are `file://`, `s3://` and `https://` URLs. A location is never part of the cache key.

A directory is a tree manifest: relative paths, each with a file identity, a size and an executable flag, plus empty directories. The identity of a directory is the digest of its canonical tree manifest.

**Digest choice: SHA-256.** The research notes suggest BLAKE3 for an own cache. This design picks SHA-256 for four reasons. The wrapper can compute it inside almost any container, with `sha256sum` from coreutils or BusyBox. S3-compatible stores can verify it on upload. OCI image digests use it. And no single algorithm serves every consumer anyway: CWL needs SHA-1 and nf-test snapshots use MD5, and both are computed on demand. Digests carry their algorithm name, so a later change stays possible.

**Local input files.** Hashing large local inputs on every run is slow. The runtime keeps a stat cache: path, size, modification time, inode and change time map to a digest. An unchanged file is hashed once. This is the "metadata as a fast path" idea of the [IR design notes](../../research/ir-design-models.md), section 9.

### 4.2 The task record

The record follows the three parts of the [feasibility report](../../feasibility-report.md). Only the first part is hashed.

**Hashed specification: what is computed.**

| Field | Contents |
|---|---|
| `format` | Version of the record format. Starts at 1. |
| `namespace` | Cache namespace from the runtime settings, empty by default. Changing it disowns all earlier results, like the `salt` of the remote execution API. |
| `image` | Container image reference pinned to a digest, and the platform, for example `linux/amd64`. Absent for tasks that run on the host. |
| `arguments` | The program and its arguments, run in the work directory. For Nextflow: `["/bin/bash", "-ue", ".command.sh"]`, from the `shell` directive. |
| `environment` | Variables and values, sorted by name. A value is either literal text or "the work directory path". Secret values are never stored, only secret names. |
| `inputs` | Every staged file or directory: relative path in the work directory, kind, identity, executable flag, writable flag. Sorted by path. The task script is one of these inputs. |
| `outputs` | Collection rules: shell glob patterns relative to the work directory. The exact language rules are applied later by the driver (section 4.5). |
| `stdout`, `stderr` | Relative paths of the files that capture the two streams. Nextflow uses `.command.out` and `.command.err`. |
| `stdin` | Optional file identity. |

**Placement: where and how big.** Not hashed.

| Field | Contents |
|---|---|
| Resources | CPU cores (whole numbers, rounded up), memory, disk, time limit, accelerators with count and type. |
| Target | Executor name, queue, architecture, preemptible flag, backend parameters. |
| Local extras | Container options (Nextflow `containerOptions`, `docker.runOptions`) and a host preamble (Nextflow `beforeScript`). Only the local executor accepts them. Other executors reject a task that needs them, with a clear error. |
| Concurrency group | A name and a limit, from Nextflow `maxForks`. |

**Request information.** Also not hashed. It travels with the request.

| Field | Contents |
|---|---|
| Driver task id | Stable id inside the run, for Nextflow the fully qualified process name and the task index. |
| Display name | For logs, for example `NFCORE_DEMO:DEMO:FASTQC (SAMPLE1)`. |
| Reuse scope | `shared`, `lineage` or `none` (section 4.7). |
| Attempt number | Starts at 1. |

**Attempt record: what happened.** Written by the runtime.

| Field | Contents |
|---|---|
| Identity | Run id, driver task id, attempt number, cache key. |
| Execution | Executor, attempt name used for idempotent submission, backend handle such as the TES task id. |
| States | Time stamps for queued, running and finished. |
| Outcome | Succeeded, tool failed (exit code), infrastructure failed, preempted, cancelled, or reused (with the run that produced the result). |
| Result | Digest of the result manifest, locations of stdout and stderr, output files with identities, sizes and version ids. |
| Usage | Peak memory and CPU time, when the executor reports them. |

The record has no language-specific field. The rules of section 4.4 and 4.5 show how Nextflow features such as `eval` outputs fit without one.

### 4.3 Canonical form and cache key

- The hashed specification is encoded as JSON under the JSON Canonicalization Scheme (RFC 8785). JSON is readable, and other tools can recompute a key.
- The cache key is `sha256` over those bytes.
- Golden tests fix the key of a set of example specifications. Any change to the encoding must bump `format`.
- Input order does not matter, because inputs are sorted by path. So a set of files given in two orders gives one key. Order still matters when it appears in the task script, for example `cat ${reads} > all.fq`. Section 4.8 handles that case.

### 4.4 The wrapper and the task shape

One wrapper script, generated by the `executor` crate, runs every task. It is Bash, because Nextflow task scripts need Bash anyway. The same wrapper runs inside the container on local Docker, on Podman and on TES. Without a container it runs on the host. So the local Docker executor is also a faithful model of a TES task.

Steps of the wrapper:

1. Create the work directory `/iunctus/work` and the control directory `/iunctus/control`, and change to the work directory. On the host without a container, both are directories under the scratch area.
2. Export the environment of the specification. Values marked "the work directory path" get the real path. The Nextflow driver uses this to set `NXF_TASK_WORKDIR`, which the Nextflow env capture code expects.
3. Record the start time.
4. Run the arguments with the two streams redirected to the `stdout` and `stderr` files. Redirection happens in the wrapper, because Aruna rejects the TES `stdout` and `stderr` fields.
5. Record the end time and the exit code in the control directory.
6. If the exit code is 0, collect the outputs (section 4.5). If it is not 0, collect only the control files and the two stream files, so that partial large outputs are not uploaded.
7. Write the result manifest: every collected file with relative path, kind, size, executable flag and SHA-256 digest when `sha256sum` exists in the image.
8. Exit with code 0 whenever steps 5 to 7 succeeded.

Step 8 differs from the research recommendation, which let the wrapper exit with the tool's code. The reason: a TES server may skip output uploads after an executor error, and then the logs of a failed task are lost. With exit code 0, the server always uploads the control files, and iunctus reads the real exit code from them. A non-zero wrapper exit, or a task without a control file, means the wrapper itself was killed. The runtime reports that as "killed, exit code unknown".

The wrapper only knows neutral names. Nextflow file names such as `.command.sh`, `.command.out` and `.command.env` appear only as values chosen by the Nextflow driver in the specification.

### 4.5 Output collection and the result manifest

Output rules are shell glob patterns. They are a superset of what the language needs:

- The wrapper expands each pattern with Bash (`globstar`, `nullglob` and `dotglob` on), and collects every match. A matched directory is collected with all its contents.
- The driver then applies its exact rules to the manifest. For Nextflow these are the Java glob rules, hidden files, `type`, `followLinks`, `includeInputs`, `arity` and `optional` ([Nextflow channels](../../research/nextflow-channels.md), section 7.8).

Nextflow itself works this way on cloud executors: its wrapper uploads everything that `ls -1d <patterns>` matches, and the engine applies the exact rules to the remote listing. The cache stores the superset. So a cached result stays valid for any driver that reads it.

**Where the files go.**

- Local executors: the runtime moves collected files from the scratch directory into the store after the wrapper ends. It hashes them with Rust on the host, so the in-container digest is not needed.
- TES: the wrapper moves collected files into a flat outbox, `/iunctus/control/out/<number>`, and the manifest maps each number to its relative path. One wildcard output with `path_prefix` uploads the outbox. Directories with many files are packed into a tar archive first, because of Aruna's limit of 1,024 outputs. The manifest describes the tree, so the directory identity is the same with or without packing.

**Features that look language-specific.**

- Nextflow `eval` and `env` outputs: Nextflow appends capture code to the task script (the `command-env.txt` template in the Nextflow jar). The Nextflow driver does the same. The capture writes `.command.env`, which is just one more output rule. The driver reads the file after the task.
- Nextflow `stdout` outputs: the driver reads the `stdout` file of the result.
- CWL `cwl.output.json` and Snakemake outputs fit the same way: a file rule plus driver-side interpretation.

### 4.6 The content-addressed store

Layout, the same in a local directory and in a bucket:

```text
blobs/sha256/<2 hex>/<62 hex>          files known by digest: imported inputs, task scripts, engine-made files
trees/sha256/<2 hex>/<62 hex>          directory manifests
runs/<run id>/attempts/<attempt>/...   task outputs, under their original relative paths
runs/<run id>/log/                     optional copy of the run log
cache/v1/<namespace>/<key>.json        shared cache entries
```

Rules:

- Local files that a remote executor needs are uploaded once, under their digest. Remote `s3://` inputs are passed through. `https://` inputs are imported into `blobs/`, because Aruna only accepts `s3://` URLs.
- Task outputs keep their original names under the attempt prefix. File names matter to Nextflow code: `file.name`, `baseName` and the Groovy string form of a path are all visible to scripts.
- In a local store, each output is also hard-linked into `blobs/` by digest. This costs nothing and gives true content addressing. In a remote store, outputs stay under the attempt prefix, and the cache index maps digests to locations. A later command can promote the outputs of a run into `blobs/` with server-side copies.
- The store reads files for the driver only when the driver needs their content: sample sheets, `.command.env`, `splitCsv`, `file.text`. Everything else stays a location. An output of one task becomes an input of the next without passing through iunctus.
- **Aruna.** Aruna is the first remote store, through its S3-compatible interface. iunctus records the version id of every object it writes or reads. Aruna's TES route pins input versions when it accepts a task, and it reports exact output versions ([TES and executors](../../research/tes-and-executors.md), section 4). These version ids go into the attempt record. A Workflow Run RO-Crate export for Aruna can later be built from the run log and the attempt records.

Removing old runs must respect the cache. `iunctus clean` removes a run together with the cache entries that point into it, unless the run was promoted.

### 4.7 The task cache: when a result may be reused

The [decisions](../../decisions.md) ask for "rules for deterministic and non-deterministic steps". This design states the rule in terms of completeness.

A cached result is valid for another request if the hashed specification captures everything the task depends on. Whether the tool is deterministic does not matter. Any successful execution of the same specification is an acceptable answer. What matters is whether something outside the specification could change the answer.

| Reuse scope | When | Reuse |
|---|---|---|
| `shared` | The image is pinned to a digest. Every input has a content or version identity. The task does not need the network. The driver did not mark it otherwise. | Any run that can read the cache. |
| `lineage` | The image could not be pinned (for example offline). An input has only a lenient metadata identity. The task needs the network, such as a database download. The user marked the process as non-deterministic in the runtime settings. | Only runs of the same lineage, that is, when resuming. |
| `none` | Nextflow `cache false`. | Never. |

Steps inside the engine are a separate case. Operators, closures and engine-made files such as `collectFile` results are cheap and are computed again on resume. Their only non-determinism is order and outside facts. Section 4.8 records those.

A cache entry holds:

- the cache key, the namespace and the canonical specification (for checks and debugging);
- the result manifest, with output identities and locations;
- the reuse scope, the lineage id for `lineage` entries, the run and attempt that produced it, and the time.

Lookup and write:

1. Look in the local index (a `redb` database), then in the shared cache in the store if one is configured.
2. Check the entry: all output locations must still exist (parallel `HEAD` or `stat`). A missing output makes the entry invalid, and the task runs again.
3. Write an entry only after a successful execution, and only for `shared` and `lineage` scopes. Writes are "put if absent": the first writer wins.
4. Requests with the same key that are in flight at the same time share one execution, except in scope `none`.

Early cutoff comes for free. If a task runs again and produces the same bytes, the keys of all downstream tasks stay the same. In practice many bioinformatics outputs contain time stamps, so this helps less than in build systems.

Nextflow's `cache` directive maps as follows: `false` gives scope `none`. `true` and `'lenient'` only control the local stat cache (whether the modification time counts). `'deep'` is what iunctus always does.

### 4.8 The run log, external facts and resume

The run log is an append-only file per run. Each record has a sequence number, a time, a kind, a JSON payload, and a length and checksum to detect a torn write at the end. Kinds:

- run started: iunctus version, program digest, config digest, params, lineage id;
- fact recorded: call site, occurrence number, value;
- task requested: driver task id, cache key, reuse scope;
- execution submitted: attempt name, executor, backend handle;
- task finished: cache key, outcome, result manifest digest;
- published, driver error, run finished.

The runtime writes ahead: a submission is logged before the executor is called, and a completion is logged before it is delivered to the driver. So the log order is the delivery order.

**External facts.** The driver asks the runtime for everything that is not a function of the program: the current time, random ids, the generated run name (Nextflow uses random names such as `sick_boltzmann`), `workflow.sessionId`, and random seeds. Each answer is recorded under its call site and occurrence. On resume, recorded answers are returned again. nf-core pipelines put a time stamp into params (`new java.util.Date().format(...)`, 134 pipelines per the [config inventory](../../research/nf-core-config.md)). If that time changed on resume, the parameter summary that MultiQC reads would change, and MultiQC would run again.

Directory listings are not recorded. iunctus sorts them instead (section 5.14), which makes them deterministic.

**Resume**, with `iunctus run ... -resume [run id]`:

1. The new run joins the lineage of the old one.
2. The runtime reads the old log: its facts, the order in which tasks finished (by cache key), and executions that were submitted but never finished.
3. Executors reconcile. They find the unfinished executions by attempt name and adopt them instead of submitting again.
4. The driver evaluates the program from the start. Host calls return recorded facts where they exist.
5. Every task request is looked up: a finished result of the lineage or the shared cache is a hit, an adopted execution is awaited, anything else is submitted.
6. Hits are delivered in the old completion order (ordered replay, below).
7. When the replayed part is used up, the run goes on live.

**Ordered replay.** Operators such as `mix`, `join` and `groupTuple` pass on values in arrival order, and arrival order comes from task completion order. If cached tasks all "finish" at once on resume, a list such as a `groupTuple` bag can come out in a new order. If that list is rendered into a task script, the key changes and the cache misses. Nextflow's documentation warns about exactly this ([IR design notes](../../research/ir-design-models.md), section 8).

The runtime therefore keeps the old completion order as a list of keys with a cursor:

- A hit for the key at the cursor is delivered at once, and the cursor moves on.
- A hit for a later key waits.
- When the driver is idle and the key at the cursor was not requested, the cursor skips it.

Each skip moves the cursor, so replay always ends. Live completions are delivered when they arrive. Replay is best effort. A divergence, for example after the user edited the pipeline, costs cache hits and never correctness, because keys are content based.

**Size.** A run of 100,000 tasks writes a few hundred thousand records, which is tens of megabytes. Resume cost is dominated by evaluating the program again, not by reading the log. Snapshots can come later if measurements ask for them. This differs from Temporal, which stores history in a service with hard event limits.

### 4.9 The executor interface

The trait follows the nine operations of the [feasibility report](../../feasibility-report.md), section "Nine operations make up the executor trait". A sketch:

```rust
pub trait Executor: Send + Sync {
    fn capabilities(&self) -> &Capabilities;
    /// Turns store locations into locations this backend can read, and stages the wrapper.
    async fn prepare(&self, execution: &Execution) -> Result<Prepared, ExecutorError>;
    /// Idempotent under the attempt name: a second call returns the existing handle.
    async fn submit(&self, prepared: Prepared) -> Result<Handle, ExecutorError>;
    /// Status of many executions in one call.
    async fn poll(&self, handles: &[Handle]) -> Result<Vec<StatusUpdate>, ExecutorError>;
    async fn cancel(&self, handle: &Handle) -> Result<(), ExecutorError>;
    /// Reads the control files and the result manifest after a terminal state.
    async fn result(&self, handle: &Handle) -> Result<ExecutionResult, ExecutorError>;
    async fn logs(&self, handle: &Handle, tail_bytes: usize) -> Result<Logs, ExecutorError>;
    /// Finds executions of a run after a restart, without changing them.
    async fn reconcile(&self, run: &RunId) -> Result<Vec<(AttemptName, Handle, Status)>, ExecutorError>;
    /// Removes scratch data after the result is durably recorded.
    async fn cleanup(&self, handle: &Handle) -> Result<(), ExecutorError>;
}
```

Capabilities are explicit flags, in the style of Snakemake's executor settings: container support, local extras, directory inputs and outputs, output wildcards, inline content, maximum inputs and outputs, URL schemes, GPUs, preemption, and whether a shared file system exists.

Errors have four kinds: tool failure (the task ran and exited non-zero), infrastructure failure (retryable), preempted and cancelled. The runtime retries infrastructure failures and preemption itself, a few times, without asking the driver. Tool failures go to the driver, which applies the language's error strategy. An out-of-memory kill with exit code 137 is a tool failure, as in Nextflow.

### 4.10 Executors

**Local processes.** For tasks without a container. The executor creates a scratch directory, stages inputs by hard link (or copy, or symbolic link, following Nextflow `stageInMode`), runs the wrapper in its own process group with `process-wrap`, and moves outputs into the store. It enforces the time limit itself. It runs the host preamble (Nextflow `beforeScript`) before the wrapper, as Nextflow does.

**Docker and Podman.** Through their command-line tools, not the Docker API. Three reasons: nf-core passes `docker.runOptions` and `containerOptions` as raw command-line text (for example `-u $(id -u):$(id -g)` and `--gpus all`); the same code then serves Podman; and it matches what Nextflow does. The scratch directory is mounted at `/iunctus/work`. The container name is the attempt name, so `reconcile` can find running containers after a restart. CPU and memory limits come from the placement.

**TES.** Built on the `tes` crate, in the portable shape of the [TES research](../../research/tes-and-executors.md), section 8:

- one TES task per execution, with exactly one executor: `["/bin/bash", "/iunctus/control/wrapper.sh"]`, work directory `/iunctus/work`;
- the wrapper and the task script are uploaded to the store and staged as normal inputs, because Aruna rejects inline content;
- every input has an explicit type; directory inputs are expanded into files on servers without directory support;
- outputs: the outbox wildcard plus the control files;
- resources rounded up; GPUs and time limits only through backend parameters that the server lists in its service info;
- tags: run id, attempt name, cache key, a group tag for polling, and Aruna's idempotency key;
- one tag-filtered `ListTasks` call per interval, `FULL` view only for finished tasks, and always the last task log;
- `ignore_error` is never sent.

Server profiles (Funnel, Aruna, generic) switch optional features on after a probe of the server.

**Later.** Apptainer through its command line, Slurm natively or through Funnel, and Kubernetes. The trait does not need to change for them.

### 4.11 The scheduler

The scheduler in `runtime` owns everything between a request and a completion event:

- computing the key, the cache lookup and in-flight sharing;
- choosing an executor from the placement and the runtime settings;
- admission: local CPU and memory capacity (like Nextflow's `executor.cpus` and `executor.memory`), a queue limit per remote executor (like `executor.queueSize`), and concurrency groups (`maxForks`);
- staging decisions: which store objects must be copied or uploaded for the chosen executor;
- polling, time limits, retries of infrastructure failures, and cancellation on abort or Ctrl-C;
- writing the run log and cache entries;
- the publishing service: copy, hard link, symbolic link or move from the store to local or `s3://` targets, on request of the driver.

The scheduler runs on Tokio. The driver never does.

### 4.12 The driver interface

A driver is a state machine, as in miniwdl ([IR design notes](../../research/ir-design-models.md), section 5). A sketch:

```rust
pub trait Driver {
    /// Loads the program and returns the first requests.
    fn start(&mut self, host: &mut dyn Host) -> Result<Step, DriverError>;
    /// Handles one event, runs until it has no more work, and returns new requests.
    fn handle(&mut self, event: Event, host: &mut dyn Host) -> Result<Step, DriverError>;
}

pub struct Step {
    pub requests: Vec<Request>,          // run a task, cancel a task, publish files
    pub finished: Option<RunOutcome>,
}

/// Synchronous services for the driver thread. Answers that are not a function of
/// the program are recorded in the run log and replayed on resume.
pub trait Host {
    fn read(&mut self, file: &FileRef) -> Result<Bytes, HostError>;
    fn list(&mut self, location: &Location, pattern: &str) -> Result<Vec<Entry>, HostError>;
    fn identify(&mut self, location: &Location) -> Result<FileRef, HostError>;
    fn put(&mut self, bytes: Bytes, name: &str) -> Result<FileRef, HostError>;
    fn fact(&mut self, site: &CallSite, kind: FactKind) -> Result<FactValue, HostError>;
    fn environment(&self, name: &str) -> Option<String>;
    fn log(&mut self, level: Level, message: &str);
}
```

The driver runs on its own operating system thread. Host calls block that thread. This keeps the evaluator synchronous, which is much simpler than an async tree walker. Values never leave the driver thread, so the evaluator can use `Rc` instead of `Arc`.

### 4.13 Worked example: one FASTQC task

The nf-core FASTQC module ([nf-core script inventory](../../research/nf-core-inventory.md) shows the module pattern) is called with `[[id: 'test', single_end: true], [file('https://raw.githubusercontent.com/.../test_1.fastq.gz')]]`.

1. The driver fires the process. `path(reads, stageAs: '?/*')` stages the file as `1/test_1.fastq.gz`.
2. The driver resolves directives. The container expression picks `quay.io/biocontainers/fastqc:0.12.1--hdfd78af_0`, because `workflow.containerEngine` is `docker`.
3. The driver renders `.command.sh` exactly as Nextflow does: script text, indentation stripping, the shebang `#!/bin/bash -ue`, and the env capture code for the `eval` output.
4. The runtime pins the image to a digest (a recorded fact) and imports the test file into `blobs/` (hashing it on the way).
5. The hashed specification, shortened:

```json
{
  "format": 1,
  "namespace": "",
  "image": {"reference": "quay.io/biocontainers/fastqc@sha256:...", "platform": "linux/amd64"},
  "arguments": ["/bin/bash", "-ue", ".command.sh"],
  "environment": {"NXF_TASK_WORKDIR": {"work_directory": true}},
  "inputs": [
    {"path": ".command.sh", "kind": "file", "identity": "sha256:..."},
    {"path": "1/test_1.fastq.gz", "kind": "file", "identity": "sha256:..."}
  ],
  "outputs": [{"pattern": "*.html"}, {"pattern": "*.zip"}, {"pattern": ".command.env"}],
  "stdout": ".command.out",
  "stderr": ".command.err"
}
```

6. The key misses the cache. The local Docker executor runs the wrapper. The manifest lists `test_fastqc.html`, `test_fastqc.zip` and `.command.env`.
7. The driver applies the Nextflow output rules. It binds `[meta, test_fastqc.html]` to `html`, `[meta, test_fastqc.zip]` to `zip`, and `['FASTQC', 'fastqc', '0.12.1']` to `versions_fastqc` and to the topic `versions`.
8. A second run computes the same key and reuses the result. No container starts.

## 5. The Nextflow front end

