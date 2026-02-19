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

### 5.1 Loading a pipeline

1. Read the manifest scope of the project `nextflow.config` alone, to find the main script ([Nextflow config](../../research/nextflow-config.md), section 2.7).
2. Evaluate the config (section 5.9). The script needs `params` and `workflow.containerEngine`.
3. Parse the main script and follow `include` statements to all modules. Report parse errors of all files at once.
4. Lower every module (section 5.2). Report all load-time errors at once, including unsupported features.
5. Run the top-level statements of each module in load order: legacy `params.x = v` assignments and feature flags.
6. Evaluate the entry workflow body. This builds the dataflow network (section 5.11).
7. Apply the `output` block, then ignite the network.

`iunctus check <pipeline> -profile <names>` runs steps 1 to 6 and prints every unsupported feature. With `-stub` and a built-in executor that runs stub scripts without containers, it also reaches the run-time paths. The corpus report in section 6 uses this command.

### 5.2 Evaluator representation: a lowered tree

**Decision.** The evaluator does not walk the syntax tree of `nextflow-parser` directly. A lowering pass turns each module into a lowered tree once. The evaluator walks the lowered tree. There is no bytecode.

What lowering does:

- **Resolves names** the way the strict checker does: local slots, captured variables, module members (functions, processes, workflows, included names), built-in names, and supported class names. A name that stays unresolved is the error "`x` is not defined", as in `nextflow lint`. Only dynamic contexts keep names open (section 5.6).
- **Applies the compile-time rewrites of Nextflow** ([Nextflow Groovy subset](../../research/nextflow-groovy.md), section 2): GString to String except nested GStrings; comparisons through the `LangHelpers` rules; wrapping of dynamic directives; path escaping of values interpolated in `script:` and `stub:` sections; `branch` and `multiMap` criteria; the implicit `it` parameter; `set` and `tap` as declarations; named arguments collected into a map as the first argument.
- **Turns statement-shaped declarations into structures.** The syntax tree keeps legacy inputs such as `tuple val(meta), path(reads, stageAs: '?/*')` and outputs such as `path("*.html"), emit: html` as statements, because that is how they are written. Lowering turns them into typed input and output declarations, and directives into a list with a "dynamic" flag.
- **Pre-computes literals.** Integer literals become `Integer`, `Long` or `BigInteger`. Decimal literals become `BigDecimal` with their scale. Regular expression literals are compiled.
- **Reports unsupported features** before anything runs (section 5.7).

Every lowered node keeps its source span for error messages.

**Why not walk the syntax tree directly.** Nextflow's own meaning comes from compile-time rewrites plus Groovy dynamic dispatch. Doing the rewrites in the walker would mean deciding them again at every step, deep inside a run that may be hours old. Decision 5 asks for load-time errors, which needs a whole-program pass anyway. Resolved names make closures cheap, and closures run once per channel item and per task. And `nextflow-parser` stays a faithful source tree for other tools, such as a formatter or a language server.

**Why not bytecode.** The expensive work in this language is value work: `BigDecimal` arithmetic, string building, list and map copies, and hashing. Dispatch of a resolved tree is a small part of that. A virtual machine would add a register allocator, a closure capture scheme and a debugger, without a clear gain. If profiling shows otherwise, a later compiler can start from the lowered tree.

### 5.3 Value model

The value model keeps Groovy runtime classes apart when the difference is visible ([Nextflow Groovy subset](../../research/nextflow-groovy.md), section 3).

| Value | Representation | Notes |
|---|---|---|
| `null` | variant | |
| Boolean | `bool` | |
| Integer, Long | `i32`, `i64` | Arithmetic wraps on overflow, as in Java. |
| BigInteger | `Rc<BigInt>` (`num-bigint`) | |
| Float, Double | `f32`, `f64` | Float appears through config conversion of command-line text. |
| BigDecimal | own type: unscaled `BigInt` plus `i32` scale | Java division rules, `toString`, `setScale` with half-up rounding. An own type is safer than a general crate, because every Java rule must match. |
| String | shared text with an ASCII flag and a cached Java hash | Lengths and indexes use UTF-16 code units, as Java does. The ASCII flag keeps the common case fast. |
| GString | rendered text plus a marker | Only nested GStrings survive at run time. They render like strings, but Java equality with a String is false. |
| List | `Rc<RefCell<..>>` with a kind: `ArrayList`, tuple (`ArrayTuple`), `ArrayBag` (from `collect`), `BlankSeparatedList` (several task files), `String[]` (from `split`) | Reference semantics, because Groovy mutates in place (`<<`, `sort()`, `unique()`). The kind decides mutability, closure spreading and formatting. |
| Range | lazy description: `IntRange`, `NumberRange`, `ObjectRange`, empty | Turned into elements only when needed. |
| Map | `Rc<RefCell<..>>` of an insertion-ordered map, with a kind: `LinkedHashMap`, record, `TreeMap` (from `sort()`) | Keys use Java `equals` and `hashCode`: `Integer` 1 and `Long` 1 are different keys. |
| Set | insertion list plus a kind: `LinkedHashSet` or `HashSet` | A `HashSet` iterates in Java `HashMap` bucket order. `toSet()` needs this. |
| Map entry | pair | `key=value` in Java formatting. |
| Closure | lowered code, captured cells, owner module, optional delegate | |
| Path | location or task path: a staged name plus the file reference | `toString()` follows Nextflow: a task path renders its staged name, an `s3://` path renders `/bucket/key`. |
| MemoryUnit, Duration | `u64` bytes, `u64` milliseconds | Own `toString` rules, and the zero-value assertion in comparisons. |
| VersionNumber | parsed parts plus source text | `nextflow.version`. |
| Pattern, Matcher | compiled regular expression, match state | |
| Channel | index into the dataflow arena, with its kind (queue or value) | |
| Channel result | named list of channels (`ChannelOut`) | Result of a process or workflow call. |
| Process, workflow, function | index into the program | Callable values. |
| Group key | key plus size | `groupKey(key, n)`, with its one-sided equality. |
| Object | `Rc<dyn JavaObject>` | Stand-ins for Java classes: `Date`, `File`, `JsonSlurper`, `Yaml`, template engines, exceptions. |

