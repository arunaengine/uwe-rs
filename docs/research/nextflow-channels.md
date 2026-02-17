<!-- Verified implementer specification of Nextflow dataflow semantics: channels, factories, operators, process task formation, workflow outputs and topics -->
<!-- Copyright (c) 2026 The iunctus Contributors -->
<!-- SPDX-License-Identifier: MIT or Apache-2.0 -->

# Nextflow dataflow semantics: implementer specification

This document describes how Nextflow moves data through channels, operators and processes.
It is written for a native Rust implementation that uses async channels.
It focuses on value kinds, ordering, completion and error cases.
Every claim in it was checked against the Nextflow source. Section 12 lists what is still unverified.

## 0. Scope, sources and conventions

### 0.1 Sources

- Nextflow checkout: `/tmp/nf-research/nextflow`, commit `bbf6ff0` (2026-09-25).
- Unless a path starts with `modules/` or `docs/`, source paths in this document are relative to
  `modules/nextflow/src/main/groovy/nextflow/`. Example: `extension/JoinOp.groovy:67`.
- Nextflow builds its dataflow on GPars 1.2.1 (`modules/nextflow/build.gradle:43`).
  GPars is not in the checkout. I read the GPars 1.2.1 sources jar from Maven Central
  (`org/codehaus/gpars/gpars/1.2.1/gpars-1.2.1-sources.jar`). GPars paths below are relative to
  `groovyx/gpars/dataflow/` inside that jar and are marked "GPars".
- Some behaviour was also checked by running small scripts with a locally installed Nextflow 26.04.6
  (strict syntax parser v2, the default). These checks are marked "experiment". The checkout is newer
  than 26.04.6, so an experiment only confirms the source reading, it does not replace it.
- Groovy language rules (for example `==`, `isCase`, `combinations()`) come from the Groovy runtime,
  which is not in the checkout. Where this document relies on them, the behaviour was confirmed by an experiment.

### 0.2 Two operator implementations

Nextflow has two implementations of channel operators:

- Legacy operators: `extension/OperatorImpl.groovy` and the `extension/*Op.groovy` classes.
  They run for every script that does not enable static typing.
- Typed operators: `dataflow/ChannelImpl.groovy`, `dataflow/ValueImpl.groovy` and `dataflow/ops/*.groovy`.
  They run only when a script sets `nextflow.enable.types = true`.
  The v2 compiler imports `channel` as `nextflow.dataflow.ChannelNamespace` when typing is enabled,
  and as `nextflow.Channel` otherwise (`script/parser/v2/ScriptCompiler.java:386-389`).
  Typing is detected per script file by `ScriptNode.isTypingEnabled()` (`modules/nf-lang/src/main/java/nextflow/script/ast/ScriptNode.java:168-174`).
  A typed channel object still falls back to the legacy implementation for every operator name that it does
  not define itself (section 10).

In the nf-core corpus (`~/.cache/iunctus/corpus/nf-core-text`), no file sets `nextflow.enable.types`.
So nf-core runs the legacy operators, even for the operators that `docs/reference/stdlib-types/channel.mdx`
lists as the "core" set. Section 5 specifies the legacy operators. Section 10 specifies the typed core set.

### 0.3 Terms

- Queue channel: an asynchronous sequence of values that ends with a stop signal.
- Value channel: a dataflow variable. It is bound at most once. Nextflow docs also call it a "dataflow value".
- STOP: the stop signal. In source it is `Channel.STOP`, which is the GPars `PoisonPill` singleton
  (`Channel.groovy:64`, `extension/CH.groovy:48`).
- VOID: `Channel.VOID`, the Groovy `NullObject` (`Channel.groovy:66`). Some operator closures return it to mean "emit nothing".
- Null and VOID: several legacy operators compare a closure result with VOID using the Groovy `==` operator or a
  Groovy `switch` case (for example `extension/MapOp.groovy:60`, `extension/DataflowHelper.groovy:327`,
  `extension/OperatorImpl.groovy:178`, `:692`, `:760`). Groovy treats `null` as equal to `NullObject` there,
  so in these operators a `null` result behaves exactly like VOID.
  Experiment: `map`, `reduce`, `flatMap` and `flatten` all treat `null` like VOID (sections 5.16, 5.17, 5.22, 5.29).
  Operators built on the GPars `ChainWithClosure` compare by identity instead, so there a `null` result is
  emitted and only VOID is skipped (GPars `operator/ChainWithClosure.java:62-76`).
