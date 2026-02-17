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

### 5.4 `collate`

- `collate(size, remainder = true)`: same as `buffer(size: size, remainder: remainder)`. Note the different default
  for `remainder` (`extension/OperatorImpl.groovy:802-812`).
- `collate(size, step, remainder = true)`: a new window starts at every `step`-th value. Each value is appended to every
  open window. When the oldest window reaches `size`, it is emitted. On STOP, if `remainder`, all open windows are
  emitted in start order (`extension/OperatorImpl.groovy:814-871`).
- Errors: `size <= 0` or `step <= 0` at call time. At run time, `step > size` fails at the first value that falls
  between two windows, because the code reads the head of an empty window list (`extension/OperatorImpl.groovy:862`).
  Experiment: `of(1..7).collate(2, 3)` emitted `[1, 2]`, then failed with "Cannot access first() element from an empty List".
- Value source: the result is emitted, but the operator never stops and the run never ends (experiment, section 1.5).
- Output: queue. Order preserved.
- Experiments: `collate(2)` over 1..5 gives `[1,2] [3,4] [5]`; `collate(3,1)` gives `[1,2,3] [2,3,4] [3,4,5] [4,5] [5]`.

### 5.5 `collect`

- Forms: `collect()`, `collect(closure)`, `collect(flat: bool, sort: true | closure | Comparator)` (`extension/CollectOp.groovy`).
- Waits for the source to complete, then emits one value.
- For each value `v`: `x = closure ? closure(v) : v`. If `x` is a `List` and `flat` is not false, its elements are added
  (one level only). Otherwise `x` is added (`extension/CollectOp.groovy:64-73`).
- `sort: true` sorts by natural order; a closure or `Comparator` sorts with it (`extension/CollectOp.groovy:75-89`).
- The emitted value is an `ArrayBag`: a list that keeps arrival order. Its Java `equals` ignores order (same size and
  `containsAll`) and its `hashCode` is order-independent (`util/ArrayBag.groovy:69-120`). Groovy `==` still compares it
  as an ordered list (`util/ArrayBag.groovy:80-98`). Task hashing treats it as unordered
  (`modules/nf-commons/src/main/nextflow/util/HashBuilder.java:161-162`).
- Empty source: the output is bound to STOP, so it is an empty value channel, not `[]` (`extension/CollectOp.groovy:58`).
  The same happens when every value is an empty list, because nothing is added.
- Output: value.
- Experiments: `of(1,2,3).collect()` gives `[1, 2, 3]`; `of([1,[2,3]],[4]).collect()` gives `[1, [2, 3], 4]`;
  `of(1,2,3).collect { v -> [v, [v]] }` gives `[1, [1], 2, [2], 3, [3]]`;
  `empty().collect()` emits nothing; `empty().collect().ifEmpty('DEF')` gives `DEF`.

### 5.6 `collectFile`

- Forms: `collectFile([opts])`, `collectFile([opts], closure)` (`extension/CollectFileOp.groovy`).
- Each value is turned into (file name, content) (`extension/CollectFileOp.groovy:139-177`):
  - `x = closure ? closure(v) : v`.
  - `x` is a list or array with 2 or more elements: element 0 (as a string) is the file name, each later element is appended to that file.
  - `x` is a `Path` or `File`: its content is appended to the file named by the `name` option, or to a file with the
    same name as `x`.
  - `x` is `null`: it is skipped.
  - Otherwise, `x` is appended to the file named by `name`, or to `collect-file.data`.
- Content items must be a `Path`, `File`, `CharSequence` or `byte[]`. Anything else is an error
  "Not a valid file collector argument" (`file/FileCollector.groovy:143-156`).
  Experiment: `of(1,2,3).collectFile(name: 'z.txt')` failed with this error.
- Waits for completion. Then it writes the files to `storeDir` (or a temporary folder in the work directory) and emits
  each file, then STOP (`extension/CollectFileOp.groovy:185-194`).
- Options (checked) (`extension/CollectFileOp.groovy:41-54`, `docs/reference/operator.mdx` section `collectFile`):

