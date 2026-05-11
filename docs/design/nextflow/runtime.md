<!-- The chosen design of the native Nextflow evaluator, dataflow runtime and shared task layer. -->
<!-- Copyright (c) 2026 The iunctus Contributors -->
<!-- SPDX-License-Identifier: MIT or Apache-2.0 -->

# Nextflow runtime design

Status: chosen design, 2026-09-27. It combines two independent proposals:

- [Proposal A](proposal-language.md) starts from the language and a small first milestone.
- [Proposal B](proposal-platform.md) starts from the task layer that all languages will share.

The two agree on most points. Where they differ, this document picks one and says why. It also applies the decisions made after they were written ([decisions](../../decisions.md), items 8 to 10): TES first, Aruna later, native S3 and file system storage, and a cache directory plus an embedded database instead of a work directory. So every mention of Aruna in the proposals is out of scope for now.

Container-only execution and Conda image preparation follow decisions 11 to 13. They replace the host execution paths in the proposals.

## 1. The design in short

1. **The task layer knows no language.** Every driver turns its program into concrete tasks. A task record has a hashed specification, a placement and an attempt record. The cache key is the SHA-256 of the canonical specification. Groovy values, channels and directives never reach this layer.
2. **The evaluator lowers the syntax tree once, then walks the lowered tree.** Lowering resolves names, repeats Nextflow's compile-time rewrites and finds unsupported features before anything runs.
3. **Values keep Groovy's runtime classes.** `Integer`, `Long` and `BigDecimal` stay apart, maps keep insertion order, and formatting follows Groovy. A difference here would change a task command without any error.
4. **The dataflow network is a single-threaded event loop.** It is deterministic for a given order of task completions. A run that would hang in Nextflow stops with an error in iunctus.
5. **Storage is a cache directory on a file system or in S3,** plus an embedded database for the state of runs. There is no work directory.
6. **Results are reused when the specification is complete.** Tasks whose image and inputs are pinned by content share results across runs, pipelines and machines. Others are only reused when a run is resumed.
7. **TES is the first executor.** A local container executor runs stub and test tasks in containers too. There is no host execution fallback.
8. **The real Nextflow 26.04.6 is the reference, in development only.** Its answers are recorded as test fixtures, so `cargo test` never needs a JVM.

## 2. Crates

Crates are named for what they are and sit at the workspace root, like `nextflow-parser`.

| Crate | What it is | Shared or Nextflow only |
|---|---|---|
| `nextflow-parser` | Syntax trees for scripts and config files (done) | Nextflow |
| `nextflow-evaluator` | Lowering, values, the interpreter, the standard library, stand-ins for Java classes | Nextflow |
| `nextflow-config` | Config files, includes, profiles, params, process selectors, per-task settings | Nextflow |
| `nextflow-plugins` | Native versions of plugin functions, starting with nf-schema | Nextflow |
| `nextflow-runtime` | Module loading, the dataflow network, operators, processes to tasks, the Nextflow driver | Nextflow |
| `task-model` | The task record, file references, canonical encoding, the cache key. No I/O. | Shared |
| `content-store` | The cache directory on a file system or in S3, imports from HTTPS | Shared |
| `executor` | The executor interface, the wrapper script, the result manifest, the TES executor and the local container executor | Shared |
| `task-runner` | The scheduler, cache lookups, the run database, resume, publishing | Shared |
| `workflow-graph` | Language-neutral tasks, the driver interface, the execution graph and the mock executors ([engine](../engine.md)) | Shared |
| `iunctus` | The binary: command line, settings, wiring | Binary |
| `nextflow-conformance` | Development harness for oracle runs and snapshot comparison. Never linked into the binary. | Development |

Rules:

- Dependencies point one way. No shared crate depends on a Nextflow crate; a workspace test checks this.
- Nextflow crates never call an executor or a store directly. They go through the driver interface of `workflow-graph` (section 5.6).
- The evaluator defines a small host trait for file access, the environment and the clock. So the evaluator and the config crate can be tested with a fake host.
- The proposals differ on crate size. This design takes Proposal B's separation of `nextflow-plugins` (it keeps the `jsonschema` dependency out of the evaluator) and Proposal A's single `executor` crate, split into modules, until a second executor family needs its own crate.