- Empty value channel: a value channel that is bound to STOP.
- Groovy truth: the Groovy rule that turns any value into a boolean (null, 0, empty string, empty collection and `false` are false).
- `isCase` criteria: a filter argument matched with Groovy `isCase` (`extension/OperatorImpl.groovy:104-122`).
  A regex must match the whole `toString()` of the value (experiment: `filter(~/a/)` keeps `'a'` but not `'abc'`).
  A class matches instances (experiment: `filter(Number)`). A range or collection matches its members
  (experiment: `filter(2..3)` keeps 2 and 3). A closure is called and its result is tested with Groovy truth.
  Any other value is compared for equality.
- Key equality: operators that group or match by key (`join`, `groupTuple`, `combine` with `by`, `cross`, `unique`)
  store keys in Java hash maps, so keys match by Java `equals` and `hashCode`, not by Groovy `==`.
  Experiment: an `Integer` key `1` and a `Long` key `1` do not match in `join`.

## 1. Execution model

### 1.1 Build first, then ignite

A run has two phases.

1. Build. The entry workflow body runs once. Each factory call, operator call and process call only wires
   a node into the graph (`docs/reference/semantics.mdx`, section "Definition vs execution").
   Factories do not emit values yet.
2. Ignite. After the body has run, `Session.fireDataflowNetwork()` connects topic channels, connects legacy queue
   bridges, and then calls every registered "igniter" (`Session.groovy:616-646`, `script/ScriptRunner.groovy:225-232`).
   Factories register their emission code as igniters (`extension/CH.groovy:211-231`).
   Processes register their start as igniters (`processor/TaskProcessor.groovy:608-613`, `Session.groovy:606-614`).
   If an igniter throws, the run is aborted and the remaining igniters are not called (`Session.groovy:636-645`).
   A plain igniter added after ignition is never called; only process starts run at once after ignition
   (`Session.groovy:598-614`).

The workflow `output` block is applied after the entry workflow body and before ignition (`script/BaseScript.groovy:306-308`).

Exceptions to "nothing is emitted during the build":

- Some value channels are bound during the build: plain process arguments (`script/params/BaseInParam.groovy:83-85`)
  and plain objects emitted by a named workflow (`script/WorkflowDef.groovy:165-170`).
  A value channel keeps its value, so late readers still see it.
- `take(0)` writes STOP to its queue output at call time (`extension/TakeOp.groovy:54-57`). A downstream
  operator creates its read cursor later and never sees that STOP (section 2.1).
  Experiment: `channel.of(1,2).take(0).toList().view()` never ended.

Implementation rule: build the whole graph and create all subscriptions before any source emits.
To match Nextflow exactly, `take(0)` must behave as described above; a cleaner design is to close the
output at ignition (design choice, not Nextflow behaviour).

### 1.2 Unbounded buffers

GPars channels have no capacity limit. A producer never blocks on a slow consumer.
Many Nextflow patterns rely on this. Example (my analysis, not tested): a process reads queue `a` and queue `b` pairwise,
and `b` is derived from `a.collect()`. The process subscription on `a` must buffer all of `a` until `collect` finishes.
With a bounded buffer and backpressure this deadlocks.
Implementation rule: give every subscriber an unbounded buffer (or a buffer that spills to disk).

### 1.3 Operator threading

- An operator created without `maxForks` handles one message at a time, in arrival order
  (GPars `operator/DataflowOperator.java:58-63` and `operator/DataflowProcessor.java:88-91`: a single
  `DataflowOperatorActor` unless `maxForks` is set and not 1).
- No built-in legacy operator sets `maxForks` (`extension/DataflowHelper.groovy:144-160`). Only the undocumented
  `chain(params, closure)` passes user parameters through (`extension/OperatorImpl.groovy:415-419`).
  So a single-input operator sees values in channel order and calls its closure sequentially.
- Operators with several inputs that use the subscribe helper (section 2.4), for example `mix`, `join`,
  `combine` and `cross`, create one GPars operator per input. Handlers for different inputs can run
  at the same time. (`concat` subscribes to its inputs one after another, so its handlers do not overlap.)
  The operators guard their shared state with locks (`extension/JoinOp.groovy:115`,
  `extension/CombineOp.groovy:123`, `extension/CrossOp.groovy:73`).