| Option | Meaning |
| --- | --- |
| `name` | output file name; a `Path` or a name with `/` also sets `storeDir` |
| `storeDir` | folder for the result files |
| `tempDir` | folder for temporary sort files |
| `newLine` | append a newline after each entry (default false) |
| `seed` | initial content (value or map of values per file) |
| `skip` | skip the first N lines of each entry |
| `keepHeader` | keep the header of the first entry; implies `skip: 1` unless set; `skip` must then be at least 1; conflicts with `seed` |
| `sort` | `'hash'` (default), `true`/`'true'`/`'natural'`, `'index'`, `'deep'`, `false`/`'none'`, closure or `Comparator` |
| `sliceMaxSize`, `sliceMaxItems` | limits for the sort collector |
| `deleteTempFilesOnClose` | clean temporary files |
| `cache` | resume caching, `true`/`false` or a hash mode name (default true) |

- Content order inside a file follows `sort`. The default `'hash'` gives the same order in every run, independent of
  arrival order (`extension/CollectFileOp.groovy:216-219`). Experiment: entries `a` then `b` were written as `b` then `a`.
- Order of the emitted files: with any sorting mode (the default) the files are emitted in ascending file name order
  (`file/SortFileCollector.groovy:103-107`, `:247-257`). Experiment: entries for `b.txt` then `a.txt` were emitted as
  `a.txt`, `b.txt`. With `sort: false` or `'none'` the files come from a `ConcurrentHashMap`, so the order is unspecified
  (`file/SimpleFileCollector.groovy:37`, `:96-105`).
- Output: queue.

### 5.7 `combine`

- Forms: `combine(right)`, `combine(right, by: index | [indexes])`. `right` is a channel, a value channel, a `Collection`
  or an array (`extension/CombineOp.groovy:57-75`). The `flat` option is accepted but not used
  (`extension/OperatorImpl.groovy:79`).
- Emits every pair (left value, right value) exactly once, at the moment the later of the two arrives. A new left value
  is paired with every right value received so far, in right arrival order, and the other way round
  (`extension/CombineOp.groovy:123-147`).
- Result tuple: key elements, then the left value, then the right value. A `List` value contributes its elements
  (one level); any other value, including an array, contributes itself. A one-element result is emitted bare
  (`extension/CombineOp.groovy:113-120`, `extension/DataflowHelper.groovy:384-391`).
  Experiment: `of([1,[2,3]]).combine([4,[5]])` gives `[1,[2,3],4]` and `[1,[2,3],5]`.
- `by`: only pairs with equal keys are combined. The key elements move to the front of the result, in list index order;
  the other elements of the left tuple, then of the right tuple, follow (`extension/DataflowHelper.groovy:357-380`).
  A non-list value is its own key when `by` is `[0]`, otherwise it is an error "Not a valid `by` index".
- STOP after both sides complete. A value channel on either side counts as one value.
- Output: queue. The set of pairs is deterministic; their order depends on arrival interleaving.
- Memory: keeps every value of both sides until completion.
- Experiments: `of(['a',1],['b',2]).combine(of('x','y'))` gives the 4 pairs `[a,1,x] [a,1,y] [b,2,x] [b,2,y]`;
  `combine(..., by: 0)` gives `[a,1,x] [a,2,x] [b,3,y]` for left `[a,1] [a,2] [b,3]` and right `[a,x] [b,y]`.

### 5.8 `concat`

- `concat(ch2, ch3, ...)`: emits all values of the source, then all values of `ch2`, and so on, then STOP
  (`extension/ConcatOp.groovy:44-66`).
- Each later operand is read only after the previous one completed. Its values wait in its read cursor.
- Output: queue. Fully ordered. A value channel counts as one value. Experiment: `value(1).concat(of(2,3))` gives `1 2 3`.

### 5.9 `count`

- `count()` or `count(criteria)` (`isCase`). Emits the number of matching values when the source completes.
- Empty queue source: 0. Value source: 1 or 0 (`extension/OperatorImpl.groovy:538-555`).
- Quirk: an empty value channel counts as 1 without criteria, because the STOP it holds is treated as a value
  (`extension/OperatorImpl.groovy:544-546`). Experiment: `empty().collect().count()` gives 1.
- Output: value.

### 5.10 `countFasta`, `countFastq`, `countJson`, `countLines`

- Each takes the options of the matching splitter (section 5.33). It splits every source value and counts the emitted
  chunks. Without `by` that is the number of records; with `by: n` it is the number of chunks
  (`splitter/SplitterFactory.groovy:85-106`, the counter is the `each` closure at line 95).