## 3. The Nextflow front end

### 3.1 Loading and lowering

Loading follows Proposal B, section 5.1: read the manifest, evaluate the config, parse the main script and all included modules, lower every module, run legacy `params.x = v` statements in load order, evaluate the entry workflow to build the network, apply the `output` block, then ignite.

Lowering follows Proposal A, sections 3.1 and 3.2. Its passes repeat the Nextflow compiler in order: names and scopes, script structure (legacy inputs and outputs become declarations, directives are marked fixed or per task), type stripping, `LangHelpers` comparisons, `branch` and `multiMap` criteria, GString to String, and the escaping of staged paths in task scripts. Every lowered node keeps its file and span. There is no bytecode; a later compiler can start from the lowered tree if profiling asks for it.

`iunctus check <pipeline>` loads and lowers a pipeline and prints every unsupported feature at once. The test-workflows report uses it.

### 3.2 Values

The value model is the same in both proposals (A 3.3, B 5.3): one variant per Java runtime class, shared mutable lists, maps and sets, Java number rules with an own `BigDecimal` type on `num-bigint`, strings measured in UTF-16 code units, Java equality and hashing for map keys and operator keys, and paths that remember where they come from.

Three relations and three string conversions are kept apart (B 5.4): Groovy equality, Java equality with hash codes, and Groovy ordering; the GString form, the Groovy `toString` form and the Java `String.valueOf` form. The 20 high-risk behaviours of the [Groovy spec](../../research/nextflow-groovy.md), section 14, become one test file.

### 3.3 Dispatch, closures and dynamic contexts

- Methods resolve along the receiver's class chain in a registry of built-ins; each entry records its argument shapes and the Groovy or Nextflow source it mirrors (B 5.5).
- Named arguments become a map in first position; a trailing closure becomes the last argument.
- Closures capture variables in shared cells. Argument adaptation (list spreading, the optional `it`, map iteration by parameter count) lives in one helper.
- Names that lowering cannot resolve are only allowed in the four dynamic contexts: per-task directives, the task sections (`when:`, `script:`, `stub:`, outputs), config closures such as `ext.args = { ... }`, and operator criteria. They resolve in the order of the [config spec](../../research/nextflow-config.md), section 7.2.

### 3.4 Standard library and unsupported features

The inventory decides the order of the library, not the documentation (A 3.8, B 5.8): first what the current milestone needs, then everything the nf-core modules use, then the three utility subworkflows with their Java stand-ins, then the long tail ranked by pipelines unlocked.

Unsupported features fail loudly in four groups (B 5.7): rejected by strict syntax, outside the scope of iunctus (Groovy classes in `lib/`, JVM plugins without a native version), not implemented yet, and deliberately different. Each error names file, line, column and group, and iunctus exits with code 33, as CWL runners do for unsupported requirements. There is no silent fallback.

### 3.5 Config

Config uses the same evaluator in a config mode. The rules are those of the [config spec](../../research/nextflow-config.md) and both proposals agree on them (A 4, B 5.9): an ordered value tree, assignment versus deep merge, file order, includes, profiles applied at the end of each `profiles` block, params precedence, selectors resolved per process invocation, and closures evaluated per task and capped by `resourceLimits`.

Runtime settings are separate (B 5.9). Executors, the store, the cache namespace and credentials live in the iunctus settings file, not in Nextflow config, because they serve every language. The Nextflow driver only maps a few Nextflow settings: the enabled container engine sets `workflow.containerEngine`, and `env` values become task environment.

### 3.6 Modules, params and plugins

- Includes, module instances, `moduleDir`, templates and the pipeline `bin/` directory follow B 5.10.
- One global `params` map; command-line params stay strings at run time, as in Nextflow, with a warning when a string replaces a boolean or number default.
- `include { f } from 'plugin/nf-schema'` binds to a native function in `nextflow-plugins`. Other plugins fail at load time.
- nf-validation 1.1.3, which nf-schema replaced, is native too, because older nf-core pipelines use it ([decisions](../../decisions.md), item 14): `validateParameters`, the summaries, `paramsHelp` and the channel factory `fromSamplesheet`, with the error messages of its everit validator.
- nf-schema is implemented natively: `validateParameters`, `samplesheetToList`, `paramsSummaryMap`, `paramsSummaryLog` and `paramsHelp`, with JSON Schema 2020-12 through the `jsonschema` crate.
- The nf-core utility subworkflows run as code, with stand-ins for the Java classes they use. Both proposals agree: this is faithful by construction and follows template changes for free.