- The process operator is created with `maxForks: session.poolSize` (`processor/TaskProcessor.groovy:597`).
  The operator body (task creation) can then run in parallel on a thread pool
  (GPars `operator/ForkingDataflowOperatorActor.java`). The task index is assigned in `beforeRun`, which GPars calls
  in the single actor thread in message order (`processor/TaskProcessor.groovy:2063-2077`).

### 1.4 Operator errors

An exception in an operator closure aborts the whole run. The default listener logs the error and calls
`session.abort(e)` (`extension/DataflowHelper.groovy:94-102`, `extension/DataflowHelper.groovy:105-125`).
Exception: errors while publishing workflow outputs do not abort the run (section 8.5).

### 1.5 When a run ends

`Session.await()` waits until every process has terminated, then joins every operator
(`Session.groovy:816-827`, `Session.groovy:863-877`).
So a run ends only when every process and every operator has stopped.
An operator that never receives STOP keeps the run alive forever.
Experiment: `channel.of(1,2,3).first { v -> v > 10 }.view()` never ends; the log shows `[operator] view` still active.

Some legacy operators never stop when their source is a value channel. They emit their result, but the operator
itself stays active, so the run never ends. Experiments showed this for `distinct`, `until`, `tap`, `buffer`,
`collate`, `max` and `min` (`max` and `min` also emit nothing). The cause: a GPars operator whose only input is a value
channel re-reads the value without a new message, so it never fires again and never sees STOP
(GPars `operator/DataflowOperatorActor.java:56-69`). `max`, `min` and `count` on a queue use `reduceImpl`
(`extension/DataflowHelper.groovy:313-353`), which only binds its result when the operator stops.

These operators do not have this problem:

- Operators that stop explicitly after the first value: `map`, `unique`, `flatten`, `flatMap`, `merge` (when its first
  operand is a value), `toInteger`, `toLong`, `toFloat`, `toDouble`.
- Operators that use the subscribe helper (section 2.4).
- `filter`, `first` and `count` on a value source, which use a one-time `whenBound` callback and create no operator
  (`extension/OperatorImpl.groovy:108-113`, `:469`, `:543-546`).

## 2. Channel kinds and the stop signal

### 2.1 Queue channels

- Created by `CH.create()`, which returns a GPars `DataflowBroadcast` (`extension/CH.groovy:143-148`).
  A few legacy places still use a plain `DataflowQueue` (`CH.queue()`), for example `Channel.empty()` (`Channel.groovy:121-125`)
  and `channel.fromSRA` (`Channel.groovy:617`).
- Every reader gets its own read cursor from `DataflowBroadcast.createReadChannel()`.
  A cursor sees only values written after it was created (GPars `DataflowBroadcast.java:58-67`, javadoc: "from now on").
  Operator calls create the cursors of their source and channel arguments at call time (`extension/OpCall.groovy:143-167`).
  This is safe because nearly all emission happens after ignition (section 1.1).
- A `DataflowQueue` that is read through `CH.getReadChannel()` gets a bridge broadcast. At ignition the queue is
  piped into the bridge (`extension/CH.groovy:59-104`).
- Result: a queue channel is multicast. Every consumer sees every value, in the same order.
  Experiment: two `map` operators on the same `channel.of(1,2,3)` both receive 1, 2 and 3.
- Kind test: `CH.isChannelQueue()` is true for `DataflowQueue`, `DataflowStreamReadAdapter` and
  `DataflowStreamWriteAdapter` (a `DataflowBroadcast` is a `DataflowStreamWriteAdapter`) (`extension/CH.groovy:183-185`).

### 2.2 Value channels

- A GPars `DataflowVariable` (`extension/CH.groovy:143-148`, `extension/CH.groovy:196-204`).
- All readers read the same value. Reading an unbound value waits.
- Binding twice with an equal value is ignored. Binding twice with a different value throws
  (GPars `expression/DataflowExpression.java:356-372`).
- Kind test: `CH.isValue()` is true for any `DataflowExpression` (`extension/CH.groovy:179-181`).

### 2.3 Completion

- A queue channel is complete when STOP is written to it. Values written after STOP are never seen.
- A value channel is complete when it is bound. If it is bound to STOP it is an empty value channel.
- Helper `CH.close0()` writes STOP to a queue, or binds STOP to a value only if it is still unbound (`extension/CH.groovy:128-137`).
- An operator built on a GPars operator with declared outputs forwards STOP to all its outputs and stops when STOP
  arrives on any input (GPars `operator/DataflowOperatorActor.java:72-87`, `operator/DataflowProcessorActor.java:112-118`).