- Emits the total when the source completes. Output: value.

### 5.11 `cross`

- `cross(right, [keyClosure])` (`extension/CrossOp.groovy`, `extension/OperatorImpl.groovy:921-927`).
- Key: `keyClosure(item)`, or by default the first element of a list or array, the first value of a map, the key of a
  map entry, or the item itself (`extension/OperatorImpl.groovy:886-919`).
- Result: `[leftItem, rightItem]`, not flattened.
- Per key, the left side keeps only its latest item. A new left item replaces the old one
  (`extension/PhaseOp.groovy:153-155`). Each right item is paired once with the current left item. Right items that
  arrive before any left item for their key wait. When a left item arrives, all waiting right items are paired
  (`extension/CrossOp.groovy:68-95`).
- Unmatched items are dropped. STOP after both sides complete.
- Output: queue. With duplicate left keys the result depends on timing. Docs: not commutative, keys should be unique
  (`docs/reference/operator.mdx` section `cross`).
- Experiment: left `[a,1] [b,2] [a,3]`, right `[a,x] [a,y] [b,z]` gave `[[a,1],[a,x]]`, `[[a,1],[a,y]]`, `[[b,2],[b,z]]`.

### 5.12 `distinct`

- `distinct([closure])`: drops a value when its key (`closure(v)` or `v`) equals the key of the previous value
  (`extension/OperatorImpl.groovy:440-457`). It only removes consecutive duplicates. Keys are compared with Groovy `==`.
- Quirk: the "previous key" starts as `null`, so leading values whose key is `null` are dropped (`extension/OperatorImpl.groovy:442`, `:447`).
  Experiment: `of(null, null, 1).distinct()` gives `1`.
- Output: same kind. Experiment: `1,1,2,2,1` gives `1,2,1`.
- Value source: the value is emitted, but the operator never stops and the run never ends (experiment, section 1.5).

### 5.13 `dump`

- `dump([tag: String, pretty: bool], [closure])` (`extension/DumpOp.groovy`, `extension/ChannelEx.groovy:57-72`).
- Enabled only when the run has `-dump-channels` (or the `dumpChannels` config setting) and one of its names matches the
  tag. A name is a regex in which `*` becomes `.*`, and it must match the whole tag (`extension/DumpOp.groovy:66-74`).
  A `dump` without tag uses the empty string as its tag.
  The names are split on commas. `-dump-channels` without names means `*`, which matches every tag
  (`modules/nf-cli-v1/src/main/groovy/nextflow/config/ConfigCmdAdapter.groovy:411-412`,
  `modules/nf-cli-v1/src/main/groovy/nextflow/cli/Launcher.groovy:333-335`).
- Disabled: returns the source channel itself. No node is added.
- Enabled: logs `[DUMP: tag] <value>` at info level and forwards each value. Same kind as the source (`extension/DumpOp.groovy:76-97`).
- `dump` cannot be used on the right side of `|` (`modules/nf-lang/src/main/java/nextflow/script/dsl/WorkflowDslV1.java:56`).

### 5.14 `filter`

- `filter(criteria)` with `isCase`, or `filter(closure)` with Groovy truth of the result (`extension/OperatorImpl.groovy:104-140`).
- Queue source: emits matching values, order preserved.
- Value source: the output value is bound to the value if it matches, else to STOP (empty value).
  No operator is created (`whenBound`).

### 5.15 `first`

- `first()`: a value channel bound to the first source value. Empty source gives an empty value.
  A value source gives a warning (`extension/OperatorImpl.groovy:459-471`).
- `first(criteria)`: bound to the first value that matches (`isCase`) (`extension/OperatorImpl.groovy:473-494`).
  If no value matches, the output is never bound, and any consumer of it waits forever.
  Experiment: `channel.of(1,2,3).first { v -> v > 10 }.view()` never ends.
- Later values are ignored.

### 5.16 `flatMap`

- `flatMap([closure])`. `r = closure ? closure(v) : v` (`extension/OperatorImpl.groovy:149-187`):
  - `Collection` or array: each element is emitted.
  - `Map`: each `Map.Entry` is emitted.
  - `Map.Entry`: its key, then its value.
  - `null` or VOID: nothing (section 0.3; experiment: `of(1, null, 2).flatMap { v -> v }` gives `1, 2`).
  - anything else: `r` itself.
