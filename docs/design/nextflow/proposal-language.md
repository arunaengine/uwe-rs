<!-- Design of the native Nextflow evaluator and runtime, starting from the language and a small first milestone. -->
<!-- Copyright (c) 2026 The iunctus Contributors -->
<!-- SPDX-License-Identifier: MIT or Apache-2.0 -->

# Nextflow evaluator and runtime: a design that starts from the language

> Input to the chosen [Nextflow runtime design](runtime.md), which takes precedence where the two differ. Written before the decisions to start with TES and to leave Aruna for later.

This document designs the part of iunctus that runs Nextflow. It covers the evaluator for the Groovy subset of strict syntax, config evaluation, module loading, the dataflow runtime and the task layer below it. It builds on the finished `nextflow-parser` crate. That parser agrees with `nextflow lint` 26.04.6 on all 14,624 nf-core files.

The design starts from the language. Its first goal is to produce the same values as Nextflow, and so the same task scripts. Its second goal is a small first milestone: the nf-core module GUNZIP, with all six cases of its nf-test file, stub and real, running locally. Everything after that grows from real pipelines, in the order that the [nf-core inventory](../../research/nf-core-inventory.md) gives.

The main choices:

1. **The evaluator lowers the syntax tree, then walks the lowered tree.** The lowering repeats Nextflow's own compile-time rewrites and resolves names. So most unsupported features fail at load time, before any task runs.
2. **Values are Groovy values, with their Java runtime classes.** They stay inside the Nextflow crates. Only file references and plain data cross into the shared task layer.
3. **The dataflow runtime is a single-threaded event loop with explicit nodes.** It does not use async Rust. It is deterministic for a given order of task completions. It reports a run that cannot make progress, where Nextflow would hang.
4. **Tasks run in scratch directories and their outputs go into a content store.** The task record has the three parts of the [feasibility report](../../feasibility-report.md): hashed specification, placement and attempt. The cache is keyed on the hashed specification.
5. **Verification uses the real Nextflow 26.04.6 as an oracle, in development only.** Its answers are recorded as fixtures, so `cargo test` never needs a JVM. The nf-test snapshots that nf-core commits are a second oracle that needs no JVM at all.

## 1. Ground rules

The [decisions](../../decisions.md) apply: one native binary, strict syntax only, no Nextflow work directory, content-addressed storage with Aruna first, and core rules before the library.

The three implementer specifications are the reference for behaviour: [Groovy subset](../../research/nextflow-groovy.md), [channels](../../research/nextflow-channels.md) and [config](../../research/nextflow-config.md). This document does not repeat their rules. It decides where each rule lives in the code and how it is tested.

**Faithful by default.** When a Nextflow behaviour changes a value, a task script or the content of a channel, iunctus copies it. This includes quirks, for example that command line parameters are strings at run time. There are few exceptions, and section 11 lists them:

- A run that Nextflow would leave hanging stops with an error in iunctus.
- Quirks that the specifications mark as "not to copy" and that nf-core does not rely on are not copied. Examples are the `ext` map shared between process aliases, and nested command line params shared between config files.

**Target version.** The target is Nextflow 26.04.6, the oracle. Where the specifications chose the newer Nextflow source over 26.04.6 (for example invalid string escapes), iunctus follows the specifications, and the recorded fixtures mark the difference.

**Not covered here:** Conda and Mamba environments, Singularity and Apptainer, Wave, Fusion, HPC executors, secrets, lineage, agents and the remote module registry. They come after the milestones in section 9.

## 2. Crate layout

Crates are named for what they are, like `nextflow-parser`. They sit at the workspace root, next to it.

| Crate | What it is | Nextflow only or shared |
|---|---|---|
| `nextflow-parser` | Syntax trees for scripts and config files (exists) | Nextflow only |
| `nextflow-evaluator` | The Groovy subset: lowering, values, interpreter, standard library, program loading | Nextflow only |
| `nextflow-config` | Config files to a resolved config; process selectors; directive coercion | Nextflow only |
| `nextflow-runtime` | Dataflow network, operators, processes, task rendering, publishing, run metadata, native plugin functions | Nextflow only |
| `task-model` | The concrete task record, file references, canonical encoding, cache key. No I/O. | Shared |
| `content-store` | Files and directories by digest, task outputs, cache entries; a local directory and S3-compatible stores (Aruna, MinIO) | Shared |
| `task-runner` | Cache lookup, scheduling, the wrapper script, executors (local, Docker and Podman, TES), the event log | Shared |
| `iunctus` | The binary: command line, language detection, wiring | Binary |

Dependencies point one way. The parser is used by the evaluator. The evaluator is used by the config crate and the runtime. The runtime talks to the task runner through `task-model` types. No shared crate depends on a Nextflow crate. CWL and Snakemake drivers will later use the same three shared crates.

Module outline:

```text
nextflow-evaluator/src/
  lower/        name resolution and Nextflow's compile-time rewrites
  value/        Value, numbers, strings, lists, maps, sets, ranges, paths, units, Java hash order
  format.rs     Groovy string conversion and Java number layouts
  interpreter/  frames, statements, expressions, closures, calls, errors with source positions
  library/      methods and properties per type, static classes, stand-ins for Java classes
  regex.rs      Java regular expressions on top of fancy-regex
  program.rs    loads a main script and its includes into one program
  host.rs       the Host trait: files, environment, clock, log, remote reads, commands
nextflow-config/src/
  tree.rs       ordered config tree with assign and deep merge
  load.rs       file order, includes, profiles, params
  process.rs    selectors and process settings
  directive.rs  reading directives: coercion, defaults, resource limits
nextflow-runtime/src/
  network/      event loop, channels, nodes
  operators/    one file per operator family
  process/      firing rules, task context, rendering, output collection, retries
  workflow.rs   workflow calls, emits, .out, pipes
  topics.rs  publish.rs  metadata.rs  plugins/nf_schema/
task-model/src/     spec.rs  placement.rs  attempt.rs  digest.rs  encode.rs
content-store/src/  layout.rs  local.rs  s3.rs  ingest.rs
task-runner/src/    runner.rs  cache.rs  schedule.rs  wrapper.rs  log.rs  executor/
```

Three boundaries matter.

**Groovy values do not cross into the shared crates.** The feasibility report proposed one value model for all languages. This design shares values only where they cross the task boundary. Groovy values have Java classes, identity and mutation, and the rendering of a task script depends on all three. Forcing them into a cross-language type would lose exactly that. What crosses is a task request (rendered files, file references with digests, plain settings) and, on the way back, a task event (output files with digests, captured text).

**The evaluator does not know about the dataflow.** The config crate needs the evaluator for expressions and closures, but not channels. Channel values are handles that the runtime gives meaning to.

**Threads.** The dataflow network and the evaluator run on one thread. The task runner, executors and storage run on a Tokio runtime on other threads. They talk through channels.

The native nf-schema functions live in `nextflow-runtime` as a module. They can move into their own crate if the `jsonschema` dependency slows builds.

## 3. The Groovy-subset evaluator

### 3.1 What is evaluated: a lowered tree

| Option | Verdict |
|---|---|
| Walk the syntax tree from the parser directly | Rejected. Nextflow changes the meaning of the tree before running it, and names are resolved statically. Both would end up scattered through the interpreter. |
| Lower to a resolved tree, then walk it | **Chosen.** |
| Compile to bytecode for a small virtual machine | Rejected for now. It adds a compiler and a machine, and makes source positions harder, for speed that is not needed. |

Reasons for the choice:

1. **Nextflow rewrites the tree before it runs it.** `ScriptCompiler.convertToGroovy` applies a list of visitors: GString to String (except nested GStrings), comparisons through `LangHelpers`, type stripping, branch and multiMap criteria, dynamic directives, task script escaping ([Groovy spec, section 2](../../research/nextflow-groovy.md#2-how-a-strict-syntax-script-runs)). Each rewrite becomes one lowering pass, in Nextflow's order. Each pass can then be checked against the Nextflow source it copies.
2. **Strict syntax resolves names statically.** `nextflow lint` reports "`x` is not defined" before running. Resolving names while lowering gives the same load-time errors. The same walk finds unsupported classes and methods (section 3.9).
3. **The interpreter gets simpler and faster.** Variables become slots. The lowering knows which variables closures capture, so only those need shared cells.
4. **Speed is not the constraint.** Rendering a task evaluates a few hundred nodes. Operator closures run once per channel item. A walker over resolved nodes does millions of simple steps per second, which is far more than the largest nf-core runs need. If profiling on large pipelines shows otherwise, method lookup can be cached first.

The parser stays a tree "as written". The lowered tree lives in the evaluator. The parser needs no changes for this design.

### 3.2 The lowering passes

The passes follow the Nextflow compiler and checker:

1. **Scopes and names**, as in `VariableScopeVisitor` and `ResolveVisitor`. Each name becomes a local slot, a captured variable, a function, process or workflow, an included name, a built-in name for the current context (script, process section, config, config closure), or a class. The implicit `it` is added. An undefined name is a load-time error.
2. **Script structure**, as in `ScriptToGroovyVisitor`, `ProcessToGroovyVisitorV1` and `WorkflowToGroovyVisitor`. Process directives are classified as fixed or per task, with the exclusion list for `label`, `publishDir` and the others. Legacy input and output statements such as `tuple val(meta), path(reads, stageAs: '?/*')` become declarations. `emit:` and `topic:` names become strings. `set { x }` and `tap { x }` become assignments in the calling scope. In a `script:` or `stub:` section, an assignment to an undeclared name creates a task variable. GUNZIP needs this: its script sets `gunzip = prefix + ".${extension}"`, and its output reads `path("${gunzip}")`.
3. **Type stripping**, as in `StripTypesVisitor`.
4. **Comparisons**, as in `PathCompareVisitor`. `==`, `<` and the others use the `LangHelpers` rules, not plain Groovy.
5. **Criteria**, as in `OpCriteriaVisitor`. Closures given to `branch` and `multiMap` become criteria with labelled conditions and optional results.
6. **GString to String**, as in `GStringToStringVisitor`. A GString nested inside `${...}` of another GString stays a GString value.
7. **Task script escaping**, as in `TaskCmdXformVisitor`. Every GString in a `script:` or `stub:` section is marked, so that input files are escaped when they are the direct value of `${...}`.

A lowered expression looks like this (shortened):

```rust
enum Expr {
    Local(Slot),
    Captured(usize),
    Global(Global),          // params, workflow, task, projectDir, moduleDir, ...
    Dynamic(Symbol),         // a free name in a task or config closure (section 3.6)
    Literal(Value),
    Interpolate { parts: Vec<Part>, escape_task_paths: bool, keep_gstring: bool },
    CallFunction { function: FunctionId, arguments: Arguments },
    CallMethod { receiver: Box<Expr>, name: Symbol, access: Access, arguments: Arguments },
    CallStatic { class: ClassId, name: Symbol, arguments: Arguments },
    Property { receiver: Box<Expr>, name: Symbol, access: Access },
    Compare { operator: Comparison, left: Box<Expr>, right: Box<Expr> },
    Closure(Rc<ClosureCode>),
    // lists, maps, ranges, indexing, arithmetic, casts, instanceof, ternary, elvis
}
```

Every node keeps its source span and file, so every error names a file, line and column.

### 3.3 The value model

```rust
pub enum Value {
    Null,
    Boolean(bool),
    Integer(i32),
    Long(i64),
    BigInteger(Rc<BigInt>),
    BigDecimal(Rc<Decimal>),     // unscaled BigInt plus scale, with Java's rules
    Double(f64),
    Float(f32),
    String(Rc<str>),
    GString(Rc<GString>),        // only the nested case that survives lowering
    List(Rc<RefCell<List>>),     // kind: ArrayList, tuple, BlankSeparatedList, LinkedList, String[]
    Range(Rc<Range>),            // IntRange, NumberRange, ObjectRange, EmptyRange
    Map(Rc<RefCell<Map>>),       // kind: LinkedHashMap, record, TreeMap; insertion order kept
    Set(Rc<RefCell<Set>>),       // kind: LinkedHashSet or HashSet (Java hash order)
    MapEntry(Rc<(Value, Value)>),
    Closure(Rc<Closure>),
    Path(Rc<PathValue>),         // local, remote (s3, https) or staged inside a task
    MemoryUnit(u64),             // bytes
    Duration(u64),               // milliseconds
    VersionNumber(Rc<Version>),
    Pattern(Rc<JavaRegex>),
    Matcher(Rc<RefCell<Matcher>>),
    GroupKey(Rc<GroupKey>),
    Channel(ChannelId),          // a queue or value channel of the running network
    ChannelOut(Rc<ChannelOut>),  // the outputs of a process or workflow call
    Process(ProcessId),
    Workflow(WorkflowId),
    Class(ClassId),              // for static members, `instanceof` and `as`
    Object(Rc<dyn HostObject>),  // stand-ins for Java classes: Yaml, JsonSlurper, Date, File, ...
}
```

The rules behind it:

- **One variant, one Java class.** `getClass()`, `instanceof`, method lookup and formatting all depend on the runtime class. `1L instanceof Integer` is false, and `Integer` 1 and `Long` 1 are different map keys. Where Groovy has a family of classes with the same methods (lists, maps, ranges), one variant carries a kind field.
- **Lists, maps and sets are shared references.** `<<`, `sort()` and `get(k, default)` change the object that every holder sees, as in Groovy. The network runs on one thread, so `Rc` is enough. Reference cycles can leak until the run ends. That is acceptable for a process that lives for one run.
- **Numbers follow Java.** `Integer` and `Long` arithmetic wraps. `BigInteger` uses `num-bigint`. `BigDecimal` is a small own type on top of `BigInt`, with Java's scale rules, `toString` and Groovy's division rule. The `bigdecimal` crate has different scale and formatting rules, so it is not used. `Double` text takes the shortest round-trip digits (the `ryu` crate) and then applies Java's layout.
- **Strings are UTF-8 inside, UTF-16 outside.** `length()`, `substring` and indexing count UTF-16 code units, with a fast path for ASCII.
- **Java equality and hashing.** A `Key` wrapper implements Java `equals` and `hashCode` for every value class. Map keys, set members and the keys of `join`, `groupTuple`, `combine` and `cross` use it. A `HashSet` iterates in Java `HashMap` bucket order: hash spreading, a power-of-two table, and doubling at load factor 0.75. `toSet()`, casts to record types and the typed `collect` need this order.
- **Paths carry where they come from.** A path is a local absolute path, a remote URL, or a staged name inside a task that remembers its source. `toString()` follows [Groovy spec, section 6.4](../../research/nextflow-groovy.md#64-nextflow-types). The paths of task outputs point into the content store (section 7.7). So `name`, `baseName`, `parent`, `text` and `readLines` work as they do on work directory paths.
- **Channels are handles.** A `Channel` value is an id into the running network (section 6). The evaluator only passes it around. Methods on it go to the operator table.

### 3.4 The rules that decide task scripts

A small set of rules decides almost every character of a task script. Each lives in exactly one place:

- `format.rs` holds the two Groovy formatting functions (`FormatHelper.write` for interpolation, `FormatHelper.toString` for `+`, `join`, `println` and elements), the Java layouts of `BigDecimal`, `Double` and `Float`, and the text of `MemoryUnit` and `Duration`.
- `value/` holds Groovy truth, equality and comparison as rewritten by Nextflow, and arithmetic promotion.
- `nextflow-runtime` holds task script escaping of staged names and script normalisation (section 7.3).

The checklist of 20 high-risk behaviours in [Groovy spec, section 14](../../research/nextflow-groovy.md#14-checklist-of-high-risk-behaviours) becomes one test file, with at least one test per item.

### 3.5 Method and property dispatch

A method call `receiver.name(arguments)` is resolved at run time in this order:

1. **Null receiver.** With `?.` the result is `null`. Otherwise the few methods of Groovy's null object apply (`toString`, `equals`, `+` with a string). Anything else is the error "Cannot invoke method name() on null object".
2. **Channel or ChannelOut receiver.** The name is looked up in the operator table. The `ChannelOut` rules of [channels spec, section 5.1](../../research/nextflow-channels.md#51-common-rules) apply. Any other name is an error.
3. **Any other receiver.** The name is looked up along the receiver's class chain, most specific first. For a tuple the chain is tuple, List, Collection, Iterable, Object. For a record it is record, Map, Object. Each library entry is a Rust function `fn(&mut Interpreter, &Value, &[Value]) -> Result<Value, Error>`. It checks the argument count and classes itself. If no form fits, it returns "no match", and the lookup continues with the next class in the chain. The oracle checks that this picks the same overload as Groovy for the shapes the test workflows use.

Closures passed to library methods are always called through one helper. It implements Groovy's argument adaptation: list spreading, the optional `it`, and default parameters ([Groovy spec, section 9.2](../../research/nextflow-groovy.md)). It also implements map iteration by parameter count (section 9.3 of the same spec).

A property read `receiver.name`:

- On a map or record it is always a key lookup. So `m.class` and `m.empty` read keys, as in Groovy.
- On other objects it calls the getter `getName` or `isName` from the library table.
- On a list it reads a list property if one exists, and otherwise spreads over the elements.
- On a number it tries the unit suffixes (`2.GB`, `1.h`), and otherwise fails.

Static calls such as `Math.max(...)` and constructors such as `new java.util.Date()` resolve their class at load time. User functions also resolve at load time. They are called with the named arguments collected into a map in first position, list spreading, defaults evaluated per call, and a run-time check of typed parameters.

### 3.6 Closures, owners and delegates

A closure value holds its lowered body, the cells of the variables it captured, and an optional delegate. Captured variables are shared, so later changes are visible on both sides, as in Groovy.

Strict syntax resolves names statically, so an ordinary closure never looks at a delegate. Dynamic lookup is needed in four places only:

1. process directives evaluated per task, including those that Nextflow wraps in a closure;
2. process sections evaluated in the task context: `when:`, `script:`, `stub:` and the output declarations;
3. config closures, such as `ext.args = { ... }`, `memory = { ... }` and the `saveAs` closure of `publishDir`;
4. `branch` and `multiMap` criteria, which the lowering already rewrote.

In these places the lowering turns each free name into a `Dynamic` node. At run time a dynamic name is looked up in the order of [config spec, section 7.2](../../research/nextflow-config.md#72-evaluation-when-a-directive-is-read):

1. the task inputs, the task variables and `task`;
2. the binding of the script that defines the process: `params` (the run-time map), `workflow`, `nextflow`, `projectDir`, `moduleDir` and the others;
3. the config `env` scope;
4. the launch environment;
5. the names of the config file itself.

An unknown name fails with Nextflow's "No such variable" error. A method call without a receiver inside such a closure goes to the functions of the defining script, except `template`. Local variables captured from the enclosing code are read directly and never go through this lookup.

### 3.7 The host interface and blocking reads

The evaluator is synchronous. Every effect goes through a `Host` trait:

```rust
pub trait Host {
    fn read(&mut self, path: &PathValue) -> Result<Vec<u8>, HostError>;
    fn metadata(&mut self, path: &PathValue) -> Result<Option<Metadata>, HostError>;
    fn list(&mut self, pattern: &PathValue, options: &ListOptions) -> Result<Vec<PathValue>, HostError>;
    fn write(&mut self, path: &PathValue, bytes: &[u8], append: bool) -> Result<(), HostError>;
    fn environment(&self, name: &str) -> Option<String>;
    fn now(&self) -> Timestamp;
    fn log(&mut self, level: Level, message: &str);
    fn run_command(&mut self, argv: &[String]) -> Result<CommandOutput, HostError>;
}
```

Tests use a fake host with a fixed clock, a fixed environment and an in-memory file system. The runtime uses a host that reads local files directly and remote files through the content store. A remote read blocks the network thread until the file arrives. That is acceptable, because the network thread only runs evaluator work.

Reading a dataflow value (`getVal()`, `.val`) works when the value is already bound. The common case is the completion handler: 100 pipelines call `getVal()` on the result of `toList` there, when every channel is complete. If the value is not bound yet, the read fails with a clear error: iunctus cannot block inside the network. In the test workflows, only spatialaxe reads `.val` inside operator closures. If more pipelines need it, the operator can be suspended until the value is bound.

### 3.8 How the standard library grows

The library grows in tiers. Each tier is driven by the inventory data (`scripts.json` from the inventory tool):

1. **What the current milestone needs.** For Milestone 1 that is GUNZIP, its nf-test inputs and its config: `extension`, `baseName`, `name`, `file()`, `in` on lists, string `+`, `task.ext`, `task.process`, `workflow.containerEngine`.
2. **Everything the nf-core modules repository uses.** The load-time method check of section 3.9 runs over all module files and lists the missing names. That list must reach zero for Milestone 2.
3. **The utility subworkflows and nf-schema** for Milestone 3. These need stand-ins for Java classes: `org.yaml.snakeyaml.Yaml`, `groovy.json.JsonOutput`, `JsonGenerator.Options`, `JsonSlurper`, `GStringTemplateEngine`, `SimpleTemplateEngine`, `File`, `FilesEx.copyTo`, `java.util.Date`, `URL` for hooks, `execute()`, and `System.exit`, `getenv` and `getProperty`.
4. **The long tail,** ordered by the "own code" pipeline count.

Raw pipeline counts are a poor order on their own. The vendored utility subworkflows raise the counts of methods like `withWriter`, `delete` and `openConnection` to more than 50 pipelines, although almost no pipeline calls them in its own code. So the order uses the modules repository columns and the "own code" column.

Rules for every library entry:

- It has a unit test with the documented or observed behaviour, and oracle cases (section 8).
- A stand-in for a Java class implements only the methods the test workflows call. Any other method of that class is an unsupported-feature error.
- CI writes a coverage report: names used in the test workflows against names implemented, per type, with pipeline counts.

Templates appear in three places: the process `template` statement (44 pipelines), `GStringTemplateEngine` and `SimpleTemplateEngine`. One template reader serves all three. It turns `$x`, `${...}`, `<% ... %>` and `<%= ... %>` into lowered code, which the same interpreter runs.

### 3.9 How unsupported features fail

**At load time,** before any task runs:

- syntax outside strict syntax (the parser);
- undefined names (the lowering);
- classes outside the supported list. Calls into `lib/` classes such as `WorkflowMain` or `NfcoreTemplate` get the message that iunctus does not load Groovy classes from `lib/`;
- method names that no supported type implements. This check is cheap and catches most library gaps before a run starts;
- included plugin functions that iunctus does not implement;
- features not implemented yet, for example `exec:` sections or `nextflow.enable.types`, until their milestone.

**At run time:** a known method name called on a receiver, or with arguments, that the library does not handle.

Every unsupported-feature error names the source position and the construct, and says that Nextflow supports it but iunctus does not yet. Such errors use a distinct exit code, 33, as CWL runners do for unsupported requirements. There is never a silent fallback. A command that only loads and lowers a pipeline lists all its unsupported features at once. Users and the support matrix (section 8.6) both use it.

## 4. Config evaluation

`nextflow-config` turns config files plus command line options into a resolved config: the config tree, the final params, the declared profiles, the plugin list and the manifest. The rules are those of the [config spec](../../research/nextflow-config.md). This section names the parts.

**File order.** The home config, the project `nextflow.config`, the launch directory `nextflow.config`, then each `-c` file. `-C` replaces the list. The manifest is read first from the project config alone, with includes ignored and no profiles, to find the main script.

**One file.** Statements run top to bottom against an ordered tree. An assignment replaces the old value. A block adds a name prefix. A selector is only allowed in the `process` scope. At the end of the file, map literals become nested config maps (but not inside lists). Values are evaluator values. Closures are stored unevaluated, together with the environment of their file. Outside closures, only the config names are allowed (`params`, `projectDir`, `baseDir`, `launchDir`, `secrets`, `env()`) plus classes. Any other name is a load-time error, as in the strict parser.

**Merging files.** Each top-level file is evaluated on its own and then deep-merged: maps merge key by key, and other values are replaced. Existing keys keep their position. New keys are appended. Key order matters, because selectors apply in map order.

**Includes.** The include expression is evaluated when the statement is reached. A relative path resolves against the including file. `/dev/null` includes nothing. A constant path to a missing local file is a load-time error, even inside a profile that is not selected. The included file starts with a shallow copy of the includer's params. Its tree is deep-merged at the top level. Remote `https://` includes go through the host and are cached. All 144 pipelines include nf-core/configs this way. With `NXF_OFFLINE` set, the nf-core ternary include picks `/dev/null`, as in Nextflow.

**Profiles.** At the end of each `profiles` block, the selected profiles defined in that block run at the top level, in `-profile` order. Without `-profile` the list is `standard`. Unknown profile names are an error once all files are read.

**Params.** Each top-level file starts with the command line params and the params file only. Inside config, a command line string is converted to the type of the config default, including decimal text to a 32-bit `Float`. The final params are the merged config params, overlaid with the raw command line values, which are strings. A script `params.x = v` only fills missing names. This is faithful, so `--flag false` is truthy. iunctus prints a warning when a command line string replaces a boolean or number default.

**Plugins.** A top-level `plugins { }` block sets the plugin list. A `plugins` block anywhere else is dropped, as in Nextflow, but with a warning.

**Process settings.** The `process` scope stays an ordered map of plain keys and `withLabel:` or `withName:` keys. When a process is invoked, `process.rs` builds its settings with the algorithm of [config spec, section 6.4](../../research/nextflow-config.md#64-the-algorithm):

- matching `withLabel` selectors;
- matching `withName` selectors for the base name, then the alias, then the fully qualified name;
- then the plain settings, which only fill a directive that has no value or still holds its built-in default.

Selector patterns are Java regular expressions, anchored, and a leading `!` negates them. `*` keeps its regular expression meaning. It is not a glob. `ext` merges entry by entry. `publishDir`, `label`, `module` and `pod` replace. Every process alias gets its own copy of `ext`.

**Per-task reading.** Closures in the settings are not evaluated here. `directive.rs` supplies the coercions of [config spec, section 7.3](../../research/nextflow-config.md#73-coercion-and-defaults-when-read):

- `memory` becomes a `MemoryUnit` and `time` a `Duration`;
- `cpus` 0 reads as 1, and `maxRetries` 0 reads as 1;
- `maxErrors` 0 blocks every retry;
- `resourceLimits` is applied on every read of `cpus`, `memory`, `time` and `disk`.

The runtime evaluates closures in the task context (section 3.6). It caches each directive value for one attempt, except values inside `ext`, which it evaluates again on every read. Closures inside map-valued directives, such as the `saveAs` of `publishDir`, are bound to the task context but not called.

**Other scopes.** `env` reaches every task. At most one container engine may be enabled, and it sets `workflow.containerEngine`, which nf-core `container` expressions test. `docker.registry` prefixes image names that have no registry. `executor.cpus` and `executor.memory` size the local executor. The runtime reads `manifest.nextflowVersion`, the `trace` scope, `outputDir` and `nextflow.enable.configProcessNamesValidation`.

A command prints the resolved config in Nextflow's flat format, so that it can be compared with `nextflow config` (section 8).

## 5. Module loading

### 5.1 From a main script to a program

`program.rs` loads the main script and everything it includes into one program:

- Each file is parsed and lowered once, keyed by its canonical path.
- An include path resolves against the directory of the including file. The `.nf` suffix is optional. A directory means its `main.nf`.
- An included name must be a process, workflow or function of that file. Aliases (`A as B`) are kept. Include cycles and duplicate names are errors.
- `plugin/<name>` resolves against the native plugin registry (section 5.3). Other include sources, such as remote modules, are an unsupported-feature error for now.
- Each file keeps its own `moduleDir`, its definitions and its include table.

A process definition is a template. Each call creates an instance under its fully qualified name, for example `NFCORE_DEMO:DEMO:FASTQC`. That name is used for selectors, `task.process` and the "already used" check.

### 5.2 Params in modules

There is one params object for the run. Legacy `params.x = v` statements run at load time, in include order (included files first, depth first), before the entry workflow. They fill only names that are still missing. Reading a missing param gives Nextflow's warning and `null`. The typed `params { }` block (3 pipelines) comes with typed syntax in Milestone 6.

### 5.3 Plugins

A native plugin has a name, the versions whose behaviour it copies, and the functions it exports. Pipelines declare plugins in the config `plugins { }` block. The policy:

- **Included functions must be implemented.** Otherwise loading fails with an unsupported-feature error.
- **Plugins that only observe the run** (nf-prov, nf-co2footprint, nf-slack) and export no included function get a warning that their reports are not written.
- **nf-validation** (16 pipelines) is not supported. It is the legacy predecessor of nf-schema, and it also adds a channel factory.

**nf-schema natively.** 110 pipelines include it. The functions are `validateParameters`, `paramsSummaryLog`, `paramsSummaryMap`, `samplesheetToList` and `paramsHelp`. They need:

- JSON Schema draft 2020-12 validation through the `jsonschema` crate, plus nf-schema's own keywords and formats (`format: file-path`, `exists`, `schema`, `mimetype`, `errorMessage`);
- sample sheet reading (CSV, TSV, JSON, YAML) with type casting and `meta` fields, returning the same list of tuples as the plugin;
- the `validation` config scope.

The printed text of help, summaries and validation errors must match the plugin, because users and tests read it. The behaviour is pinned to nf-schema 2.5.1 first (55 pipelines), then 2.7.x (26). Where the output of these versions differs, iunctus follows the version that the pipeline declares.

### 5.4 The nf-core utility subworkflows run as code

`utils_nextflow_pipeline`, `utils_nfcore_pipeline` and `utils_nfschema_plugin` are ordinary Nextflow code, vendored in about 114 pipelines. They carry most of the Java use in nf-core. iunctus runs them as code, with the stand-ins of section 3.8, and does not replace them with native functions. Reasons:

- It is faithful by construction. The printed summaries and version YAML come out of the same code.
- The subworkflows change between template versions. Native replacements would need a table of versions.
- Their side effects (email through `sendmail`, Slack and Teams hooks through `URL`) only happen when the user sets the matching params. These stand-ins come last. Until then they fail with a clear unsupported-feature error.

## 6. The dataflow runtime

### 6.1 A single-threaded event loop

The network runs on one thread. Operators, processes, publishers and topic mixers are nodes. A node is a state machine. It has a handler for items that arrive on its inputs, and processes also have a handler for task events. One FIFO queue holds deliveries of the form (node, input, item). Emitting to a channel appends one delivery per subscriber.

```text
build:   run the entry workflow body; create channels, nodes and subscriptions
         apply the output block; connect topics
ignite:  factories emit; processes without inputs create their task
loop:
    deliver queued items until the queue is empty
    if every node has stopped and no task is pending: finish
    if no task is pending: stop with "the run cannot make progress" and list the waiting nodes
    wait for the next event from the task runner and hand it to its process node
finish:  run the completion handlers
```

Why this shape and not async tasks:

- **It matches the semantics.** Nextflow gives every subscriber an unbounded buffer, lets a single-input operator handle one message at a time in order, multicasts queue channels, and binds value channels once ([channels spec, section 1](../../research/nextflow-channels.md#1-execution-model)). A FIFO of deliveries does all of this without futures, wakers or locks.
- **It is deterministic.** For a given order of task completions, every operator sees the same order. Nextflow's order depends on thread timing. The order iunctus produces is one that Nextflow can also produce. Operators that preserve order in Nextflow preserve it here too.
- **Hangs become errors.** When the queue is empty and no task is running, nothing can change any more. Nextflow waits forever in this state, for example for `first` with no match or for a process that reads the topic it writes ([channels spec, section 9](../../research/nextflow-channels.md#9-hang-error-and-silent-drop-checklist)). iunctus stops with an error that names the nodes and what they wait for.
- **The evaluator stays a plain function call.** Parallelism comes from tasks, which run in the task runner.

### 6.2 Build and ignite

The build phase follows [channels spec, section 1.1](../../research/nextflow-channels.md#11-build-first-then-ignite). The entry workflow body runs once. Factory, operator and process calls only wire nodes. Subscriptions are created at call time. Value channels bound during the build (plain process arguments, plain values emitted by a workflow) keep their value for late readers. At ignition, topics are connected first, then factories emit in the order they were created, then processes without inputs create their single task.

One deliberate difference: `take(0)` closes its output at ignition instead of at call time. In Nextflow the early close makes downstream operators hang.

### 6.3 Channels

- **Queue channel:** a list of subscribers and a closed flag. Items are values or the stop signal. A subscriber only sees items written after it subscribed.
- **Value channel:** unbound, bound to a value, or bound to stop (empty). A late subscriber gets the bound value at once. Binding a second, different value is an error.
- **Buffers** are unbounded, as Nextflow requires. If large runs need it, buffers can spill to disk later.
- **Null and VOID** follow each operator's own rule. Some operators treat a `null` result like VOID and some do not ([channels spec, section 0.3](../../research/nextflow-channels.md#03-terms)).

### 6.4 Operators

Each legacy operator is one node type. It follows [channels spec, section 5](../../research/nextflow-channels.md#5-legacy-operators): the output kind rules, the null rules, which inputs it reads when, and its errors. Keys of `join`, `groupTuple`, `combine` and `cross` use the Java `Key` of section 3.3. So `Integer` 1 and `Long` 1 do not match, as in Nextflow.

Operators are implemented in inventory order. First come the operators each milestone target needs, then the 21 operators used by 29 or more pipelines, then the rest by pipeline count. Splitters read file contents through the host. `collectFile` writes its files into the content store, and with `storeDir` also copies them there. The typed core operators come in Milestone 6.

### 6.5 Processes and how tasks form

A process call creates one process node. It follows [channels spec, section 7](../../research/nextflow-channels.md#7-processes-how-tasks-are-formed):

- **Inputs.** One buffer per input, plus the hidden control input. A value input is read again for every round. A process is a singleton when none of its inputs is a queue and it has no `each` input. `each` inputs get an iterator node in front, which emits the combinations with the first `each` varying fastest. A stop signal on any input ends the rounds.
- **A round fires.** The task gets the next task index, in round order. The node then:
  1. binds the tuple elements to their names, with `path` inputs as staged paths;
  2. evaluates `when:`; if it is false, the task counts as done and binds nothing;
  3. builds the task config from the process settings (section 4);
  4. evaluates `script:`, or `stub:` in a stub run, which sets the task variables;
  5. evaluates the output declarations: file patterns, `eval` commands and `env` names;
  6. renders the task (section 7.3) and sends a task request to the runner.
- **A task succeeds.** Its outputs are collected (section 7.6). All outputs of the task are bound together, in declaration order. Across tasks the order is the order of completion. Optional outputs follow the rules of section 7.8 of the channels spec, including the tuple rule. Outputs with `topic:` also feed their topic. `publishDir` requests go to the runner.
- **A task fails.** The error strategy is read with `task.exitStatus` set. `terminate` stops the run, `finish` waits for running tasks, and `ignore` goes on. `retry` creates a new attempt: `task.attempt` goes up, and the directives and the script are evaluated again. `maxRetries` and `maxErrors` use the readings of section 4.
- **The process stops** when it has received a stop signal and no task is pending. Then its queue outputs get the stop signal, and unbound value outputs become empty.
- **`maxForks`** limits the running tasks of one process. The runner limits the whole executor.

### 6.6 Workflows

Calling a named workflow runs its body at once, during the build, with `take:` names bound to the arguments. A `ChannelOut` argument is spread. The result is a `ChannelOut` of the `emit:` values, where plain values become value channels. Process instances inside get the workflow's name as a prefix. `.out`, `.out.name`, `.out[i]`, the pipe `|` and `&` follow [channels spec, section 6](../../research/nextflow-channels.md#6-pipe--and--set-tap-and-process-output-assignment). A second call of the same process in one scope is an error.

### 6.7 Topic channels

A topic registry is filled during the build. Every process output with `topic: name` is a queue channel and a source of that topic, from any workflow depth. At ignition, each topic channel becomes a mix of its sources. It closes when all sources have closed. A topic without sources closes at once. Items arrive in task completion order, as in Nextflow. A process that reads a topic it writes can never finish. The no-progress check reports this case instead of hanging.

### 6.8 Publishing

**`publishDir`** runs after a task's outputs are bound. The runner copies from the content store. The modes map as follows:

| Mode | What iunctus does |
|---|---|
| `copy` | copy from the store |
| `link` | hard link from a local store |
| `symlink`, `rellink` | link to the stored file |
| `move` | copy, with a warning, because stored files are never moved |

`pattern`, `saveAs`, `enabled` and `overwrite` behave as in Nextflow.

**The `output { }` block and `publish:`** (12 pipelines) create publisher nodes that follow [channels spec, section 8.5](../../research/nextflow-channels.md#85-publish-and-the-output-block): the path directive, index files, and errors that are stored instead of stopping the run. Only files that tasks produced are copied. Other files are recorded as they are, as Nextflow does for files outside its work directory.

### 6.9 Termination and errors

- **Success:** every node has stopped and no task is pending.
- **An exception in an operator closure** stops the run, as in Nextflow. Running tasks are cancelled.
- **A failed task** follows its error strategy (section 6.5).
- **Publishing errors** do not stop the run.
- **No progress:** the run stops with an error that lists each waiting node, its source position and the input it waits on.

The error report names the process, the tag, the exit status, the tail of `.command.err` and the scratch directory. Nextflow users look for the same things. The completion handlers run in every case.

### 6.10 Deterministic task identity

A task's identity is the SHA-256 of two things: the fully qualified process name, and the canonical encoding of the input values of its round. The encoding writes a class tag and the content of every value. Maps keep insertion order. Numbers keep their class, so `Integer` 1 and `Long` 1 differ, as Java keys do. Files are encoded as their content digest and staged name. The identity therefore does not depend on arrival order. The task index still follows round order, as in Nextflow. It is only used for display and for `task.index`.

The identity names a task in logs, in the event log and in the trace. The cache key is a different digest (section 7.8).

### 6.11 Run metadata and completion handlers

The `workflow` object exposes the properties that nf-core reads: `start`, `runName` and `sessionId` from the host, `profile`, `containerEngine`, `manifest`, `projectDir`, `launchDir`, `commandLine`, `success`, `duration`, `stats`, `errorMessage`, `exitStatus`, `toMap()` and the others. `workflow.onComplete` and `workflow.onError` handlers are collected, including those registered inside named workflows, as nf-core does. They run in order after the loop ends. The trace file is written, because nf-core enables it in every pipeline. The HTML report, the timeline and the DAG file are not written at first. A warning says so.

## 7. The task layer

### 7.1 From a round to a running task

1. The process node renders the task and resolves each input file to a content digest (section 7.5).
2. It sends a task request to the runner. The request holds the hashed specification, the placement, where each input can be read, and the task identity.
3. The runner computes the cache key. On a hit it answers at once with the recorded outputs. On a miss it schedules the task.
4. The executor stages the inputs into a scratch directory, runs the wrapper, and collects the outputs into the content store.
5. The runner writes the cache entry and a line in the event log, then sends the task event back to the network.

### 7.2 The task record

```rust
/// What is computed. The cache key is the SHA-256 of its canonical encoding.
pub struct TaskSpec {
    pub format: u32,                        // version of this record and of the wrapper
    pub image: Option<Image>,               // name, and digest when known
    pub shell: Vec<String>,                 // from the `shell` directive: ["/bin/bash", "-ue"]
    pub script: Digest,                     // the rendered .command.sh
    pub environment: Vec<(String, String)>, // config `env` scope, PATH for bin/ directories
    pub host_setup: Option<String>,         // beforeScript, which runs outside the container
    pub container_options: Vec<String>,
    pub inputs: Vec<InputFile>,             // sorted by path in the task directory
    pub outputs: Vec<OutputRule>,           // glob, type, optional, hidden, follow links, include inputs
    pub namespace: Option<String>,          // a cache salt, empty by default
}
pub struct InputFile { pub path: String, pub kind: FileKind, pub digest: Digest, pub size: u64, pub stage: StageMode }

/// Where and how big. Not hashed.
pub struct Placement { pub cpus: u32, pub memory: Option<u64>, pub disk: Option<u64>, pub time: Option<u64>,
                       pub accelerator: Option<Accelerator>, pub queue: Option<String>,
                       pub backend: BackendChoice, pub parameters: Vec<(String, String)> }

/// Where each input can be read. Not hashed.
pub struct Staging { pub sources: Vec<(Digest, Location)> }

/// What happened. Not hashed.
pub struct Attempt { pub identity: Digest, pub key: Digest, pub number: u32, pub handle: Option<String>,
                     pub state: State, pub exit_status: Option<i32>, pub started: Option<Timestamp>,
                     pub finished: Option<Timestamp>, pub outputs: Vec<OutputFile>, pub logs: Logs }
```

| Part of a Nextflow task | In the cache key? | Why |
|---|---|---|
| Rendered `.command.sh`, including the output capture block | Yes | It is the command. |
| Container image | Yes | It decides the tools. No image is also a value. |
| Environment, `beforeScript`, `containerOptions`, `shell`, stage mode | Yes | They can change what runs. |
| Inputs: path in the task directory, type, content digest | Yes | Content, not location, decides the result. |
| Output rules | Yes | They decide which files form the result. |
| The pipeline `bin/` directory and module binaries | Yes, as directory inputs | Scripts call them. A change must invalidate. Nextflow does not hash them. |
| `cpus`, `memory`, `time`, queue, accelerator | No | They only enter the key through the script text, for example `--threads ${task.cpus}`. |
| Process name, task index, attempt number | No | Two calls that compute the same thing may share the result. |
| Source locations of inputs | No | A file moved to another bucket is still the same input. |

The record has no Nextflow-only field. `host_setup` is also where Snakemake's `envmodules` or an HPC module load would go later.

### 7.3 Rendering `.command.sh`

1. Take the `stub:` block in a stub run if the process has one, else `script:`. A `template` statement renders the template file with the task context.
2. Normalise as `TaskProcessor.normalizeScript` does: Groovy `stripIndent(true)`, trim, one final newline, and the shebang from the `shell` directive ([Groovy spec, section 6.7](../../research/nextflow-groovy.md#67-task-scripts)).
3. If the process has `env` or `eval` outputs, append the output capture block, as Nextflow does. A one-process stub run with the oracle on 2026-09-27 confirmed two facts. Nextflow 26.04.6 appends this block to `.command.sh`, and the block writes `.command.env`. It does so in stub runs too, so `eval` commands such as `gunzip --version` run in stub mode and need the tool in the container.

The text before the capture block must be byte-identical to Nextflow's. The capture block must produce the same `.command.env` format. Whether iunctus copies Nextflow's capture text word for word is an open decision (section 11).

Inside the task directory iunctus uses Nextflow's file names: `.command.sh`, `.command.env`, `.command.out`, `.command.err`, `.command.log` and `.exitcode`. Some module scripts and many users read them.

### 7.4 The wrapper

`task-runner` generates a small Bash wrapper. It is the same for all languages. It:

1. changes into the task directory and exports the environment, including `NXF_TASK_WORKDIR` for compatibility;
2. runs the host setup outside the container;
3. runs `.command.sh` with the task shell, inside the container when there is an image. Standard output goes to `.command.out`, standard error to `.command.err`, and both to `.command.log`;
4. writes the exit code to `.exitcode`;
5. runs `afterScript`;
6. for remote executors, writes a manifest of the files in the task directory.

Nextflow's `.command.run` is not copied. Most of it stages and unstages files, which iunctus does outside the wrapper for local runs.

### 7.5 Staging inputs without a work directory

Each attempt gets a fresh scratch directory under a scratch root. The default root is the system temporary directory, and config can change it. Inputs are placed with the task's `stageInMode`. The default is a symbolic link, as in Nextflow. `link`, `copy` and `rellink` are also supported. Names follow `stageAs` and `name` patterns with Nextflow's rules for `*`, `?` and `?/*` numbering. A name collision is an error.

Inputs come from three kinds of places:

- **Task outputs** already in the content store.
- **Local files** such as `file(params.input)`, used in place. Their SHA-256 is computed once and remembered in a local index keyed by path, size, modification time and inode.
- **Remote files** (`https://`, `s3://`), fetched once into the content store. The store remembers each URL with its validators (ETag, Last-Modified) and digest. nf-core test data comes from `https://raw.githubusercontent.com/...`, so Milestone 1 needs this.

With a container, the scratch directory is mounted at the same path, and the directories that the links point to are mounted read-only.

### 7.6 Collecting outputs

After exit code 0, iunctus matches the output rules in Rust, on the scratch directory. It uses Nextflow's glob semantics: Java NIO globs, hidden files, `**`, file type, `maxDepth`, `followLinks`, `includeInputs` and `arity`. A missing required file fails the task with Nextflow's "Missing output file(s)". A missing optional output binds nothing. Matched files move or hard-link into the attempt area of the store, with their digests recorded in a manifest. Values of `eval` and `env` outputs come from `.command.env`, and a `stdout` output comes from `.command.out`. The scratch directory is then removed. It is kept after a failure, and on request.

### 7.7 The content store

```text
<store>/
  objects/sha256/<first two hex digits>/<rest>   files from the host or the web, by content digest
  runs/<run id>/<task identity>/<attempt>/       one attempt: outputs at their task paths, logs, manifest
  cache/<cache key>.json                         a cache entry: the attempt and its output digests
  urls/<sha256 of the URL>.json                  remote files already fetched, with their validators
```

The same layout works on a local directory and on an S3-compatible store (Aruna, MinIO) through `object_store`. On Aruna, manifests also record the object version ids. The local store defaults to a directory in the launch directory. `-w` and `workDir` set its location, so existing habits keep working.

Output paths in the network point into `runs/...`. So they have real names and parents and can be read, like work directory paths in Nextflow. Splitters and `file.text` work on them. A string of such a path that ends up inside a file stays readable on the same machine, as it would with a local Nextflow work directory.

### 7.8 Cache and resume

- **Key:** the SHA-256 of the canonical encoding of the hashed specification.
- **Write:** every successful attempt writes a cache entry. The entry is written last, and atomically, so a partial attempt is never visible.
- **Read:** with `-resume`, as in Nextflow. An option turns on reading in every run, for a shared cache.
- **`cache false`** neither reads nor writes. `cache 'deep'` and `cache 'lenient'` behave like the default, because inputs are always hashed by content. The documentation says so.
- **Equal keys in one run** run once. Each task still appears as its own task in the trace and in `workflow.stats`.
- **Resume is a new run of the network with cache hits,** as in Nextflow. Driver state is not replayed, because keys depend on content, not on order. One limit is shared with Nextflow: an order that differs upstream can change a later key, for example `collect` after `mix` without sorting.
- **Deterministic and non-deterministic steps.** The first recorded result of a key wins and is reused. Downstream keys depend on the output digests, so a result that differs would show up as new downstream keys. The namespace salt drops poisoned entries. A later verification mode can run a sample of cached tasks again, compare digests, and mark tasks that are not deterministic.
- **Sharing across pipelines.** The process name is not in the key. So the same module call on the same input, with the same image and arguments, shares its result across pipelines.

### 7.9 Executors and storage

The executor interface is the nine operations of the feasibility report: capabilities, prepare, submit, poll, cancel, result, logs, reconcile and cleanup. The executors come in this order:

1. **Local.** Processes started with `process-wrap`. Scheduling counts CPUs and memory against `executor.cpus` and `executor.memory`, as Nextflow's local executor does, and respects `queueSize`.
2. **Docker and Podman,** through their command lines. The command copies the options of Nextflow's `docker run`, so nf-core's `docker.runOptions` (for example `-u $(id -u):$(id -g)`) and `containerOptions` work as written. The command line is used instead of an API crate, because nf-core configs pass raw command line options.
3. **TES,** through the `tes` crate, with the portable task shape of the feasibility report. It has one executor, the wrapper staged as an `s3://` object, explicit inputs with types, the log, exit code and manifest files as outputs, tags for run, task, attempt and cache key, polling by tag, and the last task log as the result.

Aruna comes first as the S3-compatible content store, then as a TES target. Singularity, Apptainer and Slurm come after the milestones.

### 7.10 The event log

Each run writes one JSON lines file into the store. It records: run start (command line, config digest, params), task submitted (identity, key, attempt, executor handle), cache hit, task finished (state, exit status, output digests), publish actions and run end. It has four uses:

- reconciling after a crash, by adopting remote tasks by tag instead of submitting them again;
- provenance, and later a Workflow Run RO-Crate;
- the trace file;
- a command that shows the history of a run.

## 8. Verification

### 8.1 Principles

- The oracle is Nextflow 26.04.6 (`~/.cache/iunctus/oracle/nextflow-26.04.6-all.jar`, Java 21). It is a development tool. It is never needed at run time, and never needed by `cargo test`.
- Oracle answers are recorded as fixtures, with the script that recorded them, under `tests/fixtures/oracle/` of each crate. A `just` recipe records them again. Upgrading the oracle means recording again and reviewing the difference.
- The nf-test snapshots in nf-core repositories were produced by real Nextflow in nf-core's CI. They are an oracle that needs no JVM.
- Every layer also has plain unit tests, taken from the observed examples in the three specifications.
- Oracle runs use an isolated `NXF_HOME` and `NXF_OFFLINE=true`, as the parser's test-workflows tooling already does.

### 8.2 Tests by layer

| Layer | Unit tests | Oracle or test-workflows check |
|---|---|---|
| Values and formatting | Every observed example in the Groovy spec; the 20-item checklist | Expression cases run by the oracle in batches and recorded: rendered text, runtime class, error class |
| Lowering | Name resolution; each rewrite pass | Lower every lint-clean nf-core file. The only allowed failures are named unsupported features. |
| Standard library | One or more tests per method | Oracle cases generated from the call shapes in the test workflows; the coverage report |
| Config | Experiments 1 to 33 of the config spec as tests | `nextflow config` output for all 144 pipelines with the default, `test` and `test,docker` profiles; per-task directives from the trace of oracle stub runs |
| Operators | The experiments of the channels spec; the `docs/snippets` examples of Nextflow with their `.out` files | Random inputs through both. Order is compared exactly for order-preserving operators, and as a multiset for the others. |
| Processes and rendering | Firing rules, `each`, optional outputs, retries | Oracle stub runs: every `.command.sh` compared byte for byte up to the capture block; task counts and directives from the trace |
| Task layer | Staging names, glob matching, wrapper, cache, store | Glob cases against the oracle; executor tests locally, with Docker, and on Funnel with MinIO |
| Modules | | The nf-test snapshots of the module tests |
| Pipelines | | The nf-test snapshots of the pipeline tests, stub first, then real |

### 8.3 The expression oracle

A generator writes a strict-syntax script with one `println` per case. Each line prints the label, the rendered value and `getClass().getSimpleName()`, inside `try`/`catch`. This is the method the Groovy spec already used. One oracle run handles thousands of cases, so the cost of starting the JVM does not matter. The cases come from:

- the observed tables of the Groovy spec;
- every GString interpolation shape in nf-core process scripts (6,614 shapes), filled with sample values of each kind;
- generated combinations of value kinds with operators and methods. These focus on the risky areas: `BigDecimal` division, `Double` text, `MemoryUnit` and `Duration` text, ranges in collections, map keys, and hash set order.

### 8.4 Rendered scripts from stub runs

For each module test case and each pipeline test profile, the oracle runs with `-stub`. A script collects `work/*/*/.command.sh` and the trace. Tasks are matched by process name and input file names. The `.command.sh` text is compared up to the capture block. The tag, `cpus`, `memory`, `time` and `container` values are compared from the trace. Real (not stub) runs are compared for a sample.

### 8.5 nf-test snapshots

nf-test files are Groovy code, not strict Nextflow. A small Groovy script on the oracle JVM reads each `.nf.test` file and writes its test cases as JSON: name, process or workflow, script path, the `when` block, params, config file and options such as `-stub`. The JSON is recorded as a fixture.

A harness in `nextflow-runtime/tests/` then, for each case:

1. builds the test workflow: it includes the module and binds `input[0]`, `input[1]` and so on from the `when` block;
2. runs it with iunctus and records every item on every output channel;
3. renders the items in nf-test's snapshot format: numbered and named channels, maps as objects, files as `name:md5,<hex>`;
4. compares the result with the committed `.snap` file.

At first the harness supports `snapshot(process.out).match()` and `snapshot(workflow.out).match()`. Other `then` shapes are skipped and counted. For pipeline tests, the harness implements the helpers of the `nft-utils` plugin that nf-core pipeline tests use (`getAllFilesFromDir`, `removeNextflowVersion`) and the `.nftignore` rules.

Runs without containers compare everything except the values of `eval` outputs, which report tool versions. A rotating sample runs with Docker and compares everything.

### 8.6 The support matrix

CI publishes the counts per test-workflows revision:

- files that lower;
- pipelines whose config evaluates;
- module test cases that pass, stub and real;
- pipeline test cases that pass.

Every failure is classified as a named unsupported feature or as a bug.

## 9. Milestones

| Milestone | Scope | Effort |
|---|---|---|
| 0. Evaluator core | Lowering passes, value model, numbers, formatting, truth, equality, arithmetic, closures, functions, GStrings, ranges, indexing; the library for GUNZIP; the expression oracle with fixtures | 2 to 3 person-months |
| 1. GUNZIP end to end | Program loading, the config the module test needs, one process type, the event loop, local and Docker executors, local store, `https` inputs, cache, resume, the nf-test harness | 2 to 3 person-months |
| 2. The nf-core modules | The library and operators the modules repository uses, templates, `each`, `env` and `stdout` inputs and outputs, `stageAs`, `arity`, test data cache | 3 to 4 person-months |
| 3. nf-core/demo | Full pipeline config, utility subworkflows with Java stand-ins, native nf-schema, topics, `collectFile`, `publishDir`, trace, completion handlers | 2 to 3 person-months |
| 4. Breadth | More operators and library by test-workflows count, splitters, `exec:`, the output block, pipelines from Git, performance | 4 to 6 person-months |
| 5. TES and Aruna | S3 content store, TES executor, reconcile, server profiles | 2 to 3 person-months, in parallel with Milestone 4 |
| 6. Typed syntax | Records, typed processes and workflows, the `params { }` block, typed core operators | 2 to 3 person-months, starting by early 2027 |

The total is about 17 to 25 person-months. That is in line with the feasibility report's 15 to 24 person-months for the Nextflow subset, plus the TES work and the typed syntax.

**Milestone 0 is done when:**

- every observed example of the Groovy spec passes as a test;
- at least 5,000 recorded oracle cases pass, for both rendered text and runtime class;
- every lint-clean file of the nf-core modules repository lowers, or fails only with named unsupported features. That list becomes the work list of Milestone 2.

**Milestone 1 is done when:**

- All six test cases of GUNZIP's `tests/main.nf.test` (four real, two stub) run. With Docker or Podman, every snapshot equals `main.nf.test.snap`. Without a container, everything except the gzip version equals the snapshot. The snapshot holds gzip 1.13 from the container.
- For each case, `.command.sh` equals the oracle's up to the capture block. This covers the `ext.prefix` closure from the test config and the escaping of staged names.
- A second run with `-resume` runs no task and gives the same snapshot. Changing `ext.prefix` runs only the affected task again.
- A copy of the module with one unsupported construct stops at load time with its source position and exit code 33.

GUNZIP is the right first target. It is small, and its tool exists on every machine. Yet it needs most of the core:

- a `tuple val, path` input;
- a `path` output whose name comes from a task variable;
- an `eval` output with `topic: versions`;
- `when:`, `script:` and `stub:`;
- a container expression that tests `workflow.containerEngine`;
- a config closure for `ext.prefix`;
- test data over `https`.

**Milestone 2 is done when:**

- at least 95% of the module test cases whose shape the harness supports pass in stub mode, with full comparison for the container sample;
- at least 90% of the subworkflow stub cases pass;
- `.command.sh` agrees with the oracle for every passing case;
- real runs pass for 100 modules with small test data;
- every remaining failure is a named unsupported feature or a filed bug.

**Milestone 3 is done when:**

- nf-core/demo passes its nf-test snapshots with `-profile test,docker`, in real and stub mode. Demo runs FASTQC, SEQTK_TRIM, MULTIQC and a local module, and it uses the whole nf-core template: nf-schema, the utility subworkflows, topics, `branch`, `groupTuple`, `collectFile` and MultiQC's methods description template.
- The config iunctus resolves agrees with `nextflow config` for all of demo's profiles, and for the `test` profile of every pipeline whose config the strict parser accepts.
- `--help`, a validation error and the parameter summary print the same text as the oracle with nf-schema 2.5.1.

**Milestone 4 is done when:**

- stub snapshots pass for 30 lint-clean pipelines, then for 80% of all lint-clean pipelines;
- the test profiles of 10 pipelines, rnaseq among them, pass with real runs;
- the support matrix lists every remaining gap.

**Milestone 5 is done when:**

- the GUNZIP tests and the demo test profile pass on Funnel with MinIO and on an Aruna v3 node;
- a local run followed by a TES run on the same store reuses every cached task;
- stopping iunctus during a TES run and resuming adopts the running tasks instead of submitting them again.

**Milestone 6 is done when** nf-core modules that have moved to typed syntax pass their tests, and the typed operators agree with recorded oracle fixtures. nf-core makes typed syntax mandatory by Q2 2027, so this milestone must start by early 2027, even if Milestone 4 is not finished.

## 10. Risks

| Risk | Effect | Mitigation |
|---|---|---|
| Silent drift in values | A task runs a different command, and nothing fails | Oracle fixtures for values; byte comparison of `.command.sh`; no silent fallbacks |
| Java behaviour that is hard to copy: hash set order, the regex dialect, `BigDecimal` division, `Double` text, UTF-16 strings | Wrong order or text in rare cases | One module per behaviour, with dense oracle cases. Regex patterns are translated to `fancy-regex`, and constructs the translator does not know fail with an error. |
| The long tail of library methods and Java classes | Pipelines fail one method at a time | The load-time method check; the coverage report; stand-ins limited to the methods the test workflows call |
| A moving target: monthly edge releases, typed syntax mandatory by Q2 2027, many nf-schema versions | Pipelines change under the design | Pin the oracle; record fixtures again for each stable release; start Milestone 6 by early 2027; pin nf-schema behaviour per version |
| Test data and containers make CI heavy | Slow or flaky CI | Cache test data and images; run stub cases without containers, except `eval` values; rotate a container sample |
| Order that differs between Nextflow and iunctus where Nextflow allows any order | Comparisons fail for no reason | Compare as multisets where the channels spec says the order is not defined; keys depend on content, not order |
| Memory use of unbounded buffers on large runs | Large runs run out of memory | Measure on the rnaseq full test; spill buffers to disk if needed |
| Blocking reads inside operator closures | spatialaxe fails | A clear error for now; suspend the operator if more pipelines need it |
| TES output shape on Aruna, which has no directory outputs | TES runs cannot collect outputs | Decide with the TES test matrix of the feasibility report: wildcard outputs with `path_prefix`, or an archive |
| Text reused from Nextflow or nf-core (the capture block, snapshots) | License questions | Write iunctus's own capture block unless review allows reuse; snapshots are test data only and are never shipped |

## 11. Open decisions

| Decision | Options | Recommendation |
|---|---|---|
| What the evaluator runs | Syntax tree; lowered tree; bytecode | A lowered tree with a tree walker (section 3.1). |
| How the network runs | Async tasks on Tokio; a single-threaded event loop | The event loop (section 6.1). |
| Faithful behaviour or corrected behaviour | Copy every quirk; fix quirks | Copy everything that changes values, scripts or channel content. Turn hangs into errors, close `take(0)` at ignition, and do not copy the shared `ext` map or the shared nested params. List each difference in the user documentation. |
| Command line params at run time | Strings, as in Nextflow; typed values | Strings, with a warning when a string replaces a boolean or number default. |
| The nf-core utility subworkflows | Run them as code; replace them natively | Run them as code, with stand-ins for Java classes (section 5.4). |
| Digest algorithm | SHA-256; BLAKE3 | SHA-256 for file digests and cache keys. `sha256sum` exists in nearly every container, so remote tasks can hash their own outputs, and S3 supports SHA-256 checksums. The speed gain of BLAKE3 matters less than that. |
| When the cache is read | Only with `-resume`; always | Only with `-resume` by default, as in Nextflow. An option reads it always, for a shared cache. |
| The output capture block | Copy Nextflow's text (Apache-2.0); write iunctus's own | Write iunctus's own block that produces the same `.command.env`, and compare `.command.sh` up to the block. Copy the text only if the license review allows it and full byte comparison turns out to matter. |
| Collecting TES outputs | Directory output; wildcard with `path_prefix`; an archive; a helper binary inside the task | A directory output where the server supports it, and a wildcard with `path_prefix` on Aruna. Decide after the TES test matrix. Consider a helper binary only if exact glob matching inside the task becomes necessary. |
| Reading nf-test files | Extract them on the oracle JVM; parse them in Rust; run nf-test itself against a Nextflow-compatible command | Extract them on the JVM into recorded JSON (section 8.5). |
| Plugins | Implement all; implement used ones and warn for the rest | Implement nf-schema. Fail at load time for other included plugin functions. Warn for plugins that only observe the run. Do not support nf-validation. |
| Container engines | API crates; command lines | Command lines, which match nf-core's raw run options. |
| Command line of `iunctus run` | New options; Nextflow's `run` options | Accept Nextflow's `run` options (`-profile`, `-c`, `-params-file`, `-resume`, `-stub`, `-with-docker`, `-w`) and `--name value` params, so that nf-core instructions work. Options that only iunctus has also use a single dash, because double-dash names are pipeline params. |
| Where output paths point | A virtual work directory; the content store | The content store (section 7.7). Paths then have real names and can be read, without a work directory. |