### 3.7 The dataflow network

Both proposals describe the same network (A 6, B 5.11 to 5.13):

- Operators and processes are nodes in an arena. A queue channel is an append-only buffer with one read cursor per subscriber; a value channel is bound once.
- Building the network only wires nodes. Ignition then starts factories in creation order, connects topic sources and starts processes without inputs.
- One queue of nodes with pending input drives the loop. When it is empty, the driver hands its task requests to the runner and waits for the next event.
- A process fires one round per complete set of inputs, evaluates `when:`, directives, the task script and output declarations, renders the task and sends a request. On completion it binds all outputs of the task together.
- Retries evaluate the directives and the script again with `task.attempt` increased.
- The run succeeds when every node has stopped and no task runs. If nothing can progress while a node still waits, the run stops with an error that names the waiting nodes.

Task identity: inside a run, a task is its fully qualified process name plus a digest of the canonical encoding of its input values (A 6.10), with an occurrence number for exact duplicates. This identity does not depend on arrival order, so attempt names stay stable after a restart without replay. The task index still follows firing order for display and `task.index`.

## 4. Storage and the run database

### 4.1 The cache directory

The same layout works on a local file system and in an S3 bucket. The `content-store` crate uses the `object_store` crate for both.

```text
<store>/
  cas/sha256/<2 hex>/<62 hex>          file contents by digest: imported inputs, task scripts, engine-made files
  trees/sha256/<2 hex>/<62 hex>        directory manifests
  runs/<run id>/<task>/<attempt>/      task outputs under their original names, logs, the result manifest
  ac/v1/<namespace>/<cache key>.json   one entry per finished task: outputs, digests, scope, producer
  urls/<sha256 of URL>.json            remote files already imported, with their ETag and digest
```

- Outputs keep their original names under the attempt prefix, because `file.name`, `baseName` and the string form of a path are visible to Nextflow code (B 4.6).
- On a local file system each output is also hard-linked into `cas/`, which costs nothing. In S3, outputs stay under the attempt prefix, and the `ac/` entry maps their digests to locations.
- The `ac/` entries next to the files are the source of truth for the cache. So a store on S3 can be shared between machines and users.
- The default store is a directory in the user's cache directory (`$XDG_CACHE_HOME/iunctus/store`), so runs of different pipelines on one machine share results. TES runs need a store that the TES server can reach: an S3 bucket, or a shared file system.

### 4.2 The run database

A database file next to the local store holds everything that belongs to runs rather than to the shared cache:

| Table | Contents |
|---|---|
| `runs` | run id, lineage id, command line, program digest, config digest, params, start and end |
| `events` | the append-only run log: task requested, execution submitted, task finished, published, facts recorded, errors |
| `tasks` | task identity, process name, cache key, reuse scope, display name, dependencies (the task graph) |
| `attempts` | attempt number, executor, backend handle, state, timings, exit code, result manifest digest |
| `facts` | recorded answers to non-repeatable questions: time stamps, random values, run names, image digests |
| `index` | a local copy of `ac/` entries and a stat cache for local input files (path, size, time, inode to digest) |

The engine is SQLite through `rusqlite` with the bundled library, in WAL mode, so several iunctus processes can share one database safely. All access goes through a small internal interface with portable SQL, so Turso can replace SQLite once its access from several processes is stable ([decisions](../../decisions.md), open questions).

Writes go ahead of effects: a submission is recorded before the executor is called, and a completion is recorded before it is delivered to the driver. So the order of events in the database is the order the driver saw.

## 5. The shared task layer

### 5.1 File identity