- Output: queue, order preserved. A value source emits its elements, then STOP.
- Experiment: `of([a:1,b:2]).flatMap { v -> v }` gives the entries `a=1`, `b=2`.

### 5.17 `flatten`

- A `Collection` or array is flattened recursively and each leaf is emitted. Maps are not flattened.
  `null` and VOID emit nothing. Other values are emitted as they are (`extension/OperatorImpl.groovy:723-769`).
- Output: queue, order preserved.
- Experiments: `of([1,[2,[3]]], [a:1]).flatten()` gives `1, 2, 3, [a:1]`; `of(1, null, 2).flatten()` gives `1, 2`.

### 5.18 `groupTuple`

Options (checked): `by` (index or list, default `[0]`), `size`, `remainder`, `sort`
(`extension/GroupTupleOp.groovy:34`, `docs/reference/operator.mdx` section `groupTuple`).

Rules (`extension/GroupTupleOp.groovy:92-155`):

- Every source value must be a `List`. Experiment: a non-list value fails with
  "Unknown method invocation `collect` on GroupTupleOp type".
- Group key: the list of elements at the `by` indexes, in `by` order. Keys are compared with Java `equals` and `hashCode`.
  `GString` key elements are converted to `String` (`extension/GroupTupleOp.groovy:265-272`).
- A new group copies the key elements from its first tuple and puts an empty bag at every other index. Each value at a
  non-key index is appended to the bag at that index. The emitted tuple keeps key elements in their original positions.
  Tuples of one group should have the same length; the size check counts the last non-key bag (`extension/GroupTupleOp.groovy:104-118`).
- Group size: the `size` option, or else `groupKey(key, n).getGroupSize()` when the key is a single `GroupKey`
  (`extension/GroupTupleOp.groovy:248-257`, `Nextflow.groovy:399-401`). Otherwise there is no size.
  `size: 0` and a negative size mean no size (`extension/GroupTupleOp.groovy:63`, `:117-118`, `:142`).
  Experiment: `groupTuple(size: -1)` behaved like no size.
- With a size `n > 0`: a group is emitted as soon as it has `n` values, and it is removed. A later value with the same key
  starts a new group.
- At completion, all open groups are processed in first-seen key order. With a size and `remainder: false`, a group
  whose size is not `n` is dropped silently. Otherwise it is emitted. Then STOP.
- `sort`: `true`, `'true'`, `'natural'`, `'hash'`, `'deep'`, a closure (1 argument: sort key; 2 arguments: comparator;
  other: error) or a `Comparator`. It sorts every list element of the tuple before emission. The docs discourage it.
- `GroupKey` equality is one-sided (`extension/GroupKey.groovy:79-86`). A `GroupKey` equals its wrapped key and has the
  same hash, but the wrapped key does not equal the `GroupKey`. So mixing `GroupKey` and plain keys for the same key
  depends on which came first. Experiments: `[groupKey('a',2), 1]` then `['a', 2]` gave two groups (the sized one was
  dropped, `[a, [2]]` was emitted); `['b', 1]` then `[groupKey('b',2), 2]` gave one group `[b, [1, 2]]` with no size.
- The emitted key is the key object of the first tuple of the group (a `GroupKey` if that tuple had one).
- Values in a bag keep arrival order. Bags hash as unordered collections (section 5.5).
- Output: queue. Without size, it blocks until completion.
- Experiments: `[a,1] [b,2] [a,3] [b,4] [c,5]` gives `[a,[1,3]] [b,[2,4]] [c,[5]]`; with `size: 2` the `c` group is
  dropped; with `size: 2, remainder: true` it is kept.

### 5.19 `ifEmpty`

- `ifEmpty(value)` or `ifEmpty(closure)` (`extension/OperatorImpl.groovy:952-968`).
- Forwards every source value. At completion, if no value was seen, emits the default. A closure is called at that
  moment. A queue output then gets STOP.
- Output: same kind. On an empty value channel, the output value is bound to the default.
- Experiment: an optional output of a singleton process that produced nothing, followed by `ifEmpty('X')`, gives `X`.

### 5.20 `join`

- Forms: `join(right)`, `join(right, [by:, remainder:, failOnMismatch:, failOnDuplicate:])`. `right` must be a channel
  or value channel; `null` or a non-channel is an error (`extension/OperatorImpl.groovy:193-215`).