### 5.4 Arithmetic, equality and formatting

These three areas decide the text of task scripts. Each gets one module and exhaustive tests.

- **Arithmetic** follows Groovy's promotion table: floating point, then `BigDecimal`, then `BigInteger`, then `Long`, then `Integer`. Division of integers is `BigDecimal` division with the ten-digit rule. `**` can return a `BigInteger`. `MemoryUnit` and `Duration` have their own rules.
- **Three relations** are kept apart:
  - Groovy equality (`==`), with number coercion, one-character strings equal to their character code, lists compared element by element, and the `MemoryUnit` and `Duration` rules;
  - Java equality and hash code, for map keys, `contains`, `in` on lists, and the keys of `join` and `groupTuple`;
  - Groovy ordering (`<`, `<=>`, sorting with the number-aware comparator).
- **Three string conversions** are kept apart: the GString form (`FormatHelper.write`, where a range renders as a list), the Groovy `toString` form (used by `println`, `+`, `join` and `as String`, where a range renders as `1..3`), and the Java form (`String.valueOf`, where a map renders as `{a=1}`).

The list of high-risk behaviours in [Nextflow Groovy subset](../../research/nextflow-groovy.md), section 14, becomes a test checklist for the first milestone that touches each area.

### 5.5 Method and property dispatch

- Each value has a class chain that mirrors Java and Groovy. A tuple is `ArrayTuple`, then `List`, `Collection`, `Iterable`, `Object`. A task path is `TaskPath`, then `Path`, `Object`.
- Built-in methods live in one registry, keyed by class and name. Each entry records its argument shapes, whether it has side effects, and the Groovy or Nextflow source it mirrors.
- A call walks the chain of the receiver and picks the first entry whose shape fits. Each call site caches the last class and entry.
- Named arguments are collected into a map that becomes the first argument. A trailing closure is the last argument.
- Property access follows Groovy: maps read keys (so `map.class` is a key lookup); other objects call `getName()` or `isName()`; lists read their own properties first and spread the rest over their elements; numbers fall back to unit suffixes such as `2.GB`; `null` fails unless safe navigation is used.
- Static calls (`Math.max`, `System.getenv`, `groovy.json.JsonOutput.toJson`) resolve at load time to the stand-in class, or they fail at load time.

### 5.6 Closures, scopes and delegates

- Closures capture variables by reference, in shared cells, so a later assignment is visible and `total += v` inside `each` works.
- `{ -> x }` and `{ x }` both get one optional parameter named `it`, as in Nextflow.
- Argument adaptation follows Groovy: one list argument is spread over several parameters only when no signature takes it whole. Map iteration passes `(key, value)` to two-parameter closures.
- **Dynamic contexts.** Process directive values, config closures in the `process` scope, `executor.jobName` and `publishDir` closures may use names that lowering cannot resolve, such as `meta` or `task`. Such names become dynamic lookups. At call time, a clone of the closure gets the task context as its delegate, and names resolve in this order ([Nextflow config](../../research/nextflow-config.md), section 7.2):
  1. task inputs, task-local variables and `task`;
  2. the binding of the script that defines the process: `params`, `workflow`, `nextflow`, `projectDir`, `moduleDir`, `launchDir`, `workDir`, `baseDir`, `secrets`;
  3. the config `env` scope;
  4. the launch environment;
  5. the variables of the config file.
  An unknown name fails with "No such variable". Local and captured variables are never looked up through the delegate.
- Method calls without a receiver in a task context go to the functions of the defining script, except `template(...)`.

### 5.7 Exceptions and unsupported features