A file is identified by its SHA-256 digest, written as `sha256:<hex>` (A 11, B 4.1). Reasons: `sha256sum` exists in almost every container, so a remote task can hash its own outputs; S3 can check SHA-256 on upload; OCI image digests use it. CWL's SHA-1 and nf-test's MD5 are computed on demand. A directory is identified by the digest of its canonical tree manifest. Local inputs are hashed once and remembered in the stat cache.

### 5.2 The task record

Proposal B, section 4.2, defines the fields. In short:

- **Hashed specification:** format version, cache namespace, the required container image pinned to a digest with its platform, the program and arguments (for Nextflow `["/bin/bash", "-ue", ".command.sh"]`), environment, inputs (relative path, kind, digest, flags; the task script is one of them), output rules as shell globs, and the files that capture standard output and error.
- **Placement:** resources, the chosen executor, queue, backend parameters, local-only extras such as container options.
- **Request information:** task identity, display name, reuse scope, attempt number.
- **Attempt record:** what happened, written by the runner.

The canonical form is JSON under the JSON Canonicalization Scheme (RFC 8785). The cache key is the SHA-256 of those bytes. Golden tests fix the keys of example specifications; any change to the encoding bumps the format version. Resources, the process name, the task index and input locations are not in the key, so a retry with more memory keeps its key and the same module call shares results across pipelines.

### 5.3 When a result is reused

This is the rule for deterministic and non-deterministic steps from the [decisions](../../decisions.md). It follows Proposal B, section 4.7: what matters is whether the specification captures everything the task depends on, not whether the tool gives the same bytes.

| Scope | When | Reused by |
|---|---|---|
| `shared` | The image is pinned to a digest, every input has a digest, and the task needs no network. | any run that can read the store |
| `lineage` | The image could not be pinned, an input is only known by metadata, the task needs the network, or the user marked the process in the settings. | only resumed runs of the same lineage |
| `none` | Nextflow `cache false`. | never |

Differences between the proposals, and the choice here:

- **When the cache is read.** Proposal A reads it only with `-resume`, as Nextflow does. Proposal B always reads it. This design follows B: reusing `shared` results by default is the point of the cache. `-resume` adds `lineage` results and ordered replay. `-cache off` runs everything again.
- Only successful executions are written. Writes are "put if absent", so the first result wins. Requests with the same key that run at the same time share one execution.
- A later verification mode can run a sample of cached tasks again and mark tasks whose outputs differ.

### 5.4 The wrapper and output collection

One Bash wrapper runs every task inside its container, on TES or through the local container executor (B 4.4):

1. Create the work and control directories and change into the work directory.
2. Export the environment.
3. Run the program with standard output and error redirected to files. Redirection happens in the wrapper, so the task shape does not depend on the server's support for the TES `stdout` and `stderr` fields.
4. Record the exit code and times in the control directory.
5. On exit code 0, collect every file that the output globs match (with `globstar`, `nullglob` and `dotglob`), plus the capture files. On failure, collect only the control and log files.
6. Write a result manifest with paths, kinds, sizes and SHA-256 digests.
7. Exit with 0 whenever steps 4 to 6 succeeded, so the server uploads the control files and logs of a failed tool. The real exit code comes from the control directory.

Step 7 must be confirmed on real TES servers in milestone M0. The driver then applies the exact Nextflow output rules to the manifest (Java glob rules, `type`, `followLinks`, `includeInputs`, `arity`, `optional`). Nextflow works the same way on its cloud executors.

`eval` and `env` outputs use capture code appended to `.command.sh`, which writes `.command.env`, as Nextflow does. iunctus writes its own capture code that produces the same `.command.env` format, rather than copying Nextflow's text (A 11). So `.command.sh` is compared byte for byte with the oracle up to the capture block.

### 5.5 Executors

The executor interface has the nine operations of the feasibility report: capabilities, prepare, submit, poll, cancel, result, logs, reconcile and cleanup (B 4.9). Errors have four kinds: tool failure, infrastructure failure, preemption and cancellation. The runner retries infrastructure failures and preemption itself; tool failures go to the driver's error strategy.

**TES first.** Built on the `tes` crate:

- one TES task per execution, with the wrapper as its executor, staged as an input from the store;
- inputs and outputs as `s3://` URLs, or `file://` URLs when the server shares a file system with the store;
- outputs through a wildcard with `path_prefix` over an outbox directory, with large directory trees packed into an archive when the server limits the number of outputs;
- tags for run, task, attempt and cache key; one tag-filtered `ListTasks` call per polling interval; the last task log as the result;
- capabilities from a probe of `service-info` at startup, with Funnel as the reference server for development and CI.

**The local container executor** uses Docker or Podman to run the wrapper inside the task image, with a scratch directory mounted for inputs and outputs. It serves local runs, stubs and workflow tests from M1. Tasks without an image or an available container executor fail before execution. Apptainer and HPC backends can follow, with the same container-only constraint.

**Image preparation** accepts an existing image, builds an image from a locked Conda environment, or uses a workflow-wide image containing the required tools. The [decisions](../../decisions.md), items 12 and 13, define image reuse and environment isolation. Preparation finishes before task submission; the resolved image digest enters the task cache key.

### 5.6 The driver interface and the host

The first version of this interface is `workflow_graph::Driver`, which the [engine](../engine.md) describes: `start`, `complete` and `finish`, with tasks run one at a time. What follows is the target design.

A driver is a synchronous state machine with `start` and `handle(event)`, each returning task requests, publish requests or the end of the run (B 4.12). It runs on its own thread, so the evaluator can use `Rc`. Host calls block that thread: reading and listing files, importing bytes into the store, asking for facts, reading the environment, logging. The scheduler, executors and storage run on Tokio in `task-runner`.

### 5.7 Resume

Resume follows Proposal B, section 4.8:

1. The new run joins the lineage of the old one.
2. Executors reconcile: they find unfinished executions by attempt name and adopt them instead of submitting again.
3. The driver evaluates the program from the start. Recorded facts are answered from the database, so time stamps in params stay the same.
4. Task requests hit the cache, wait for an adopted execution, or run.
5. Cache hits are delivered in the old completion order, so order-dependent inputs such as `groupTuple` lists come out the same and keep their keys. Replay is best effort: a divergence costs cache hits, never correctness.

## 6. Verification

Both proposals agree on the approach (A 8, B 6). The tests per layer:

| Layer | Tests | Reference |
|---|---|---|
| Values and formatting | every observed example of the Groovy spec; the high-risk checklist | batches of thousands of expressions run on the oracle, recorded as fixtures |
| Lowering | every lint-clean nf-core file lowers or fails with a named unsupported feature | the test workflows |
| Config | `iunctus config` against `nextflow config` for every pipeline and profile | the oracle |
| Operators | the experiments of the channels spec; generated small programs | the oracle; order compared exactly only where Nextflow defines it |
| Processes | `.command.sh` byte for byte up to the capture block; task counts and directives | oracle stub and real runs |
| Task layer | golden cache keys; permutation and change tests; store and executor conformance suites on a local directory, MinIO and Funnel | none |
| Resume | kills at random points; edit and resume | none |
| Modules and pipelines | nf-test snapshots | committed by nf-core |

The harness lives in `nextflow-conformance`. Oracle answers are recorded under `tests/fixtures/oracle/` of each crate with the script that recorded them; a `just` recipe records them again for a new Nextflow release. A nightly test-workflows report lists pipelines that load, pipelines whose stub runs complete, and the blocking features ranked by pipelines unlocked.

## 7. Milestones

Effort is in person-months for experienced Rust engineers. The numbers are judgement calls, like those of the feasibility report.