- Key: the elements at the `by` indexes (default `[0]`), in list index order. A non-list value is its own key when `by` is `[0]`, otherwise
  it is an error (`extension/DataflowHelper.groovy:357-380`).
- State: for each key, a FIFO list of pending values per side (`extension/JoinOp.groovy:161-222`).
- When a value arrives and the other side has a pending value for the same key, the oldest pending value of each side
  is taken and one result is emitted. So duplicate keys pair first-in with first-in. This is not a cross product.
- Result: key elements, then the left non-key elements, then the right non-key elements. A
  one-element result is emitted bare (`extension/JoinOp.groovy:119`).
- Completion (`extension/JoinOp.groovy:129-140`, `extension/JoinOp.groovy:224-261`): after both sides complete,
  - with `remainder: true`, every pending value is emitted, key first, with a single `null` in place of the missing
    side (keys in first-seen order, values in FIFO order). If neither side ever sent a value with non-key elements,
    just the key is emitted, bare;
  - then STOP.
- `failOnDuplicate`: the run fails when a value arrives for a key that was already matched, or when a pending value
  is left at completion for a key that was already matched (`extension/JoinOp.groovy:177`, `:220`, `:240`, `:263-268`).
- `failOnMismatch`: the run fails at completion when any value is still pending. The message lists up to 10 keys
  (`extension/JoinOp.groovy:270-292`).
- Defaults: both are false. Both become true when `nextflow.enable.strict` is set (`failOnMismatch` only when
  `remainder` is false) (`extension/JoinOp.groovy:67-68`, `NF.groovy:57-59`). The strict syntax parser alone does not
  set them. Experiment with the v2 parser: duplicates did not fail.
- `remainder` together with an explicit `failOnMismatch: true` is an error (`extension/JoinOp.groovy:70-71`).
- On an error the output gets STOP and the error aborts the run.
- `join(right, by: 'field')` with a string runs the record join of section 10 (`extension/OperatorImpl.groovy:211-213`).
- Output: queue. Emission order follows the arrival of the matching value.
- Experiment: left `[a,1] [b,2] [a,3] [c,4]`, right `[a,x] [a,y] [b,z] [d,w]` gives `[a,1,x] [b,2,z] [a,3,y]`;
  with `remainder: true` it adds `[c,4,null]` and `[d,null,w]` at the end.
- Experiment: left key `1` (`Integer`) and right key `1` (`Long`) did not match.

### 5.21 `last`

- Emits the last source value when the source completes (`extension/OperatorImpl.groovy:503-509`).
- Empty source: the output is bound to `null`, not STOP. Experiment confirms `null`.
- Output: value.

### 5.22 `map`

- `map(closure)` (`extension/MapOp.groovy:48-71`).
- `r = closure(v)`. If `r` is `null` or VOID, nothing is emitted. If `r` is STOP, STOP is emitted and the operator stops.
  Otherwise `r` is emitted.
- Output: same kind, order preserved.
- Value source: the closure runs once. If it returns `null`, the output value is never bound, and any consumer of it
  waits forever (experiment).
- Experiment: `of(1,2,3).map { v -> v == 2 ? null : v }` gives `1, 3`.

### 5.23 `max`, `min`

- `max()`, `max(closure)`, `max(Comparator)`; same for `min` (`extension/OperatorImpl.groovy:563-658`).
- A closure with 1 parameter maps each value to a comparable key. A closure with 2 parameters is a comparator.
  Ties keep the earlier value.
- `max` with a closure of 0 or more than 2 parameters is an error at call time (`extension/OperatorImpl.groovy:638-640`).
  `min` has no such check: its reduce closure stays `null`, and the run fails when the second value arrives
  (`extension/OperatorImpl.groovy:579-592`). Experiment: `of(3,1,2).min { a, b, c -> 0 }` failed with
  "Cannot invoke method call() on null object".
- Emits the result when the source completes. Empty source: `null`.
- Value source: nothing is emitted and the run never ends (experiments for `max` and `min`, section 1.5).
- Output: value.

### 5.24 `mean`

- Present in `OperatorImpl` but not in `docs/reference/operator.mdx` (`extension/OperatorImpl.groovy:675-681`). Like `sum`,
  divided by the number of non-null values (`extension/OperatorImpl.groovy:712-720`).

### 5.25 `merge`