- `DataflowProcessor.terminate()` stops an operator without writing STOP (GPars `operator/DataflowProcessor.java:146-148`).
  Some operators rely on this and bind their result themselves.

### 2.4 The subscribe helper

Most operators are written with `DataflowHelper.subscribeImpl(source, [onNext, onComplete, onError])`
(`extension/DataflowHelper.groovy:246-293`). Its rules:

- Queue source: `onNext` for each value in order, then `onComplete` after STOP.
- Value source: `onNext` once with the value, then the subscriber stops and `onComplete` runs.
- Empty value source (bound to STOP): no `onNext`, only `onComplete`.
- If a handler throws, `onComplete` is not called. Without `onError` the run is aborted. With `onError`, the
  handler is called and the run is not aborted.
- Handler names other than `onNext`, `onComplete` and `onError` are an error (`extension/DataflowHelper.groovy:218-237`).

Implementation rule: a value channel read by an operator behaves like a queue channel with one value.
An empty value channel behaves like an empty queue channel.

### 2.5 How plain values become channels

- A plain (non-channel) argument to a legacy process becomes a value channel. It is bound at call time
  (`script/params/BaseInParam.groovy:71-86`). A `null` argument is an error "A process input channel evaluates to null"
  (`script/params/BaseInParam.groovy:145-156`; experiment). Typed processes also wrap a plain value in a value
  channel at call time, but they do not reject `null` (`script/ProcessDef.groovy:252-259`).
- A plain object emitted by a named workflow becomes a value channel (`script/WorkflowDef.groovy:165-170`).
- A plain object assigned in `publish:` becomes a value channel (`script/WorkflowBinding.groovy:187-199`).
- A `Collection` (for example a `List` or `Set`) or an array as the right operand of `combine` becomes a queue channel.
  Any other non-channel operand is an error (`extension/CombineOp.groovy:57-75`).
- Factories reject channel arguments: `channel.of`, `channel.from`, `channel.value`, `channel.fromPath`,
  `channel.fromFilePairs` and `channel.fromSRA` throw "cannot be a channel object" (`Channel.groovy:93-103`).
  `channel.fromList` does not check its argument (`Channel.groovy:175-180`).

## 3. Output kind rules

The legacy helper `CH.createBy(source)` returns a value channel for a value source and a queue channel otherwise
(`extension/CH.groovy:139-141`). Operators use it or pick a fixed kind.

| Rule | Legacy operators |
| --- | --- |
| Same kind as the source | `filter`, `map`, `until`, `unique`, `distinct`, `view`, `dump` (when enabled), `ifEmpty`, `tap` (result and tapped channels), `branch` outputs, `multiMap` outputs, `merge` (kind of the first operand), `toInteger`, `toLong`, `toFloat`, `toDouble` |
| Always queue | `flatMap`, `flatten`, `mix`, `concat`, `combine`, `cross`, `join`, `groupTuple`, `buffer`, `collate`, `transpose`, `take`, `randomSample`, `collectFile`, `splitCsv`, `splitFasta`, `splitFastq`, `splitJson`, `splitText` |
| Always value | `collect`, `toList`, `toSortedList`, `first`, `last`, `count`, `countLines`, `countFasta`, `countFastq`, `countJson`, `min`, `max`, `sum`, `mean`, `reduce` |
| No output | `set`, `subscribe` |

Other kind rules:

- A process returns value channels when none of its inputs is a queue channel and it has no `each` input.
  Otherwise it returns queue channels (section 7.2).
- A topic source channel created for a legacy process output is always a queue channel (`script/ProcessDef.groovy:197`).
- A named workflow emits whatever channel it was given; plain objects become value channels (section 8.2).
- `take`, `reduce` and `randomSample` reject a value source with an error (`extension/OperatorImpl.groovy:257-258`,
  `extension/OperatorImpl.groovy:497-498`, `extension/OperatorImpl.groovy:992-993`).
- Operators that always return a queue treat a value source as a one-item channel (section 2.4).

## 4. Channel factories

All factories emit their values at ignition (section 1.1), not when they are called.
Unless stated otherwise, the output is a queue channel and ends with STOP.

