<!-- What wir is, its versioning rule, types, channel contract, op semantics and verifier checks. -->
<!-- Copyright (c) 2026 The uwe Contributors -->
<!-- SPDX-License-Identifier: MIT or Apache-2.0 -->

# wir

wir (workflow intermediate representation) is one typed graph format for workflow languages. CWL, Nextflow, Snakemake, Argo and later WDL are lowered into it, so a graph can be checked, changed and run the same way whatever language it came from.

This crate is the format only: the document types, their JSON form, a JSON Schema, a value check and a verifier. It depends on no other uwe crate, so other tools can use it alone. The runner that executes wir graphs and the language frontends are separate.

```rust
let document = wir::parse(&text)?;          // JSON form, version checked first
wir::verify(&document)?;                     // every problem, with the op path
let ok = wir::fits(&value, &port.r#type, &document.types); // run-time check of a value
```

`wir::SCHEMA` is the JSON Schema (draft 2020-12) of the JSON form. It checks the shape and the name patterns; the verifier checks the rest. The examples in [examples/](examples/) show a graph of two CWL-like tools, a whole CWL workflow as one `native` op, and a Nextflow pipeline as one `native` op with profiles and a config file.

## Document

A document has a `wir_version`, the `sources` (documents in a workflow language that ops come from or run), named `types`, named `graphs` and the `entry` graph that a run starts. A graph has typed input and output ports, ops and edges. An op has a `path` that is unique in its graph and stays stable across edits, a `kind` with the fields of that kind, typed input and output ports, `after` dependencies, `effects`, `attributes` and an `origin` in a source.

Names (sources, types, graphs, ports) use letters, digits, `_`, `-` and `.`. An op path is one or more names joined by `/`. Run settings and provenance name an op as `<graph>/<path>`.

The JSON form uses snake_case keys and leaves out fields that hold their default (an empty list, no value, flow `value`, scatter mode `dot`). Parsing rejects unknown fields, so nothing is dropped silently. Writing a parsed document gives the same JSON value back, apart from defaults written out in the input.

## Versioning

`wir_version` is `<major>.<minor>`, and `wir::VERSION` is `0.1`. A reader accepts only the versions it knows, and the parser checks the version before anything else.

- wir stays at 0.1 while the format grows, until it covers most of what the workflow languages need.
- From 1.0, a minor version only adds optional fields, op kinds, or semantics for reserved op kinds; a reader then accepts its own major version up to its own minor. Any other change raises the major version.

## Types

| Type | JSON form | Values |
| --- | --- | --- |
| `bool`, `int`, `float`, `string` | `"int"` | JSON values; `int` is a JSON integer in the signed 64-bit range |
| File | `{"file": {"format": IRI, "secondary_files": [...]}}` | `{"class": "File", "location", "basename", "format", "size", "checksum", "secondary_files", "contents"}` |
| Directory | `"directory"` | `{"class": "Directory", "location", "basename", "listing"}` |
| Record | `{"record": [{"name", "type"}]}` | JSON object with those keys |
| Array | `{"array": {"items": T, "nonempty": true}}` | JSON array |
| Map | `{"map": {"key": "string" \| "int" \| "bool", "value": T}}` | JSON object; an `int` key is its decimal text |
| Optional | `{"optional": T}` | the value or null |
| Union | `{"union": [T, ...]}` | a value of one member |
| Enum | `{"enum": ["a", "b"]}` | one of the strings |
| Tuple (a pair is a tuple of two) | `{"tuple": [T, ...]}` | JSON array of that length |
| Named | `{"named": "Sample"}` | the value of the named type |
| Dynamic | `"dynamic"` | any value, checked with `fits` where it meets a typed port |

Numbers: `int` is a signed 64-bit integer and `float` an IEEE 754 double. An `int` widens to a `float` on an edge and in a value check; a `float` never narrows to an `int` implicitly. JSON values hold no NaN or infinity.

A port of type `want` accepts a value of type `found` when one of these holds:

- either side is `dynamic`;
- they are equal, or `want` is `float` and `found` is `int`;
- `want` is `Optional<T>` and T accepts `found` (an optional value needs a `select` before it reaches a required port);
- `found` is a union and `want` accepts every member, or `want` is a union and one member accepts `found`;
- both are enums and `found`'s symbols are among `want`'s, or `want` is `string` and `found` is an enum;
- both are files, `want` names no format or the same format (no ontology reasoning yet), and every required secondary file of `want` is a required secondary file of `found`;
- both are records with no fields beyond `want`'s, each field accepts `found`'s field, and missing fields are optional;
- both are arrays whose items accept, and `found` is nonempty where `want` is; maps with the same key type and accepting values; tuples of equal length whose items accept.

Named types are resolved first and must not be defined through themselves.

A secondary file pattern is a suffix added to the primary basename; each leading `^` first removes one extension, as in CWL.

## Channel contract

Every port has a flow kind, `value` or `stream`. Both ends of an edge have the same kind; nothing converts between them implicitly.

A `value` port is bound at most once and never changes. Any number of readers read it, each gets the same value, and reading does not use it up. A bound null (an optional with no value) is a value: a reader runs with null. A port whose producer failed stays unbound, which is not null.

A `stream` port carries items in the producer's order and then closes exactly once. Every reader sees every item from the first, in that order, however many readers there are. A stream that closes with no items is empty: it is neither null nor an error. A producer failure ends the stream with that error instead of a close. Items of different streams have no order between them unless an op defines one.

No op of this version reads or writes streams; `merge` and `flatten` are defined on values only. The rest of the stream contract belongs to the reserved ops: per-round reads of a process with several stream inputs, values reused in every round, Nextflow `each`, and completion order against fair order in `merge`.

## Defined ops

**Start.** An op starts once every connected input is bound and every `after` dependency holds. An unconnected optional input is bound to null. An op whose input can no longer be bound, or whose dependency can no longer hold, does not run and ends `omitted`.

**Value edges.** An edge binds the consumer input to the producer's value once the producer has succeeded. One output may feed many inputs; every input and every graph output has at most one producer. An `int` value that flows into a `float` port becomes a float.

**Defaults.** An input port of an op or a graph may have a `default`, a value that fits its type. When the input is bound to null, or has no producer, it takes the default; any other value stays. So a port with a default also takes null from an edge, and its op or graph always sees a value of its type. An op's defaults apply when it starts, a graph's defaults when it runs: for the entry graph to the inputs of the run, else to the inputs of the `call`, the `branch` body or the round of a `map`, after the defaults of that op. Output ports take no default. A relative `location` of a File or Directory in a default starts from the folder of the document.

**`task`** runs one command. A runner gives it a fresh working folder, makes its input files and folders available, and builds the argument vector from `command`: text stays text, a scalar input becomes its text (`true` or `false` for a bool), a File or Directory becomes its path, an array one argument per item, and null no argument. There is no shell. An input argument may have a `prefix`, an argument before the value, joined to it when `separate` is false; then false and an empty array give no argument, true only the prefix, and an array the prefix once before its items, as in CWL. A `separator` joins the items of an array into one argument. Then every output is read by its one capture:

- `{"glob": pattern}`: paths in the working folder that match, sorted by path. One match fills a File or Directory output, all matches fill an array output. No match fills an optional output with null and fails the task otherwise; several matches fail a non-array output; an empty match fails a nonempty array.
- `{"patterns": input}`: paths that match any pattern that the task input holds, a string or an array of strings, like a glob per pattern, in the order of the patterns; each pattern stays inside the working folder.
- `"stdout"` and `"stderr"`: a File with what the command wrote there.

A capture with `load_contents` puts the text of each File it reads into `contents`, like CWL `loadContents`; a file over 64 KiB fails the task. The output must hold Files.

`workdir` is the absolute path of the working folder in the container, like the output folder of CWL; without it the runner picks one. `stdout` and `stderr` name the files in the working folder that standard output and standard error go to, like CWL `stdout` and `stderr`, as text or as `{"input": name}`, the string value of a task input, where null or an empty string names none; a stream capture reads that file, and a glob capture may match it too. Without a name, a stream capture writes to a file that the runner names.