- `merge(ch2, ..., [closure])` (`extension/OperatorImpl.groovy:971-989`, `extension/MergeOp.groovy`).
- A GPars operator with one input per operand. Each round takes one value from each input and emits one result.
- Result: `closure(v1, v2, ...)`, or by default a list of all values with one-level flattening
  (`extension/DefaultMergeClosure.groovy:59-65`).
- The closure must declare exactly one parameter per operand. Otherwise the call fails at build time
  (GPars `operator/DataflowOperator.java:55-57`). Experiment: a one-parameter closure for two operands failed with
  "The operator's body accepts 1 parameters while it is given 2 input streams".
- A closure result of VOID emits nothing, but `null` is emitted (GPars `operator/ChainWithClosure.java:62-76`).
  Experiment: `of(1,2).merge(of(3,4)) { a, b -> a == 1 ? null : a + b }` gives `null, 6`.
- A value channel operand is re-read every round, so its value is reused (GPars `operator/DataflowOperatorActor.java:56-69`).
- STOP on any queue operand ends the operator. So the number of results is the length of the shortest queue operand.
- Output: the kind of the first operand. If the first operand is a value channel, there is exactly one round
  (`extension/DataflowHelper.groovy:105-115`).
- Docs warn that it depends on order (`docs/reference/operator.mdx` section `merge`).
- Experiments: `of(1,2,3).merge(of('a','b'))` gives `[1,a] [2,b]`; `of(1,2,3).merge(value('V'))` gives three results;
  `value('V').merge(of(1,2,3))` gives one value `[V, 1]`.

### 5.26 `mix`

- `mix(ch2, ...)`, at least one operand; otherwise the error is "Operator 'mix' should have at least one right operand"
  (`extension/OperatorImpl.groovy:237-242`, `extension/MixOp.groovy:62-79`).
- Emits every value of every operand as it arrives. Values from one operand keep their order. Values from different
  operands interleave in any order.
- STOP after all operands complete. A value channel counts as one value.
- Output: queue.

### 5.27 `multiMap`

- Form: `multiMap { v -> a: expr1; b: expr2 }`. Several labels can share one expression (`a: b: expr`)
  (`modules/nf-lang/src/main/java/nextflow/script/control/OpCriteriaVisitor.java:283-340`, class `MultiMapTransformer`).
  `multiMapCriteria { ... }` creates a reusable criteria.
- For each value, every output receives its expression result (`extension/MultiMapOp.groovy:51-56`).
- Outputs: named, same kind as the source; completion like `branch` (`extension/MultiMapOp.groovy:58-67`).
- Order preserved per output.

### 5.28 `randomSample`

- `randomSample(n, [seed])`: reservoir sampling over the whole source (`extension/RandomSampleOp.groovy`).
- At completion emits up to `n` values. If the source had at most `n` values, they are shuffled. `null` values are
  not emitted. Then STOP.
- A value source is an error. Output: queue.

### 5.29 `reduce`

- `reduce(closure)` or `reduce(seed, closure)`; the closure takes `(accumulator, value)` (`extension/OperatorImpl.groovy:256-285`,
  `extension/DataflowHelper.groovy:313-353`).
- The accumulator starts at `seed` (null when absent). For each value: if the accumulator is `null`, it becomes the
  value; else the closure result `r = closure(acc, value)` is examined:
  - `r` is VOID or `null` (section 0.3): the accumulator is unchanged;
  - `r` is STOP: the operator stops early and emits the current accumulator;
  - otherwise the accumulator becomes `r`.
- At completion the accumulator is emitted. Empty source: the seed, or `null`.
- Experiment: `of(1,2,3).reduce { a, b -> b == 2 ? null : a + b }` gives 4 (the `null` result was ignored).
- A value source is an error. Output: value.

### 5.30 `set`

- `set { name }` binds `name` in the workflow scope to the source channel itself. No new channel and no graph node
  (`extension/OperatorImpl.groovy:1090-1102`).
- More than one name, or none, is an error.
- The strict parser treats `set { name }` (exactly one statement) inside a workflow as a declaration of `name`
  (`modules/nf-lang/src/main/java/nextflow/script/control/VariableScopeVisitor.java:755-765`).
- Applied to a `ChannelOut`, it binds the `ChannelOut` object (`extension/OpCall.groovy:83-86`).

### 5.31 `subscribe`