### 4.1 `channel.of(values...)`

- Emits each argument in argument order, then STOP (`Channel.groovy:127-142`).
- A `Range` argument is expanded into its elements (`Channel.groovy:144-153`). A list argument is emitted as one value.
- `channel.of()` with no argument emits only STOP. `channel.of(null)` emits one `null` (`Channel.groovy:131-132`).
- Channel arguments are rejected (`Channel.groovy:128`).

### 4.2 `channel.fromList(collection)`

- Emits each element in iteration order, then STOP (`Channel.groovy:175-180`).

### 4.3 `channel.value(obj)`

- Returns a value channel. It is bound to `obj` at ignition (`Channel.groovy:199-202`, `extension/CH.groovy:200-216`).
- `channel.value()` and `channel.value(null)` return a value channel that is never bound (`Channel.groovy:201`).
  Any consumer waits forever. Experiment: a process fed with `channel.value()` stays active until killed.
  An implementation should report this as an error instead of hanging (design choice, not Nextflow behaviour).
- Other falsy values such as `false` or `0` are bound normally, because the test is `obj != null`.
- The typed `channel.value(null)` is different: it binds `null` (`dataflow/ChannelNamespace.groovy:71-75`).

### 4.4 `channel.empty()`

- A queue channel that only receives STOP (`Channel.groovy:121-125`).

### 4.5 `channel.fromPath(pattern, [opts])`

- `pattern` is a string, a `Path`, or a list of them. A list is processed pattern by pattern, in list order.
  Each pattern starts after the previous one finished (`Channel.groovy:288-308`). STOP is written after the last pattern
  (`file/PathVisitor.groovy:52`, `file/PathVisitor.groovy:101-104`). If one pattern fails, the later ones are not walked.
- An empty or missing pattern is an error "Missing `fromPath` parameter" (`Channel.groovy:277`).
- Options, checked strictly (unknown option or unknown `type` value is an error) (`Channel.groovy:255-263`, `Channel.groovy:281`):

| Option | Default | Meaning (source) |
| --- | --- | --- |
| `type` | `'file'` | `'file'`, `'dir'` or `'any'` (`file/PathVisitor.groovy:157-159`) |
| `followLinks` | `true` | follow symbolic links while walking (`modules/nf-commons/src/main/nextflow/file/FileHelper.groovy:881`) |
| `hidden` | `false`, but `true` if the file pattern starts with `.` | include hidden files (`modules/nf-commons/src/main/nextflow/file/FileHelper.groovy:883`) |
| `maxDepth` | unlimited if the pattern has `**`; else the number of `/`-separated parts of the file pattern minus one; else 0 | directory depth (`modules/nf-commons/src/main/nextflow/file/FileHelper.groovy:962-974`) |
| `checkIfExists` | `false` | error if a plain path does not exist, or if a glob matches nothing ("No files match pattern") (`file/PathVisitor.groovy:171-173`, `modules/nf-commons/src/main/nextflow/file/FileHelper.groovy:1184-1194`) |
| `glob` | `true` | when `false`, the pattern is a literal path (`file/PathVisitor.groovy:106-113`) |
| `relative` | `false` | emit paths relative to the search folder (`modules/nf-commons/src/main/nextflow/file/FileHelper.groovy:887`, `:901`, `:916`) |

- A pattern without glob characters is emitted as a path even if the file does not exist, unless `checkIfExists` is true
  (`file/PathVisitor.groovy:119-123`).
- A glob that matches nothing emits nothing. A search folder that does not exist also emits nothing
  (`file/PathVisitor.groovy:168-170`). Experiment: `fromPath('data/nomatch*.fq').toList()` gives `[]`.
- Emission order is the file tree walk order of `Files.walkFileTree` (`modules/nf-commons/src/main/nextflow/file/FileHelper.groovy:892-939`). It is not sorted.
  Treat it as unspecified.
- The file walk runs on a background thread. An error aborts the run (`Channel.groovy:297-314`).
- Documented options and defaults: `docs/reference/stdlib-namespaces/channel.mdx` (section `fromPath`).

### 4.6 `channel.fromFilePairs(pattern, [opts], [grouping])`

Implementation in `Channel.groovy:420-531`. It is a pipeline:

1. Walk files like `fromPath` for each pattern. Each file is tagged with its pattern index.
2. Map each file to `[prefix, file]`. The prefix comes from `grouping` (a closure, used for every pattern) or from `readPrefix(file, pattern)`.
3. Group with `groupTuple(sort: true, size: size)` (section 5.18).
4. If `flat: true`, turn `[prefix, [f1, f2]]` into `[prefix, f1, f2]`.

Rules:

- `size` default: 2 if any pattern contains glob characters, else 1 (`Channel.groovy:487-494`). `size: 0` also gives the
  default, because 0 is Groovy false. `size: -1` means any size; groups are then emitted only after all files were seen.
- A group with fewer files than `size` is dropped silently at the end (groupTuple `remainder` is false).
  Experiment: with files `s1_1.fq`, `s1_2.fq`, `s2_1.fq` and pattern `*_{1,2}.fq`, only `s1` is emitted; with `size: -1`,
  `s2` is emitted with one file. If a prefix has more files than `size`, the extra files start a new group
  (section 5.18), which is usually dropped.
- Files in a group are sorted by natural `Path` order (`extension/GroupTupleOp.groovy:169-173`).
- `readPrefix` (`Channel.groovy:556-604`):
  - Take the file name part of the pattern (after the last `/`).
  - If the pattern has no `*`, `?`, `{` or `[`, it must equal the file name, and the prefix is the file name up to its
    first dot (all extensions removed, `modules/nf-commons/src/main/nextflow/extension/FilesEx.groovy:400-407`).
    Otherwise it is an error "Not a valid file pair globbing pattern".
  - Count `*` and `?` before the first `{` or `[`.
  - Convert the pattern to a regex: `.` is escaped, `*` becomes `(.*)`, `?` becomes `(.?)` (zero or one character),
    `{a,b}` becomes `(?:a|b)`. `[...]` is kept as a regex character class.
  - If the regex does not match the whole file name, the prefix is `null`.
  - If at least one wildcard was counted, the prefix is the file name up to the end of the last counted wildcard group.
  - If no wildcard was counted, the prefix is the file name cut at the character index where the first `{` or `[`
    appears in the pattern. Example: pattern `sample_{1,2}.fq` gives the prefix `sample` for `sample_1.fq`.
  - Trailing `-`, `_` and `.` are removed.
- Options: the `fromPath` options are passed through, unknown options are ignored silently and values are not
  checked (`Channel.groovy:472`, `Channel.groovy:533-542`). Plus `size` and `flat`.
- Output: queue channel. Groups are emitted as soon as they reach `size`, so order follows the file walk.

### 4.7 `channel.fromSRA(query, [opts])`

- Deprecated in 26.04 (`docs/reference/stdlib-namespaces/channel.mdx`). Queries NCBI and emits `[accession, file or files]`.
- Options (checked): `apiKey`, `cache`, `max`, `protocol` (`ftp`, `http`, `https`), `retryPolicy`
  (`datasource/SraExplorer.groovy:51`, `Channel.groovy:613-632`).
- Output: a plain `DataflowQueue`. Network access happens in a background task after ignition.
- The strict parser accepts it, although the nf-lang channel namespace does not declare it
  (`modules/nf-lang/src/main/java/nextflow/script/namespaces/ChannelNamespace.java:30-118`).
  Experiment: a script with `channel.fromSRA('SRP043510')` compiled and ran in `-preview` mode.

### 4.8 `channel.fromLineage(opts)`

- Experimental. Emits paths from the lineage store (`Channel.groovy:634-653`).
- Options (checked): `label` (string or list), `taskRun`, `workflowRun` (`extension/LinExtension.groovy:29-33`).
- Needs the lineage extension; without it the run fails with "Unable to load lineage extensions" (`Channel.groovy:644-646`).

### 4.9 `channel.topic(name)`

- Returns the one shared broadcast channel of that topic (`extension/CH.groovy:150-160`). See section 8.4.

### 4.10 `channel.watchPath(pattern, events = 'create')`

- Watches a folder and emits matching files as absolute paths (`Channel.groovy:316-402`).
- `pattern` is a glob string, a `Path`, or a regex `Pattern` (`Channel.groovy:349`, `:376`, `:393`).
- `events` is a comma-separated list of `create`, `modify`, `delete` (`docs/reference/stdlib-namespaces/channel.mdx`).
- For a string pattern: a pattern ending in `/` gets `*` appended; an existing directory gets `/*` appended (`Channel.groovy:378-381`).
- Hidden files are skipped when the file pattern starts with `*` (glob forms only) (`Channel.groovy:387`, `:398`).
- The channel only completes when the watcher is terminated, which happens at cleanup (`Channel.groovy:323-324`). In practice it never completes
  by itself. Use `take` or `until` (`docs/reference/stdlib-namespaces/channel.mdx`).