| Milestone | Scope | Done when | Effort |
|---|---|---|---|
| M0: probes and foundations | `task-model` with golden keys; `content-store` on a file system and MinIO; the run database; TES probes on Funnel (wrapper exit code, wildcard outputs, limits, last log); a survey of `bash` and `sha256sum` in the 100 most used nf-core images; the oracle harness for one module | probe results written down; the wrapper steps confirmed or changed | 1 to 1.5 |
| M1: one module end to end | the evaluator core and the library for GUNZIP; config with `ext` closures; one process type; the event loop; the local container executor and TES on Funnel; HTTPS test data; the cache | all six GUNZIP test cases pass: stub runs in local containers, real runs on TES; `.command.sh` equals the oracle's; a second run reuses every task; changing `ext.prefix` reruns only the affected tasks; an unsupported construct stops at load time with exit code 33 | 3 to 4 |
| M2: all nf-core modules | the full module template: all input and output options, `template`, `each`, dynamic directives; the module test harness | 95% of module test cases pass in stub mode with scripts equal to the oracle's; 100 modules pass real runs on TES | 3 to 4 |
| M3: nf-core/demo | full config, nf-schema, the utility subworkflows with their stand-ins, topics, `publishDir`, the top operators, completion handlers | stub and real runs of `-profile test` pass on TES; the task list and every `.command.sh` equal the oracle's; the stable parts of the nf-test snapshots pass | 3 to 5 |
| M4: resume and robustness | ordered replay, recorded facts, reuse scopes, reconcile, cleanup | 50 random kill points end with the same outputs as an uninterrupted run and no duplicate executions | 1.5 to 2.5 |
| M5: breadth | remaining operators and splitters, the `output` block, all error strategies and publishing modes, the long tail of the library | stub tests of 40 lint-clean pipelines match the oracle; real test profiles of 20 pass on TES | 5 to 8 |
| M6: typed syntax | records, enums, typed processes and workflows, the typed operators | the typed examples of the Nextflow docs run; typed nf-core modules pass their stub tests | 2 to 3, starting by early 2027 |

GUNZIP remains the first target (A 9). Its stub and real runs execute in containers, using the local container executor or TES. It needs most of the core: a `tuple val, path` input, an output name from a task variable, an `eval` output with a topic, `when:`, `script:` and `stub:`, a container expression, a config closure and test data over HTTPS.

The total is about 19 to 28 person-months. That agrees with the feasibility report's estimate for the Nextflow subset plus the shared task layer, minus the finished parser. These estimates predate the container-only requirement and do not include the Conda bridge.

## 8. Deliberate differences from Nextflow

| Nextflow | iunctus | Reason |
|---|---|---|
| Some runs hang forever | error naming the waiting nodes | a hang gives no information |
| `take(0)` closes its output when called | closes it at ignition | consistent with ignition |
| `fromPath` emits in file tree order | sorted by path | deterministic keys |
| work directory, `.nextflow` cache, resume tied to one session | cache directory, run database, a cache shared across runs | [decisions](../../decisions.md), items 7 to 10 |
| the cache is only read with `-resume` | `shared` results are reused by default | the point of a content-addressed cache |
| time and random values are new on every run | replayed on resume | cache hits on resume |
| process aliases share an `ext` map; nested command-line params leak between config files | separate copies | the research marks both as quirks not to copy |
| `cache 'deep'` and `'lenient'` choose how inputs are hashed | inputs are always hashed by content; the options only tune the stat cache | content identity |
| 26.04.6 `subscribe` gives nothing | gives its source channel, as up to 25.10 | pipelines written for 25.x read it, like phaseimpute ([decisions](../../decisions.md), item 14) |
| 26.04.6 has no `org.everit.json.schema` or `org.json`; a `lib/` class that imports them fails to compile ("unable to resolve class org.everit.json.schema.Schema") | stand-ins of the parts that older nf-core `lib/` classes use | these pipelines ran on the older releases that shipped the libraries ([decisions](../../decisions.md), item 14) |

`iunctus run -explain-differences` prints this table, and every difference that a run actually hits is logged once.

## 9. Open decisions

| Decision | Recommendation |
|---|---|
| Embedded database | SQLite now, behind an internal interface; Turso once its access from several processes is stable. |
| Default location of the store | The user's cache directory, so pipelines share results; a shared S3 store is opt-in. |
| Reading the cache without `-resume` | Yes for `shared` results; `-cache off` to disable. |
| Wrapper exit code | Always 0 after writing the control files; confirm in M0. |
| Capture code for `eval` and `env` outputs | Write iunctus's own code with the same `.command.env` format. |
| Command line | Accept Nextflow's `run` options (`-profile`, `-c`, `-params-file`, `-resume`, `-stub`) and `--name value` params, so nf-core instructions work. |