- Evaluation returns `Result<Value, Control>`. `Control` is a return, a thrown exception, or a fatal error.
- Exceptions carry a Java class chain, such as `NullPointerException`, `RuntimeException`, `Exception`, `Throwable`. `catch (e: IOException)` matches by chain. `catch (e)` catches `Exception` but not `Error`, so a failed `assert` and the internal assertions of `MemoryUnit` still end the run, as in Nextflow.
- Built-in failures raise the Java exception that Groovy would raise, because pipeline code catches some of them.
- `error(message)` aborts the run with the message. `exit` is supported as deprecated behaviour.

**Unsupported features fail loudly, in four groups.**

| Group | Example | When it fails |
|---|---|---|
| Rejected by strict syntax | `for` loops, `import`, `def check_max(...)` in config | Parse time, with the official message (already done in `nextflow-parser`). |
| Outside the iunctus scope | Groovy classes from `lib/`, JVM plugins without a native version, unknown Java classes | Load time. |
| Not implemented yet | A GDK method, an operator option, a directive | Load time if the name is unknown on every value kind. Otherwise at the first call, with the receiver class. |
| Deliberately different | A run that Nextflow would leave hanging | Run time, with an explanation (section 5.14). |

Every such error names the file, line and column, the construct, and its group. The process exits with a dedicated exit code (CWL runners use 33 for unsupported requirements; iunctus can use the same number).

To tell "not implemented yet" from "no such method", the oracle jar produces a table of every public JDK, GDK and Nextflow extension method on the relevant classes at development time. The table is checked in. The message can then say "`String.center` is a Groovy method that iunctus does not implement yet".

### 5.8 Standard library strategy

The inventory decides the order, not the documentation.

1. **The FASTQC module and the module template** (milestone 1): the methods and operators that FASTQC, its test and the module template need, and the twenty high-risk behaviours.
2. **All module scripts** (milestone 3): whatever `iunctus check` still reports over the 2,215 module files. The module template is uniform, so this set is small.
3. **The pipeline template** (milestone 4): the three utility subworkflows that about 114 pipelines vendor. They need `Yaml` (load, dump, `dumpAsMap`), `JsonOutput` and `JsonSlurper`, `SimpleTemplateEngine` and `GStringTemplateEngine`, `Date` with `format`, `File`, `FilesEx.copyTo`, and `workflow.toMap()`.
4. **Breadth** (milestone 6): the 60 value methods that 50 or more pipelines use, then the long tail, ranked by "pipelines unlocked". A pipeline is unlocked when its last missing feature is done. The nightly corpus report computes this ranking.

Some stand-ins need exact output, because their text ends up in task inputs:

- the YAML emitter must match SnakeYAML for the shapes nf-core uses, because version YAML files become MultiQC inputs;
- the template engines must match Groovy for `${...}` and the `<% %>` shapes in nf-core assets, because the methods description also becomes a MultiQC input.

Regular expressions use `fancy-regex` behind a Java dialect layer. The layer translates what it can and rejects the rest with a clear error when the pattern is compiled, which is load time for literal patterns. Replacement strings (`$1` and backslash escapes) follow Java rules in own code.

`execute()` on strings and lists, and `URL.openConnection()`, appear only in completion functions of the utility subworkflows (conda channel checks, email, Slack and Teams hooks). They run on the host and are recorded as facts. Until they exist, they fail with a clear error, and only when a pipeline actually reaches them, for example when `params.email` is set.

### 5.9 Config evaluation

Config files use the same evaluator in a config mode, with the names allowed by nf-lang's `ConfigDsl` ([Nextflow config](../../research/nextflow-config.md), section 3.6).