The task succeeds when its exit code is in `success_codes` (only 0 when empty) and every capture worked. Then all outputs are bound at once; otherwise it ends `failed` and binds none. A task that could not run at all, such as a missing image or a lost node, ends `errored`. The command runs without network unless the task sets `network: true`; this is part of the task and enters its cache key, so a task without network can share its results between runs. The attributes never enter its cache key.

**`call`** runs a named graph of the same document. The op has exactly the graph's port names, with types the graph accepts on the input side and the op accepts on the output side. Its outputs are bound at once when the graph has finished and every graph output is bound; the call fails when an op inside fails. A graph must not call itself, directly or through other graphs.

**`native`** runs a source document as a whole in its own language runtime, such as a complete CWL workflow. `source` names a document source with the same `language`, and `entry` what to run inside it. `profiles` lists profiles of the language, like Nextflow `-profile`, and `configs` names document sources of the same language that configure the run, like Nextflow `-c` files, both in order. The op's ports are the boundary: inputs go in by name, and outputs are exported by name and checked against their port types. These types must not contain `dynamic`, except the outputs of a Nextflow block: Nextflow emits have no types, so a block declares the output `dynamic` to take what an emit gives as it is. The block keeps its own acceptance rules and its own task cache keys, and its tasks are recorded under the op path. A failed or missing required output fails the op.

**`map`** runs its `body` graph once per round. The inputs named in `over` are arrays; each round takes one item of each, and every other input as it is, so the body has the op's port names with the item type for an `over` input. The rounds of `mode`:

- `dot` (the default): round i takes item i of every `over` input. The inputs must have equal lengths, or the op fails before any round starts.
- `flat_cross`: one round per combination of items, the first `over` input varying slowest.
- `nested_cross`: the rounds of `flat_cross`, with every output nested one level per `over` input, in their order.

Each output is the array of the values that the rounds gave for it, in round order, whatever the order in which the rounds end; a round that gives null puts null at its place. An empty `over` input gives no rounds and empty arrays; for `nested_cross` the levels of the inputs before it stay, so lengths 2 and 0 give `[[], []]`. The op succeeds when every round succeeds; a failed round fails the op and binds no output, and a failure inside a round is the failure of the op. Rounds start as their inputs allow, like calls; round i of the op `p` runs its ops at `p/<i>/<op>`, and a task keeps the path without the round numbers as its step. `parallelism` and `min_success` stay reserved: a runner of this version refuses a `map` that sets them.

**`expr`** computes its one output from its inputs, in the engine and without a task. With `language` `js`, `code` is a CWL expression: text with `$(...)` parts or one `${...}` function body, evaluated as CWL evaluates it, so one `$(...)` keeps the type of its value and text with parts becomes a string. `library` holds code that runs first, like CWL `expressionLib`. The code sees the op's input values as `inputs`, the input `self`, when there is one, also as `self`, and an empty `runtime`. A File or Directory looks as in CWL: with `path`, `nameroot`, `nameext` and `secondaryFiles` beside the wir fields. The value must fit the output type, where an integral number fits an `int` in the signed 64-bit range; a File or Directory value keeps the wir fields. An expression that throws, takes too long or gives a value that does not fit fails the op. An `expr` is pure: it reads no files and has no effects, and equal inputs give equal outputs. Other languages stay reserved: a runner of this version refuses them.

**`branch`** runs its `body` graph when its input `condition`, a bool, is true. The body has the op's ports apart from the input `condition`; each output of the op holds the body's output or null. When the condition is true the op runs like a `call` of the body, and it succeeds, fails or errors as that call does. When it is false the body does not run: the op ends `skipped` and binds every output to null, so the ops that read them run with null; inside a `map` that round gives null at its place. A dependency on `succeeded` does not hold for a skipped op, one on `skipped` does. A null condition fails the op.

**`select`** picks among the items of its one input, an array, like CWL `pickValue` and WDL `select_first` and `select_all`; an item is set when it is not null. Its one output, by `rule`:

- `first`: the first item that is set. When none is set, also for an empty array, the op fails.
- `only`: the one item that is set. When none or more than one is set, the op fails.
- `all`: the array of the items that are set, in their order; it is empty when none is set.

**`merge`** on value ports gives one array of its input values, in the order of its input ports, like CWL `merge_nested`: each input is one item, so an array input is one item and a null input a null item. **`flatten`** on a value port takes one array and gives the items of each item that is an array, then each other item as it is, in order, like CWL `merge_flattened` after a `merge`: one level deep, so an empty array adds nothing and a null item stays. A null input fails either op.

The output of `select`, `merge` and `flatten` must fit its type, or the op fails. They run in the engine without a task and are pure, like `expr`. `merge` and `flatten` on stream ports stay reserved: a runner of this version refuses them.

**`after` dependencies** name another op of the same graph and the statuses that satisfy it: `succeeded`, `failed` (ran and not accepted), `errored` (could not run), `skipped` (its own condition was false) and `omitted` (did not run because a dependency did not hold). All dependencies of an op must hold. Data edges and dependencies together must not form a cycle.

**Effects** declare what an op does beyond its outputs: `{"publish": target}`, `{"external": target}` and `"nondeterministic"`. Removing ops whose outputs nobody needs keeps every op with an effect. Effectful work stays in `task` and `native` ops.

**Attributes** never enter a task cache key: `cache` (`default` reuses a result with an equal key, `never`, `resume` reuses only results of the resumed run; no setting overrides a source's own veto on reuse), `place` (a site or a rule; explicit placement never falls back silently), `resources` (`cpus`, `gpus`, `memory` and `disk` in bytes), `retry` (more attempts after a failed or errored one) and `time` (seconds per attempt). A binding file per run may set them by op path without changing the document.

## Reserved ops

These op kinds parse and verify (their body graphs and the op inputs they name must exist), but no runner of this version runs them. Their fields are fixed; their semantics are open.

| Op | Fields | Open |
| --- | --- | --- |
| `collect` | none | order; an error inside the stream |
| `filter` | `body` | order; end of stream |
| `flatten` | none | on streams: the same rule for items of a stream |
| `merge` | none | on streams: completion order against fair order |
| `zip` | `remainder` | unequal lengths |
| `cross` | none | order; empty input |
| `group` | `by`, `size`, `remainder` | Nextflow `groupTuple` rules; order of groups |
| `join` | `by`, `remainder`, `fail_on_mismatch`, `fail_on_duplicate` | duplicate keys; mismatch errors; order |
| `loop` | `body`, `condition`, `carry` | the first check; outputs of every round; limits |
| `dynamic` | `plan` | typed boundaries; stable instance ids; reconciling with finished work |

## Verifier

`verify` reports every problem it finds, each with where it is (`document`, `types/<name>`, `<graph>` or `<graph>/<op path>`):

- the version is known;
- names are valid and unique: sources, types, graphs, ports on each side, op paths in a graph, record fields;
- named types exist and are not defined through themselves; unions, enums and tuples are not empty;
- the entry graph and every graph, source and op named by an op exist; graphs do not call themselves;
- every edge joins existing ports of equal flow, and the target type accepts the source type;
- every input and graph output has at most one producer, and every required one without a default has a producer;
- data edges and `after` dependencies form no cycle, and every dependency lists a status;
- a task has a command, uses only its own inputs, and has exactly one fitting capture per output;
- an expr has one output; a select and a flatten have one array input and one output, and a merge at least one input and one array output, each output accepting what the op gives; a branch has a bool condition, the body's other ports and optional outputs; a call matches the called graph's ports, and a map the ports of its body, with arrays for the `over` inputs and the outputs;
- a native op's source and config sources exist in its language, and none of its ports uses `dynamic`, apart from the outputs of a Nextflow block;
- the stream files of a task are relative paths that stay inside the working folder;
- a default sits on an input port and fits its type;

Not yet: the readable text form, and lowering passes other than the CWL lowering of the crate `cwl-lowering`. The runner is the crate `wir-runtime`.