- Not used in the nf-core corpus.

### 4.11 `channel.interval(duration, [closure])`

- Emits `0, 1, 2, ...` (or `closure(index)`) every `duration`, starting at ignition (`Channel.groovy:210-247`).
- If the closure returns STOP, the timer stops and STOP is emitted. Otherwise it never completes.

### 4.12 `channel.from(values...)` (deprecated)

- Emits each argument (or each element of a single collection) then STOP (`Channel.groovy:162-197`).
- Experiment: still runs under the v2 parser in 26.04.6. It is not declared in the nf-lang channel namespace
  (`modules/nf-lang/src/main/java/nextflow/script/namespaces/ChannelNamespace.java`).

### 4.13 Typed-mode factories

With `nextflow.enable.types = true`, `channel` is `dataflow/ChannelNamespace.groovy`. It has `empty`, `fromLineage`,
`fromList`, `fromPath`, `interval`, `of`, `topic`, `value` and `watchPath` (`dataflow/ChannelNamespace.groovy:32-80`).
Differences from the legacy factories:

- `fromFilePairs` and `fromSRA` are not present.
- `empty()` uses a broadcast channel instead of a `DataflowQueue` (`dataflow/ChannelNamespace.groovy:32-36`).
- `fromPath` takes a single string pattern only; `interval` takes no closure; `watchPath` takes a string pattern only
  (`dataflow/ChannelNamespace.groovy:50-80`).
- `value(null)` binds `null` (`dataflow/ChannelNamespace.groovy:71-75`, `extension/CH.groovy:200-204`).

## 5. Legacy operators

### 5.1 Common rules

- Operator names are the public, non-static methods of `extension/OperatorImpl.groovy` whose first parameter is a channel
  (`plugin/extension/PluginExtensionProvider.groovy:93-98`, `:161-195`), plus `dump` (`extension/ChannelEx.groovy:57-72`).
  The full list: `branch`, `buffer`, `chain`, `collate`, `collect`, `collectFile`, `combine`, `concat`, `count`,
  `countFasta`, `countFastq`, `countJson`, `countLines`, `cross`, `distinct`, `filter`, `first`, `flatMap`, `flatten`,
  `groupBy`, `groupTuple`, `ifEmpty`, `join`, `last`, `map`, `max`, `mean`, `merge`, `min`, `mix`, `multiMap`,
  `randomSample`, `reduce`, `set`, `splitCsv`, `splitFasta`, `splitFastq`, `splitJson`, `splitText`, `subscribe`, `sum`,
  `take`, `tap`, `toDouble`, `toFloat`, `toInteger`, `toList`, `toLong`, `toSortedList`, `transpose`, `unique`, `until`, `view`.
- An operator can be applied to a queue channel, a value channel, or the result of a process or workflow call
  (`ChannelOut`). For a `ChannelOut` with one channel, the operator uses that channel. For a `ChannelOut` with several
  channels and no other argument, the first channel is the source and the other channels become the arguments.
  Example: `P.out.mix()` mixes all outputs of `P`. With several channels and extra arguments it is an error
  "Multi-channel output cannot be applied to operator ...". A `ChannelOut` with no channel is an error
  "cannot be applied to an undefined output" (`extension/OpCall.groovy:81-102`).
- `ChannelOut` arguments are spread into separate arguments (`extension/OpCall.groovy:70`, `:78`).
- Channel arguments are subscribed when the operator is called, during the build phase
  (`extension/OpCall.groovy:143-178`). So no value is lost, even for operators that read an argument later (for example `concat`).
- Single-input operators preserve source order, because they handle one message at a time (section 1.3).
- An operator closure that throws aborts the run (section 1.4).
- With the v2 parser every interpolated string in the script is turned into a `String` at compile time
  (`modules/nf-lang/src/main/java/nextflow/script/control/GStringToStringVisitor.java`, applied in
  `script/parser/v2/ScriptCompiler.java:396`). Key equality in `join`, `groupTuple`, `combine` and `cross`
  is plain Java `equals` and `hashCode` on the key objects (section 0.3).