- **Value tree.** An ordered map. Overwriting keeps the position of a key, and new keys go to the end. Values are evaluator values, nested maps, or config closures (lowered code plus the file's variables).
- **Two merge operations.** Assignment replaces, even a map. Includes and the merge of top-level files deep-merge maps and replace everything else. Writing below a value that is not a map is an error.
- **Files.** `$NXF_HOME/config`, the project `nextflow.config`, the launch directory `nextflow.config`, then every `-c` file; `-C` replaces the list. Each top-level file starts with the command-line and params-file params only.
- **Includes.** Only at the top level or directly in a profile. The path is relative to the including file. An included file starts with a copy of the includer's params at that point. Constant paths must exist at compile time. `https://` includes are fetched, stored by digest and recorded in the run log. With `--offline`, `System.getenv('NXF_OFFLINE')` answers `true`, so the nf-core pattern `includeConfig !System.getenv('NXF_OFFLINE') && ... ? url : "/dev/null"` includes nothing.
- **Profiles.** Applied at the end of each `profiles` block, in `-profile` order, at the top level. `standard` applies when no profile is given and one exists. Unknown names fail after all files are read.
- **Params precedence.** Script defaults, then config params in file order, then the params file, then the command line ([Nextflow config](../../research/nextflow-config.md), section 5). Inside the config, command-line text is converted to the type of the config default (a decimal becomes a 32-bit `Float`). At run time, legacy params keep the raw command-line strings. A typed `params { }` block converts to the declared types.
- **Process selectors.** Stored under `withLabel:<pattern>` and `withName:<pattern>` keys. For each process invocation, the settings are resolved once with the algorithm of [Nextflow config](../../research/nextflow-config.md), section 6.4: labels, then names (base name, alias, fully qualified name), then plain settings, which apply only where the value is missing or equals the built-in default. Patterns are Java regular expressions that must match the whole name, so glob-style `'UNTAR_*'` keeps its regular expression meaning.
- **Per-task values.** Closures stay unevaluated until a task reads the directive. `cpus`, `memory`, `time` and `disk` are capped by `resourceLimits` whenever they are read. Closures inside map-valued directives such as `publishDir` are cloned with the task context but not called.
- **Quirks not copied.** Aliases of one process do not share an `ext` map, and nested command-line params are not shared between top-level files ([Nextflow config](../../research/nextflow-config.md), sections 2.3 and 6.9).

**Runtime settings stay separate.** Executors, stores, the cache namespace and credentials are language-neutral, so they live in the iunctus runtime settings file, not in Nextflow config. The Nextflow driver maps a few Nextflow settings: the enabled container engine sets `workflow.containerEngine` and asks for a container executor; `executor.cpus` and `executor.memory` set local capacity; `env` values become task environment. A Nextflow `process.executor` such as `slurm` gives a warning and is ignored.

`iunctus config` prints the resolved tree, like `nextflow config`. The differential tests compare the two.

### 5.10 Module loading, params and plugins

- **Includes.** Relative paths resolve against the including file. `.nf` is added when missing, and a directory means its `main.nf`. Each file is lowered once. Each call of a process creates an invocation with its alias and fully qualified name, as Nextflow does.
- **Per-module values.** `moduleDir` is the directory of the module file. `template` files resolve in the module's `templates/` directory. The pipeline `bin/` directory is added to `PATH`. Only `bin/` scripts whose names appear in the rendered script are staged and hashed, as Nextflow hashes only referenced scripts. `nextflow.enable.moduleBinaries` adds a module's `resources/usr/bin`.
- **Params.** One global `params` map. `addParams` is not part of strict syntax.
- **Plugins.** `include { f } from 'plugin/nf-schema'` binds to a native function in `nextflow-plugins`. Any other plugin fails at load time, with the list of plugins and functions iunctus implements.

**nf-schema, natively.** 110 pipelines include it ([nf-core script inventory](../../research/nf-core-inventory.md), section 1).

| Function | What it must do | Why exactness matters |
|---|---|---|
| `validateParameters()` | Validate params against `nextflow_schema.json` (JSON Schema 2020-12 through the `jsonschema` crate), with nf-schema's formats (`file-path`, `directory-path`, `path`, `file-path-pattern`) and keywords (`exists`, `mimetype`, `schema`); honour the `validation` config scope. | Pass or fail must match. Message text is secondary. |
| `samplesheetToList(path, schema)` | Parse CSV, TSV, JSON or YAML sample sheets; cast values per schema; build `meta` maps in schema order; check files. | It creates the main input channel. Types and key order reach task scripts. |
| `paramsSummaryMap(workflow, parameters_schema:)` | Group non-default params by schema section. | Its YAML becomes a MultiQC input. |
| `paramsSummaryLog`, `paramsHelp` | Log and help text. | Cosmetic. |

The target is the nf-schema 2.x line that the corpus uses (2.1 to 2.8). The oracle can run the real plugin, if its archive is placed in the oracle's `NXF_HOME` in advance, which gives differential tests for sample sheet parsing. The older `nf-validation` plugin (16 pipelines) is out of scope at first.

### 5.11 The dataflow runtime

The runtime follows [Nextflow channels](../../research/nextflow-channels.md). It is its own small discrete event system, not a set of async channels.

**Channels.** An arena holds channels and nodes.

- A queue channel is multicast: an append-only buffer with one read cursor per subscriber. Buffers are unbounded, because Nextflow patterns rely on that. Items are dropped once every cursor has passed them.
- A value channel is unbound, bound to a value, or bound to the stop signal (an empty value).
- Binding a value channel twice with an equal value is ignored. A different value is an error.

**Nodes.** Operators and processes are nodes with input ports and output channels. A node handles one message at a time.

**Build, then ignite.** Evaluating the entry workflow only wires nodes and creates cursors. The `output` block is applied next. Then ignition runs the factories in registration order, connects topic sources, and starts processes. This follows Nextflow's two phases ([Nextflow channels](../../research/nextflow-channels.md), section 1.1).

**Scheduling.** A first-in first-out queue of nodes with pending input. The driver takes one node, lets it handle one message, and repeats until the queue is empty. Then it returns its requests to the runtime and waits for the next event. The order depends only on the program, the recorded facts and the order of events. That is what makes the driver deterministic.

**Operators.** Each operator is a Rust state machine that follows its entry in the channels specification: output kind, order, blocking, completion and error cases. Closures run through the evaluator. Operators that read files (`splitCsv`, `splitFasta`, `splitText`, `countLines`, `collectFile` with path items) read through the host. `collectFile` writes its result with `host.put`, so its files enter the store by digest. Priority follows the inventory: the 21 operators used by 29 or more pipelines first, then the rest by pipelines unlocked.

**Termination.** The run succeeds when every node has stopped and no task is running. If the scheduler queue is empty, no task is running, and some node has not stopped, Nextflow would hang. iunctus reports an error instead, naming the channels and nodes that wait and where they were created in the source.

**Errors.**

- An exception in an operator closure aborts the run, as in Nextflow.
- A task failure goes to the process error strategy (section 5.12).
- Aborting stops new tasks. With `terminate`, running tasks are cancelled. With `finish`, they may complete. Then the `onError` and `onComplete` handlers run, and the run exits with a non-zero code.

**Deterministic task identity.** A task has two identities:

- inside the run: the fully qualified process name plus the task index, which is assigned in firing order. Replayed runs assign the same indexes;
- across runs: the cache key, which depends only on content.

The attempt name for idempotent submission is derived from the run id, the task identity and the attempt number.

### 5.12 From a process invocation to a task

This follows [Nextflow channels](../../research/nextflow-channels.md), section 7, and [Nextflow config](../../research/nextflow-config.md), section 7.

1. **Firing.** One message per input per round. Value inputs are reused. The singleton rule decides whether outputs are value channels. `each` inputs build their combinations in front of the process. A stop signal on any input ends the process, and its outputs close after its last task finishes.
2. **Task context.** Inputs are bound by name. Tuples are unpacked. A `path` input gets its stage names from `stageAs`, `name` and the numbering rules for `*` and `?`, with an error on collisions. The task context holds these task paths.
3. **Directives.** Read from the resolved process settings, evaluated lazily with the task context, capped by `resourceLimits`, and converted with Nextflow's coercion rules (`cpus` 0 reads as 1, and so on).
4. **`when:`.** If false, the task counts as complete and binds no output.
5. **Rendering.** The `script:` section, or `stub:` in stub mode, is evaluated with path escaping. `template` files are rendered with the same GString rules. The result goes through Nextflow's normalisation: Groovy `stripIndent`, trim, a final newline, and the shebang from the `shell` directive. The `env` and `eval` capture code is appended. This text is the `.command.sh` that must match the oracle byte for byte.
6. **Output rules.** Output patterns are evaluated with the task context and turned into collection rules.
7. **Request.** The driver builds the hashed specification, the placement and the request information (section 4.2), with the reuse scope from the `cache` directive and the pinning state.
8. **Completion.** The driver applies the exact output rules to the manifest, reads `.command.env` and the stdout file when needed, and binds all outputs of the task together, in declaration order. Output order across tasks is completion order, or task index order with `fair`.
9. **Failure.** The error strategy closure sees `task.exitStatus` and the attempt counters. `retry` evaluates all dynamic directives again with `task.attempt` increased, and sends a new attempt. If the script does not depend on the attempt or the resources, the key stays the same. Only successful executions are cached, so the retry really runs.

Task paths render as their staged names and escape as Nextflow escapes them in task scripts. Output paths keep their original names, and their location is the store object.

### 5.13 Topics, workflow outputs, `publishDir` and completion handlers

- **Topic channels.** A process output with `topic: name` registers its channel as a source of the topic. `channel.topic(name)` returns the one shared channel of that topic. At ignition, all sources of a topic are mixed into it, and it closes when all sources are closed. A topic without sources closes at once. A process that reads a topic it also writes would hang in Nextflow, so iunctus reports it at ignition.
- **Workflow outputs.** `publish:` assignments store channels in the outputs map after the entry workflow body. The `output` block declares their targets. For each value, a publish node finds files recursively in paths, lists and map values, computes targets with the `path` directive or closure, and sends publish requests to the runtime. Files produced by tasks are copied. Other files are listed in the index only. Index files are written at completion. Errors in publishing are stored in the output result and do not abort the run, as in Nextflow.
- **`publishDir`.** Set in config by 126 pipelines. After a task's outputs are bound, the driver evaluates the `publishDir` entries (`path`, `mode`, `pattern`, `saveAs`, `enabled`) and sends publish requests.
- **Completion handlers.** `workflow.onComplete` and `workflow.onError` closures are registered while the network is built, also from inside named workflows, as nf-core does. They run after termination with the `workflow` metadata set: `success`, `duration`, `exitStatus`, `errorMessage` and the others. `getVal()` on a bound value channel returns its value. On an unbound one it is an error, because it would block forever. An error inside a handler is logged and does not change the run result.

### 5.14 Deliberate differences from Nextflow

| Nextflow | iunctus | Reason |
|---|---|---|
| A run hangs: unbound `channel.value()`, `first(criteria)` without match, legacy operators on value channels, `take(0)`, a topic read by its own writer | Error with the waiting nodes and their source positions | A hang gives no information. |
| `take(0)` writes its stop signal at call time | Closes its output at ignition | Consistent with ignition. |
| `fromPath` emits in file-tree walk order, which is unspecified | Sorted by path | Deterministic keys. |
| Work directory, `.nextflow` cache, `-resume` tied to one session | Scratch directories, the store, a cache across runs and users | [Decisions](../../decisions.md), item 7. |
| Time and random values are new on every run | Replayed on resume | Cache hits on resume. |
| `task.workDir` is the work directory | The scratch directory of the attempt | There is no work directory. |
| `fair` loses later outputs after a task that binds none | Error at that point | Silent loss. `fair` is not used in nf-core. |
| Aliases share an `ext` map; nested command-line params leak between config files | Separate copies | Research marks both as quirks not to copy. |

`iunctus run --explain-differences` prints this table with links, and every difference that a run actually hits is logged once.

## 6. Verification

Every layer has its own tests. The oracle is the reference wherever Nextflow behaviour is involved. The oracle and its test data are downloaded by the test harness. They are never part of the binary, which keeps [decision](../../decisions.md) 1.

### 6.1 Per layer

| Layer | Tests | Oracle use |
|---|---|---|
| `task-model` | Golden keys for fixed specifications. Property tests: permuted inputs give one key; any hashed field change gives a new key; placement changes keep the key. | none |
| `content-store` | One conformance suite against a local directory, MinIO and an Aruna node: digests, versions, large files, multipart uploads, missing objects. | none |
| Executors | One conformance suite of tasks on local processes, Docker, Podman, Funnel with MinIO, and Aruna: file names with spaces, quotes and non-ASCII characters; directories, empty directories and symbolic links; missing and optional outputs; exit codes and signals; time limits; cancellation; reconcile after the engine was killed. | none |
| Cache and run log | Scope rules; entry validation; two runs writing one key; torn writes at every byte offset of the last record; replay gives the same event order for the same inputs. | none |
| Evaluator | Unit tests per built-in from the research notes. Differential batches: thousands of expressions in one strict-syntax script, each printing value and class name, run by the oracle and by iunctus; lines must be equal. Generated random expression trees over typed pools of values, nightly. Every corpus file must lower without an internal error. | Batch runs of `nextflow run` on generated scripts. |
| Config | `iunctus config` versus `nextflow config` for every corpus pipeline and profile combination whose configs pass lint. Per-process directives (cpus, memory, time, container, tag) versus the trace of oracle stub runs. | `nextflow config`, stub runs with a trace file. |
| Dataflow | The experiments in [Nextflow channels](../../research/nextflow-channels.md) as tests. The documented snippets of the Nextflow repository with their expected output. Generated small operator programs: sequences compared for ordered operators, multisets for unordered ones. | Batch runs of generated programs. |
| Process to task | Byte identity of `.command.sh` with the oracle for every task of module and pipeline tests; stage names; output rules on synthetic manifests. | Stub and real runs. |
| Pipelines | nf-test snapshots (stub, then real); published file lists; MD5 of files that are stable across two oracle runs; the same keys and outputs on local Docker and on TES. | Stub and real runs. |
| Resume | Chaos tests: kill iunctus at random points and resume; results must equal an uninterrupted run, with no duplicate executions. "Edit and resume": change one `ext.args`, only affected tasks and their descendants run. | none |
| Performance | A generated pipeline with 100,000 tasks and a built-in executor that finishes at once: driver time per task, memory, log size, resume time. | none |

### 6.2 The oracle harness

The `nextflow-conformance` crate drives the oracle.

- **Module tests.** The text-only corpus has no `.nf.test` files, snapshots, `bin/`, `templates/` or `assets/`. The corpus fetch gets a second pattern set for test runs. The harness reads the common shape of nf-core module tests: the `process { """ input[0] = ... """ }` block, the `params` block, and the test config. It writes a strict-syntax harness workflow that calls the process and dumps its outputs as JSON. The same harness runs on the oracle and on iunctus.
- **Script comparison.** Both engines run the harness in stub mode, and in real mode with Docker. The harness maps tasks by process name and tag, and compares `.command.sh` byte for byte. Nextflow writes `.command.sh` in each task directory. iunctus writes rendered scripts with `--dump-scripts <dir>`.
- **Snapshots.** Output channel contents are compared with the `.snap` JSON of the test: file names, and MD5 sums for files that are stable across two oracle runs.
- **Pipelines.** The oracle runs `-profile test,docker` with and without `-stub`. The harness compares the task list, every `.command.sh`, the published file tree, and the pipeline's nf-test snapshot.
- **Test data.** Remote test data is fetched once into `~/.cache/iunctus/test-data`, keyed by URL and digest, and imported into test stores. Runs can then be offline.

### 6.3 Corpus report

A nightly job runs `iunctus check` (and `-stub` with the built-in executor) on all 134 DSL2 pipelines and the modules repository. It writes a table: pipelines that load, pipelines that complete stub runs, and the blocking features ranked by pipelines unlocked. This table drives section 5.8.

## 7. Milestones

Effort is in person-months for experienced Rust engineers. The numbers are judgement calls, like those of the feasibility report. The parser is not included, because it is done.

### M0: task layer probes (0.5 to 1 person-month)

- Submit the portable task shape (section 4.10) to Funnel with MinIO and to an Aruna node. Check wildcard outputs with `path_prefix`, the output limits, whether control files arrive after a tool failure, and last-log behaviour.
- Check which of the 100 most used nf-core container images have `bash` and `sha256sum`.
- Confirm with the oracle that the `env` and `eval` capture code is appended to `.command.sh`.
- Set up the oracle harness for one module test.

Done when the probe results are written down, and the wrapper steps of section 4.4 are confirmed or changed.

### M1: one module, end to end (3 to 4 person-months)

Scope: the nf-core FASTQC module and its tests, on local Docker. Every layer exists in a minimal form: lowering and evaluator for the constructs FASTQC and its harness use; config with params, `withName`, `ext` closures and `docker.enabled`; `channel.of`, a process call, `.out`, emit names and the `versions` topic; `task-model`; the local store with HTTPS import; the local cache index; the run log without replay; the wrapper; the local Docker executor; `iunctus run`, `-stub` and `--dump-scripts`.

Done when:

1. The FASTQC single-end and paired-end tests produce a `.command.sh` byte-identical to the oracle, in stub and in real mode.
2. The output channels match the test snapshot: file names and the versions tuple.
3. A second run reuses every task and starts no container.
4. Changing `ext.args` in the test config reruns exactly the affected tasks.
5. Killing iunctus during a run and running again leaves no broken state, and finished tasks are reused.
6. A module with an unsupported construct fails at load time with file, line and group.
7. The driver time per task is measured on a generated pipeline, as a baseline.

### M2: the same module on TES and Aruna (2 to 3 person-months)

Scope: `tes-executor`, the S3-compatible store for MinIO and Aruna, uploads by digest, reconcile, and tag-based polling.

Done when:

1. The M1 tests pass on Funnel with MinIO, and on an Aruna node used as both store and TES server.
2. A TES run reuses results that a local run created, and the other way round, when both use the same shared cache and image digests.
3. Killing iunctus during a TES run and resuming adopts the running TES tasks without submitting them again.
4. Polling uses one `ListTasks` call per interval, and version ids from Aruna appear in attempt records.

### M3: all nf-core modules (3 to 4 person-months)

Scope: the module template in full: all input and output options (`stageAs`, `arity`, `name`, `optional`, `includeInputs`, `eval`, `env`, `stdout`), `when:`, `template`, `each`, dynamic directives, config closures, and the module test harness for the whole repository.

Done when:

1. At least 95% of module test cases pass in stub mode, with `.command.sh` identical to the oracle and matching output names. The rest are listed with their unsupported feature.
2. In real mode, 100 module tests, chosen by how many pipelines use the module, match the oracle scripts and the stable snapshot MD5 sums on local Docker. 20 of them also pass on TES.
3. `iunctus check` reports no unsupported feature for at least 98% of the 2,215 module files.

### M4: the first pipeline, nf-core/demo (3 to 5 person-months)

nf-core/demo is small and lint-clean. It uses FASTQC, SEQTK_TRIM, MULTIQC and a local module, the three utility subworkflows, nf-schema, topic channels, `branch`, `groupTuple`, `collectFile`, `flatten`, `collect` and the methods description template.

Scope: full config evaluation (includes, profiles, selectors, `resourceLimits`, `publishDir`), nf-schema natively, the stand-ins of the pipeline template, the top operators, completion handlers, and publishing.

Done when, with `-profile test,docker`:

1. Stub and real runs complete, locally and on TES.
2. The task list and every `.command.sh` equal the oracle's.
3. The published file names equal the oracle's, and files that are stable across two oracle runs have equal MD5 sums.
4. The stable parts of the pipeline's nf-test snapshots pass.
5. `iunctus config` equals `nextflow config` for the non-closure values.

### M5: resume and robustness (1.5 to 2.5 person-months)

Scope: ordered replay, recorded facts, reuse scopes, reconcile for all executors, and clean-up.

Done when:

1. Chaos tests with 50 random kill points end with the same published outputs as an uninterrupted run, and with no duplicate executions.
2. After an edit of one module's `ext.args`, only affected tasks and their descendants run.
3. Order-dependent inputs (`groupTuple` bags, `collect` lists) come out in the original order on resume, so their keys match.
4. The run log survives torn writes.

### M6: breadth (5 to 8 person-months)

Scope: the remaining common operators and splitters, the `output` block, all error strategies, all publishing modes, the trace file, and the library long tail by the corpus ranking.

Done when:

1. Stub tests of at least 40 lint-clean pipelines whose configs load under strict syntax match the oracle.
2. Real test profiles of at least 20 of them pass on local Docker and on TES.
3. A support table, generated from the corpus report, is published in `docs/`.

### M7: typed syntax (2 to 3 person-months)

nf-core plans to make records and typed processes mandatory by Q2 2027 ([feasibility report](../../feasibility-report.md)). Scope: records, enums, typed processes and workflows, typed params, the typed operators including `groupBy` and the record `join` ([Nextflow channels](../../research/nextflow-channels.md), section 10).

Done when the typed examples of the Nextflow documentation run, and the first typed nf-core modules pass their tests in stub mode.

### Total

About 20 to 30 person-months. This agrees with the feasibility report: its 15 to 24 person-months for the Nextflow subset, plus the part of the shared core (store, cache, run log, executors) that this design builds first, minus the parser. With two engineers, M1 takes about two months of calendar time.

## 8. Risks and open decisions

### 8.1 Risks

| Risk | Effect | Mitigation |
|---|---|---|
| Silent drift in Groovy semantics | A task runs a different command and nothing fails. | Byte identity of every `.command.sh` with the oracle; nightly generated expression tests; the high-risk checklist from the first milestone on. |
| Nextflow keeps changing | New syntax and behaviour every release; typed syntax becomes mandatory in nf-core by Q2 2027. | Pin the reference to 26.04; move the oracle to each stable release and rerun all differential tests; M7 is planned, not optional. |
| Exact stand-ins (SnakeYAML output, template engines, JSON formatting, Java date patterns) | Text inside MultiQC inputs differs, so keys and reports differ. | Differential tests on the shapes nf-core produces; implement only those shapes, and reject the rest clearly. |
| Java regular expression dialect | A pattern matches differently. | `fancy-regex` plus a translation layer; clear errors for unsupported syntax; differential tests on all patterns in the corpus. |
| One driver thread is too slow for very large runs | Long builds and slow resume. | Measure in M1; resolved names and call-site caches; splitters off the thread with results returned in order. The ordered event stream must stay, because determinism depends on it. |
| `sha256sum` missing in an image | No digests for TES outputs. | Version identity from Aruna; lazy hashing by the runtime when a digest is really needed; the M0 image survey. |
| TES servers differ, and Aruna's route is narrow | Tasks fail on some servers. | M0 probes; server profiles with capability flags; the executor conformance suite on every server. |
| Hashing large local inputs | The first run is slow. | The stat cache; hashing while uploading; an optional metadata identity with `lineage` scope. |
| A shared cache can hold poisoned entries | Wrong results spread to other users. | Namespaces; shared scope only for trusted stores; entries record their producer; signed entries later. |
| nf-schema behaviour is only partly known | Sample sheets parse differently. | Differential tests against the real plugin in the oracle. |
| Small team, three moving languages | Nextflow falls behind. | The corpus report shows the state every night; the feasibility report's standing estimate of 0.3 to 0.5 engineers for Nextflow upkeep. |

### 8.2 Open decisions

| Decision | Options | Recommendation |
|---|---|---|
| Content digest | SHA-256; BLAKE3; both | SHA-256 only, with algorithm-tagged digests (section 4.1). |
| Where runtime settings live | A language-neutral settings file; a new scope in Nextflow config | A language-neutral file, with a small mapping from Nextflow settings (section 5.9). |
| Exit code of the wrapper | The tool's code; always 0 after writing results | Always 0 after writing results, so logs of failed tasks arrive (section 4.4). Confirm in M0. |
| Reuse rule | Cache everything; cache by completeness of the specification | By completeness, with `shared`, `lineage` and `none` scopes (section 4.7). |
| Ordered replay on resume | Build it; accept misses when order changes | Build it in M5. The cost is small because the driver is already deterministic. |
| Time and random values on resume | Fresh; replayed | Replayed, so resumed runs keep their cache hits. |
| Hangs | Reproduce; report | Report as errors (section 5.14). |
| Command-line params | Strings, as Nextflow does; convert to types | Strings, as Nextflow does. Check with the oracle whether nf-schema converts them, and warn when a string param such as `'false'` is used as a condition. |
| Utility subworkflows | Run them as code; replace them natively | Run them as code. Replace only functions that need network features, and only if needed. |
| Container runtime access | Command-line tools; Docker API | Command-line tools (section 4.10). |
| Unpinned images (offline runs) | Refuse; run with `lineage` scope | Run with `lineage` scope and a warning. |
| Location of the default store | Launch directory; user cache directory | `.iunctus/` in the launch directory, like Nextflow's `work/`; a shared store is opt-in. |
| `BigDecimal` | Own type; the `bigdecimal` crate | Own type on `num-bigint`, tested against the oracle. |
| Remote nf-core/configs include | Fetch when online; never fetch | Fetch when online, record its digest, and skip it with `--offline`, as Nextflow does with `NXF_OFFLINE`. |
| `execute()` and URL connections | Implement; reject | Reject clearly until a targeted pipeline needs them, then implement as recorded host calls. |
| nf-validation plugin | Implement; out of scope | Out of scope until the supported set of strict-syntax pipelines needs it. |
