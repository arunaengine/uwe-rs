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
3. **Any other receiver.** The name is looked up along the receiver's class chain, most specific first. For a tuple the chain is tuple, List, Collection, Iterable, Object. For a record it is record, Map, Object. Each library entry is a Rust function `fn(&mut Interpreter, &Value, &[Value]) -> Result<Value, Error>`. It checks the argument count and classes itself. If no form fits, it returns "no match", and the lookup continues with the next class in the chain. The oracle checks that this picks the same overload as Groovy for the shapes the corpus uses.

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

Reading a dataflow value (`getVal()`, `.val`) works when the value is already bound. The common case is the completion handler: 100 pipelines call `getVal()` on the result of `toList` there, when every channel is complete. If the value is not bound yet, the read fails with a clear error: iunctus cannot block inside the network. In the corpus, only spatialaxe reads `.val` inside operator closures. If more pipelines need it, the operator can be suspended until the value is bound.

### 3.8 How the standard library grows

The library grows in tiers. Each tier is driven by the inventory data (`scripts.json` from the inventory tool):

1. **What the current milestone needs.** For Milestone 1 that is GUNZIP, its nf-test inputs and its config: `extension`, `baseName`, `name`, `file()`, `in` on lists, string `+`, `task.ext`, `task.process`, `workflow.containerEngine`.
2. **Everything the nf-core modules repository uses.** The load-time method check of section 3.9 runs over all module files and lists the missing names. That list must reach zero for Milestone 2.
3. **The utility subworkflows and nf-schema** for Milestone 3. These need stand-ins for Java classes: `org.yaml.snakeyaml.Yaml`, `groovy.json.JsonOutput`, `JsonGenerator.Options`, `JsonSlurper`, `GStringTemplateEngine`, `SimpleTemplateEngine`, `File`, `FilesEx.copyTo`, `java.util.Date`, `URL` for hooks, `execute()`, and `System.exit`, `getenv` and `getProperty`.
4. **The long tail,** ordered by the "own code" pipeline count.

Raw pipeline counts are a poor order on their own. The vendored utility subworkflows raise the counts of methods like `withWriter`, `delete` and `openConnection` to more than 50 pipelines, although almost no pipeline calls them in its own code. So the order uses the modules repository columns and the "own code" column.

Rules for every library entry:

- It has a unit test with the documented or observed behaviour, and oracle cases (section 8).
- A stand-in for a Java class implements only the methods the corpus calls. Any other method of that class is an unsupported-feature error.
- CI writes a coverage report: names used in the corpus against names implemented, per type, with pipeline counts.

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