- Key order: `join` and `combine` with `by` put the key elements in list index order, not in the order of the `by`
  list (`extension/DataflowHelper.groovy:372-377`). `groupTuple` builds its lookup key in `by` order
  (`extension/GroupTupleOp.groovy:94`) but keeps key elements at their original positions in the output.

Blocking summary. These operators emit nothing (or only part of their output) until the source completes:
`collect`, `toList`, `toSortedList`, `last`, `count`, `count*`, `sum`, `mean`, `min`, `max`, `reduce`, `randomSample`,
`collectFile`, `groupTuple` without a size, `concat` (for its later operands), the remainder part of `join`,
the remainder part of `buffer` and `collate`, and the default value of `ifEmpty`.

Order summary. Order-preserving (output order follows source order): `map`, `filter`, `flatMap`, `flatten`, `until`,
`unique`, `distinct`, `view`, `dump`, `ifEmpty`, `take`, `transpose`, `buffer`, `collate`, `split*`, `tap`, `branch`,
`multiMap`, `collect`, `toList`, `concat`. Not order-preserving across inputs: `mix`, `combine`, `cross`, `join`,
`groupTuple` (group emission), `merge` (pairs by position, so it inherits any upstream disorder), `randomSample`,
process outputs (section 7.8).

### 5.2 `branch`

- Form: `branch { v -> label1: condition1; label2: condition2 ... }`, or a criteria object from `branchCriteria { ... }`.
- Compile step: the labelled statements become an `if` / `else if` chain. Statements before the first label run first.
  Each branch returns the pair (value, label). The value is the closure parameter (a list of parameters if there are
  several), or the `return` value or last expression written after the condition
  (`modules/nf-lang/src/main/java/nextflow/script/control/OpCriteriaVisitor.java:129-250`, class `BranchTransformer`).
- Run time: the value goes to the first label whose condition is true. If no condition is true, the value is dropped
  (`extension/BranchOp.groovy:53-58`).
- Outputs: one channel per label, in label order, each of the same kind as the source. The result is a `ChannelOut`
  with names, so `result.label` works. At completion each queue output gets STOP and each unbound value output is
  bound to STOP (`extension/BranchOp.groovy:60-69`).
- Errors (compile time): duplicate label, no label, closure declared with an explicit empty parameter list
  (`OpCriteriaVisitor.java:138-183`).
- Experiment: `channel.of(1,2,3,4).branch { v -> small: v < 2; big: v > 2 }` gives `small = [1]`, `big = [3, 4]`; 2 is dropped.

### 5.3 `buffer`

Forms (`extension/OperatorImpl.groovy:771-800`, `extension/BufferOp.groovy`):

- `buffer(closingCriteria)`, `buffer(remainder: bool, closingCriteria)` and `buffer(openingCriteria, closingCriteria)`,
  criteria matched with `isCase`.
- `buffer(size: n, skip: m, remainder: bool)`. Only `size`, `skip` and `remainder` are allowed.

Rules (`extension/BufferOp.groovy:152-207`):

- State: `open` is true at start when there is no opening criteria.
- For each value: if `open`, append it. Else, if the opening criteria matches, set `open` and append it.
  Then, if the closing criteria matches the value, emit the buffer, start a new empty buffer, and set `open` back to
  "no opening criteria".
- The closing criteria is tested even when the buffer is not open. So a closing match outside an open frame emits
  an empty list. Experiment: `of(1..6).buffer({ v -> v == 2 }, { v -> v == 3 || v == 5 })` gives `[2,3]`, `[]`.
- `size`/`skip` mode: the opening criteria is "skip `m` values, then open"; the closing criteria is "`m + n` values
  counted since the last close" (`extension/BufferOp.groovy:129-149`). Example: `size: 3, skip: 2` over 1..10 gives
  `[3,4,5]`, `[8,9,10]` (traced from source).
- On STOP: if `remainder` is true and the buffer is not empty, emit it. Default `remainder` is false.
- Value source: runs once, emits the partial buffer only if `remainder`, then STOP. The operator itself never stops,
  so the run never ends (experiment, section 1.5).
- Output: queue. Order preserved.
- Errors: `size` or `skip` together with criteria (`IllegalArgumentException` without message), unknown option.
- Experiment: `channel.of(1,2,3,4,5).buffer(size: 2)` gives `[1,2]`, `[3,4]`.