- `subscribe(closure)` or `subscribe(onNext:, onComplete:, onError:)` (`extension/OperatorImpl.groovy:293-305`,
  `extension/DataflowHelper.groovy:246-293`).
- No output. Handlers run one at a time. See section 2.4.

### 5.32 `sum`

- `sum([closure])` (`extension/OperatorImpl.groovy:667-721`). `null` and VOID values are skipped.
- The first remaining value (after the closure) becomes the accumulator, whatever its type. After that, numbers are
  added, and lists are added element by element up to the shorter length (the first list is changed in place).
  Any other combination is an error "Invalid `sum` item". Experiment: `of('a','b').sum()` failed with
  "Invalid `sum` item: b [String]".
- Empty source: `null`. Output: value.

### 5.33 Splitters: `splitCsv`, `splitFasta`, `splitFastq`, `splitJson`, `splitText`

Shared rules (`extension/SplitOp.groovy`, `splitter/AbstractSplitter.groovy`, `splitter/AbstractTextSplitter.groovy`):

- Source values are handled one at a time, in order. All chunks of one value are emitted before the next value
  (`extension/SplitOp.groovy:188-193`).
- If the value is a list (tuple), the splitter picks one element: `elem` if given, else the first `Path` or `File`
  element, else element 0. Each emitted item is a copy of the tuple with that element replaced by the chunk
  (`splitter/AbstractSplitter.groovy:187-213`, `:387-406`).
- Quirk: `elem: 0` is ignored, because 0 is Groovy false, so the default search applies (`splitter/AbstractSplitter.groovy:302-303`).
  A negative `elem` means "the n-th `Path` or `File` element"; if there is none it is an error "Cannot find splittable file".
- `elem` given as a list splits several tuple elements and merges the chunks back into one tuple, like `pe: true`
  (`extension/SplitOp.groovy:97-100`, `:127-150`).
- Splittable objects for the text splitters: `Path`, `File`, `CharSequence`, `Reader`, `InputStream`, `char[]`
  (`splitter/AbstractTextSplitter.groovy:124-145`). Files ending in `.gz` are decompressed unless `decompress: false`
  (`splitter/AbstractSplitter.groovy:451-460`).
- STOP after the source completes. Output: queue.
- `autoClose` is rejected. `into` must be a channel if given (`extension/SplitOp.groovy:103-109`).

Common options: `by` (records per chunk), `limit` (max records per source value), `each` (closure applied to each
emitted item), `elem`, `decompress`, `into`, and for text splitters `charset`, `file`, `compress`
(`splitter/AbstractSplitter.groovy:312-322`, `splitter/AbstractTextSplitter.groovy:96-102`).

| Operator | Extra options | Emitted chunk |
| --- | --- | --- |
| `splitCsv` | `sep`, `strip`, `header` (true or list of names), `quote`, `skip` | a list of fields per row, or a map when `header` is set; with `by: n`, a list of rows. `file` and `compress` are not allowed (`splitter/CsvSplitter.groovy:101-111`) |
| `splitText` | `keepHeader` | lines including a trailing `\n`; `by: n` groups n lines into one string; `file: true` writes chunks to files (`splitter/TextSplitter.groovy`) |
| `splitFasta` | `record` (true or map of fields), `size` (chunk size in bytes, conflicts with `by`) | FASTA text chunk or record map (`splitter/FastaSplitter.groovy:47-62`) |
| `splitFastq` | `record`, `pe` | FASTQ text chunk or record map; `pe: true` splits two files of a tuple in parallel and merges the chunks back into one tuple (`splitter/FastqSplitter.groovy`, `extension/SplitterMergeClosure.groovy`) |
| `splitJson` | `path` | each element of the JSON array at `path`, or for an object each entry as a map `[key: name, value: value]`; only `autoClose`, `into`, `limit`, `path` allowed (`splitter/JsonSplitter.groovy:36-37`, `:71-79`, `:113-235`) |

- `splitCsv` skips empty lines. A missing field in a header row gives `null` for that column; extra fields are
  dropped (`splitter/CsvSplitter.groovy:154-175`). With `header: true` a missing or empty header is an error (`:134-143`).
- `file: true` mode stores chunks in cacheable files under the work directory. It conflicts with `record`, and
  `compress` needs it (`splitter/AbstractTextSplitter.groovy:81-86`).

### 5.34 `take`

- `take(n)` (`extension/TakeOp.groovy:49-82`). `n < 0`: all values. `n > 0`: the first `n`
  values, then STOP; later source values are ignored.
- `n == 0`: STOP is written to the output at call time, before any downstream read cursor exists (`extension/TakeOp.groovy:54-57`).
  A downstream operator never sees it. Experiment: `channel.of(1,2).take(0).toList().view()` never ended.
- A value source is an error. Output: queue.

### 5.35 `tap`

- `tap { name }` creates a new channel named `name` in the workflow scope and returns another new channel. Both receive
  every source value (`extension/TapOp.groovy:57-113`). Same kind as the source. Several names give several channels.
  A name that already exists in the script context gives a warning.
- `tap` cannot be used on the right side of `|` (`modules/nf-lang/src/main/java/nextflow/script/dsl/WorkflowDslV1.java:81`).
- The strict parser treats it as a declaration of `name`, like `set`.
- Experiment: `of(1,2,3).tap { t1 }.map { v -> v + 1 }` gives `2,3,4`, and `t1` gives `1,2,3`.
- Value source: the value is forwarded, but the operator never stops and the run never ends (experiment, section 1.5).

### 5.36 `toList`, `toSortedList`

- `toList()`: at completion emits a list of all values in arrival order. Empty source: `[]`. Value source: `[v]`;
  empty value source: `[]` (`extension/ToListOp.groovy:47-68`).
- `toSortedList([closure])`: `toList` then sort by natural order, or with the closure (1 parameter: sort key;
  2 parameters: comparator).
- Docs: `toList` equals `collect(flat: false).ifEmpty([])` (`docs/reference/operator.mdx` section `toList`).
  One difference by source: `toList` emits a plain `ArrayList`, while `collect` emits an `ArrayBag` that task hashing
  treats as unordered (section 5.5). Experiment: the classes were `ArrayList` and `ArrayBag`.
- Output: value.

### 5.37 `toInteger`, `toLong`, `toFloat`, `toDouble`

- Convert each value with Groovy `as` (`extension/OperatorImpl.groovy:999-1013`). Same kind as the source.
  A failed conversion throws and aborts the run (section 1.4).

### 5.38 `transpose`

- `transpose([by: index | [indexes], remainder: bool])` (`extension/TransposeOp.groovy:65-142`).
- A non-list value is emitted unchanged.
- For a list value: the transposed positions are the `by` indexes (each must hold a list, else an error), or all
  positions that hold a list. If there are none, the value is emitted unchanged.
- For `i = 0, 1, ...`: build a tuple with element `i` of each transposed list and the other positions unchanged.
  Without `remainder`, stop at the shortest list. With `remainder: true`, go to the longest list and use `null` for
  missing elements.
- Consequence: without `remainder`, a tuple with an empty transposed list emits nothing. With `remainder`, a tuple whose
  transposed lists are all empty emits nothing (`extension/TransposeOp.groovy:109-130`).
  Experiment: `of(['a', [], [1]], ['b', [1,2], []]).transpose()` emitted nothing.
- Output: queue, order preserved.
- Experiment: `['a',[1,2,3],['x','y']]` gives `[a,1,x] [a,2,y]`; with `remainder: true` also `[a,3,null]`.

### 5.39 `unique`

- `unique([closure])`: drops a value when its key (`closure(v)` or `v`) was already seen (`extension/OperatorImpl.groovy:315-363`).
- Output: same kind. Experiment: `1,1,2,2,1` gives `1,2`.

### 5.40 `until`

- `until(closure)`: emits values until the closure is true for a value. That value is not emitted. STOP is emitted and
  the operator stops (`extension/UntilOp.groovy:41-58`).
- Output: same kind. Experiment: `of(1,2,3).until { v -> v == 2 }` gives `1`.
- Value source: the value is emitted (if the condition is false), but the operator never stops and the run never ends
  (experiment, section 1.5).

### 5.41 `view`

- `view([newLine: bool, tag: String], [closure])` (`extension/OperatorImpl.groovy:369-404`).
- Prints `closure(v)` (or `v`) with `toString()`, then forwards `v`. `newLine: false` prints without a newline.
- With `tag`, it prints only when `-dump-channels` matches the tag; it still forwards.
- Output: same kind, order preserved. At completion it closes its output with `CH.close0`.

