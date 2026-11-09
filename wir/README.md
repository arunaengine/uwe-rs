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

`hooks` lists what runs once a run ends: `{"complete": closure}` runs after every run and `{"error": closure}` after a failed one, each with one object `{success, error}` and captures of the entry graph's inputs; `{"observe": {"source": id, "processes": [...]}}` gives the observers of a source, like Nextflow plugins, the processes of the run with their `id`, `name`, `definition`, `script` and the ids they `feeds`.

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

A secondary file pattern is a suffix added to the primary basename; each leading `^` first removes one extension, as in CWL. A File that a port of a type with secondary files takes or gives lists them in `secondary_files`, an empty list when it has none, as CWL lists them.

## Channel contract

Every port has a flow kind, `value` or `stream`. An edge connects ports with the same flow kind. Conversion between flow kinds requires an op.

A value channel has one of these states:

- waiting;
- bound to one value;
- closed empty;
- failed;
- cancelled.

A bound null is a value. It differs from an empty channel. A bound value never changes and any number of readers may read it. A closed empty value channel has no item. Its consumers end `omitted`, unless an op explicitly handles empty input.

A stream contains an ordered sequence of items followed by exactly one terminal event: close, error, or cancellation. Null is an item when the port type accepts null. Closing without items produces an empty stream.

Each reader has its own cursor. Reading advances only that reader. Detaching a reader does not close the producer or affect other readers.

An edge has a subscription rule:

- `all`, the default: start at item zero;
- `live`: start immediately after the last item committed when the reader is installed.

The installation position of a live reader is fixed once. Replanning the same edge preserves its cursor. A new edge gets a new reader. A reader installed after termination still receives the terminal event.

All readers of an edge must be installed before its consumer starts. A graph activation installs its streams and readers before starting any op in that activation. Dynamic graph adoption installs its new readers as one operation before starting any newly adopted op.

The runner retains a run-local stream log from item zero. It may spill items to disk. It must not discard unread history needed by an `all` reader added later. It must report storage failure instead of silently dropping items.

Items within a stream follow producer order. Items from different streams have no inherent order. Where an op needs an arrival order, it uses the runner's committed event order. Outputs committed together use output declaration order, then item order, to break ties.

A stream error preserves its already emitted prefix. A consumer observes that prefix before the error. Unless an op defines early completion, the error fails the consumer and ends its outputs with an error. There is no rollback of an emitted prefix.

Cancellation stops new rounds, requests cancellation of work owned by the cancelled region, detaches its readers, and ends its open outputs with cancellation. It never publishes buffered results as successful results.

An op that ends successfully cannot later change an output's terminal event. A graph boundary may forward items before the graph finishes, but it holds a successful close until the graph succeeds. A later graph failure therefore produces an error at that boundary.

Defaults apply only to value inputs. A default replaces null or an absent producer, as described below. It does not replace a closed empty channel. Stream inputs have no defaults and require a producer.

At the entry boundary, a stream input is supplied as a JSON array of items. The runner checks each item against the port's item type and closes the input after the array. An empty array supplies an empty stream. A missing required stream input is an input error.

At the result boundary, a successful stream output is represented as an array of its items. An empty value output is omitted from the result object. Null is included as null. A failed or cancelled output cannot become a successful result.

Replay has two meanings:

- A new reader of the same execution reads committed output events. It does not rerun closures, tasks, file writes, or printing.
- A resumed run is a new execution. It rebuilds control and stream state and may reuse accepted task outcomes. Fair order and source order remain stable. Completion order may differ between executions.

The stream log is not a replacement for the task cache or resume database. This contract does not promise transparent restoration of an interrupted interpreter continuation.

## Defined ops

**Start.** An op starts once every connected input is bound and every `after` dependency holds. An unconnected optional input is bound to null. An op whose input can no longer be bound, or whose dependency can no longer hold, does not run and ends `omitted`.

**Value edges.** An edge binds the consumer input to the producer's value once the producer has succeeded. One output may feed many inputs; every input and every graph output has at most one producer. An `int` value that flows into a `float` port becomes a float.

**Defaults.** An input port of an op or a graph may have a `default`, a value that fits its type. When the input is bound to null, or has no producer, it takes the default; any other value stays. So a port with a default also takes null from an edge, and its op or graph always sees a value of its type. An op's defaults apply when it starts, a graph's defaults when it runs: for the entry graph to the inputs of the run, else to the inputs of the `call`, the `branch` body or the round of a `map`, after the defaults of that op. Output ports take no default. A relative `location` of a File or Directory in a default starts from the folder of the document.

**`task`** runs one command. A runner gives it a fresh working folder, makes its input files and folders available, and builds the argument vector from `command`: text stays text, a scalar input becomes its text (`true` or `false` for a bool), a File or Directory becomes its path, an array one argument per item, and null no argument. There is no shell. The file or folder of the input `p` is staged at `inputs/p/<basename>` in the working folder, an item of an array or a tuple at `inputs/p/<index>/<basename>`, one index per level. An input argument may have a `prefix`, an argument before the value, joined to it when `separate` is false; then false and an empty array give no argument, true only the prefix, and an array the prefix once before its items, as in CWL. A `separator` joins the items of an array into one argument. With `shell`, the arguments form one `/bin/sh` command line, like CWL `ShellCommandRequirement`: each is quoted for the shell, apart from `{"raw": text}`, which stays as it is, and the words of an input argument with `quote: false`; without `shell`, raw text is an argument like other text. A `{"code": text}` argument holds code like that of a `js` expr, which the runner evaluates as it builds the task, with the task's `library`: `inputs` holds the input values, each File and Directory with the `path`, `dirname`, `basename`, `nameroot` and `nameext` of the place where the task stages it, `self` the input that `self` names, and `runtime` is empty; when that input is null, the argument gives no word, as CWL binds no null input. Its value binds like the value of an input argument, with the same `prefix`, `separate`, `separator` and `quote`; code that fails fails the task. Then every output is read by its one capture:

- `{"glob": pattern}`: paths in the working folder that match, sorted by path. One match fills a File or Directory output, all matches fill an array output. No match fills an optional output with null and fails the task otherwise; several matches fail a non-array output; an empty match fails a nonempty array.
- `{"patterns": input}`: paths that match any pattern that the task input holds, a string or an array of strings, like a glob per pattern, in the order of the patterns; each pattern stays inside the working folder.
- `"stdout"` and `"stderr"`: a File with what the command wrote there.
- `"empty"`: no paths, for a capture with `code`.
- `"results"`: the field of the output in the object of the `results` file; null when the command writes no such file.

`limits` names a task input whose value, an object, sets the resources of the task over its attributes: `cpus`, `memory` and `disk` in bytes, `gpus`, and `time` in seconds. A field that is null or missing keeps what the attributes set. A value that is no object sets nothing.

`results` names a file in the working folder, like CWL `cwl.output.json`. When the command writes it, its JSON object gives every output in place of the captures: the field of its name, null when it has none. A File or Directory there with a `path`, or else a `location`, names a place in the container, absolute or from the working folder, and becomes the file that the task made or the input staged there. A runner lists the file among the outputs of the task and as its output manifest.

A capture with `load_contents` puts the text of each File it reads into `contents`, like CWL `loadContents`; a file over 64 KiB fails the task. The output must hold Files. With `listing`, each Directory it reads holds its listing, `shallow` or `deep` as with a `load` op.

A capture with `format` sets the format of each File it reads, alone or in arrays, to the value of that code, evaluated like the `code` of a capture with that File as `self`, like a CWL `format` expression; a value that is no string sets none.

A capture with `code` gives the value of that code as the output, like CWL `outputEval`. The runner evaluates it as a `code` argument once the command ended, after `load_contents` and `listing`: `self` is the array of what the rule matched, each File and Directory with its `path` in the container, the Files and Directories of `inputs` have their staged paths, and `runtime` holds `exitCode`. A File or Directory of the value names a place as in the `results` file. For an output of one File or Directory, a list of no or one item is that item or null. The value must fit the output.

`environment` sets the environment variables of the command by name, each text or `{"input": name}`, where a string is the value, another value its JSON text, and null sets none. `stdin` names what standard input reads: a path as text, or `{"input": name}`, whose File gives its path in the container or whose string is the path. `staging` is an absolute folder that the Files and Directories of the task inputs go to before anything else, in the order of the op's inputs, each top-level one with its secondary files in a numbered folder of its own, like the input folder of CWL; `staged` names the inputs that go there, in its order; without it every input goes there; without it, a File or Directory goes below `inputs/<port>` when an argument uses it. `files` lists what the working folder holds before the command starts, like CWL `InitialWorkDirRequirement`. Each entry places a `value`, text or `{"input": name}`, at a `path` in the working folder, text or `{"input": name}` like a stream name: a File or Directory keeps its basename without a path, a Directory without a location is made with its listing, an array places each of its items by basename, an object with an `entry` places that entry at its `entryname`, a string or another value becomes a file of that text or JSON, and null places nothing. A `writable` entry is a copy that the command may change. A File or Directory that an entry places gives its path there as an argument. `workdir` is the absolute path of the working folder in the container, like the output folder of CWL; without it the runner picks one. `stdout` and `stderr` name the files in the working folder that standard output and standard error go to, like CWL `stdout` and `stderr`, as text or as `{"input": name}`, the string value of a task input, where null or an empty string names none; a stream capture reads that file, and a glob capture may match it too. Without a name, a stream capture writes to a file that the runner names.

The task succeeds when its exit code is in `success_codes` (only 0 when empty) and every capture worked. Then all outputs are bound at once; otherwise it ends `failed` and binds none. A task that could not run at all, such as a missing image or a lost node, ends `errored`. The command runs without network unless the task sets `network: true`; this is part of the task and enters its cache key, so a task without network can share its results between runs. The attributes never enter its cache key.

**`call`** runs a named graph of the same document. The op has exactly the graph's port names, with types the graph accepts on the input side and the op accepts on the output side. Its outputs are bound at once when the graph has finished and every graph output is bound; the call fails when an op inside fails. A graph must not call itself, directly or through other graphs.

**`native`** runs a source document as a whole in its own language runtime, such as a complete CWL workflow. `source` names a document source with the same `language`, and `entry` what to run inside it. `profiles` lists profiles of the language, like Nextflow `-profile`, and `configs` names document sources of the same language that configure the run, like Nextflow `-c` files, both in order. The op's ports are the boundary: inputs go in by name, and outputs are exported by name and checked against their port types. These types must not contain `dynamic`, except the outputs of a Nextflow block: Nextflow emits have no types, so a block declares the output `dynamic` to take what an emit gives as it is. The block keeps its own acceptance rules and its own task cache keys, and its tasks are recorded under the op path. A failed or missing required output fails the op.

A `task` with a `snakemake_job` builder runs one job of a Snakemake plan, its rule `call` with its `wildcards`, planned as a run plans it:

- `group` lists the other jobs, each a `source`, `rule` and `wildcards`, that run in the same task as a group job, a pipe or a service. The ports then map to the files of the whole group: `input/<n>` to the files it reads that no job of the group makes, and `output/<n>` to the files of each job in order.
- `targets` lists the targets of the plan when there are several, the `entry` first.
- `config_values` names a source with the config values of the command line, like `--config`: a JSON list of name and value pairs, in order.
- `until`, `omit_from` and `forcerun` list the targets of the options of the same names, and `forceall` forces every job, as the Snakemake options do.

**`map`** runs its `body` graph once per round. The inputs named in `over` are arrays; each round takes one item of each, and every other input as it is, so the body has the op's port names with the item type for an `over` input. The rounds of `mode`:

- `dot` (the default): round i takes item i of every `over` input. The inputs must have equal lengths, or the op fails before any round starts.
- `flat_cross`: one round per combination of items, the first `over` input varying slowest.
- `nested_cross`: the rounds of `flat_cross`, with every output nested one level per `over` input, in their order.

Each output is the array of the values that the rounds gave for it, in round order, whatever the order in which the rounds end; a round that gives null puts null at its place. An empty `over` input gives no rounds and empty arrays; for `nested_cross` the levels of the inputs before it stay, so lengths 2 and 0 give `[[], []]`. The op succeeds when every round succeeds; a failed round fails the op and binds no output, and a failure inside a round is the failure of the op. Rounds start as their inputs allow, like calls; round i of the op `p` runs its ops at `p/<i>/<op>`, and a task keeps the path without the round numbers as its step.

- `parallelism` limits how many rounds run at once. The rounds still start in round order: the next one starts when a running one ends. It is at least 1; a runner refuses 0.
- `min_success` lets the op succeed when at least that many rounds succeed, once every round has ended. A round that did not succeed then gives null at its place in every output, so the items of the outputs must take null, or the op fails. With fewer successful rounds the op fails as without `min_success`.

**`expr`** computes its one output from its inputs, in the engine and without a task. With `language` `js`, `code` is a CWL expression: text with `$(...)` parts or one `${...}` function body, evaluated as CWL evaluates it, so one `$(...)` keeps the type of its value and text with parts becomes a string. `library` holds code that runs first, like CWL `expressionLib`. Parameter references, like `$(inputs.reads.path)`, and text with them evaluate in the engine when there is no `library`; only other code needs JavaScript. The code sees the op's input values as `inputs`, the input `self`, when there is one, also as `self`, and an empty `runtime`. A File or Directory looks as in CWL: with `path`, `nameroot`, `nameext` and `secondaryFiles` beside the wir fields. The value must fit the output type, where an integral number fits an `int` in the signed 64-bit range; a File or Directory value keeps the wir fields. An expression that throws, takes too long or gives a value that does not fit fails the op. An `expr` is pure: it reads no files and has no effects, and equal inputs give equal outputs.

With `language` `wdl`, `code` is a WDL expression, or a WDL command section (`command <<< ... >>>` or `command { ... }`) whose value is its text, with the common indentation removed as WDL removes it. `library` holds WDL declarations like `Int n = count + 1`, evaluated in order before the code; each sees the inputs and the declarations before it, and its value takes its declared type. The code is WDL of the version of the op's `origin` source, else 1.2. The inputs are seen by name, and an input `a.b` as the member `b` of an object `a`, as WDL sees the outputs of a call. A File or Directory is seen as its path. In a command section and its library, a File or Directory of the input `p` is at the path where a task op stages it. The value takes the output type as WDL coerces to a declared type: a pair becomes a tuple of two, a map an object, a struct a record. The functions that read files, `stdout`, `stderr` and `glob` fail, and so do those that write files outside a command section; in a command section, `write_lines` and the other write functions give files below `.wdl/written/` that the command makes before it runs. Other languages stay reserved: a runner of this version refuses them.

With `language` `argo`, `code` is an Argo text with `{{...}}` tags, which the Argo runtime of uwe substitutes as its native run does, with the workflow globals and the inputs by tag name, like `steps.make.outputs.result` or `item`; an array input is the outputs of a loop, which Argo reads as a JSON list. `form` says what the op gives: `text` (the default) the substituted text, `condition` whether it holds as a `when`, `items` the items of a loop from the JSON list of a `withParam` or the JSON object of a `withSequence`, and `labels` the node name suffixes of those items, like `(0:a)`. An argo expr has no library and no captures, and only an argo expr has a `form`. A runner substitutes it in the Argo document of the source that its `origin` names, with the configs of the Argo task or native ops of that source and the inputs of the run as workflow parameters.

**`load`** gives the value of its one input with files read into it, as its one output of the same type. With `contents`, each File of the value, alone or in arrays, holds its text as `contents`, like CWL `loadContents`; a file over 64 KiB fails the op. With `listing`, each Directory of the value holds its `listing`: its entries with `shallow`, and with `deep` the entries of each Directory below it too, like CWL `loadListing`. A listing the value holds stays.

**`branch`** runs its `body` graph when its input `condition`, a bool, is true. The body has the op's ports apart from the input `condition`; each output of the op holds the body's output or null. When the condition is true the op runs like a `call` of the body, and it succeeds, fails or errors as that call does. When it is false the body does not run: the op ends `skipped` and binds every output to null, so the ops that read them run with null; inside a `map` that round gives null at its place. A dependency on `succeeded` does not hold for a skipped op, one on `skipped` does. A null condition fails the op.

**`select`** picks among the items of its one input, an array, like CWL `pickValue` and WDL `select_first` and `select_all`; an item is set when it is not null. Its one output, by `rule`:

- `first`: the first item that is set. When none is set, also for an empty array, the op fails.
- `only`: the one item that is set. When none or more than one is set, the op fails.
- `all`: the array of the items that are set, in their order; it is empty when none is set.

**`merge`** on value ports gives one array of its input values, in the order of its input ports, like CWL `merge_nested`: each input is one item, so an array input is one item and a null input a null item. **`flatten`** on a value port takes one array and gives the items of each item that is an array, then each other item as it is, in order, like CWL `merge_flattened` after a `merge`: one level deep, so an empty array adds nothing and a null item stays. A null input fails either op.

The output of `select`, `merge` and `flatten` must fit its type, or the op fails. They run in the engine without a task and are pure, like `expr`. `merge` and `flatten` on stream ports stay reserved: a runner of this version refuses them.

**`after` dependencies** name another op of the same graph and the statuses that satisfy it: `succeeded`, `failed` (ran and not accepted), `errored` (could not run), `skipped` (its own condition was false) and `omitted` (did not run because a dependency did not hold). All dependencies of an op must hold. Data edges and dependencies together must not form a cycle.

**Effects** declare what an op does beyond its outputs: `{"publish": target}`, `{"external": target}` and `"nondeterministic"`. Removing ops whose outputs nobody needs keeps every op with an effect. Effectful work stays in `task` and `native` ops; a runner refuses effects on other ops. A result of another run never answers a task of an op with effects, since its outputs or its effect may differ: the task reuses only results of the resumed run, as with `cache: resume`. A `native` op with only `publish` effects is the exception, since its block publishes as its language does, also for the tasks it reuses.

**Attributes** never enter a task cache key: `cache` (`default` reuses a result with an equal key, `never`, `resume` reuses only results of the resumed run; no setting overrides a source's own veto on reuse), `place` (a site or a rule; explicit placement never falls back silently), `resources` (`cpus`, `gpus`, `memory` and `disk` in bytes), `retry` (more attempts after a failed or errored one) and `time` (seconds per attempt). A binding file per run may set them by op path without changing the document.

On a `native` op only `cache` and `place` apply, to every task of its block. Its language sets the resources, time limits and retries of those tasks, so a runner refuses `resources`, `retry` and `time` there.

`place` names a site of the executor. Every task of a placed `task` or `native` op runs at that site, and a run that has no site of that name refuses the task. The other ops run in the engine, so a runner refuses a `place` on them when it plans the run.

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
| `reduce` | `rule`, `body`, `compare`, `seed`, `empty`, `nulls`, `steps` | with `steps`, a stream of the value after every item, like nf-boost `scan` |
| `loop` | `body`, `condition`, `carry` | the first check; outputs of every round; limits |
| `dynamic` | `plan` | typed boundaries; stable instance ids; reconciling with finished work |

## Verifier

`verify` reports every problem it finds, each with where it is (`document`, `types/<name>`, `<graph>` or `<graph>/<op path>`):

- the version is known;
- names are valid and unique: sources, types, graphs, ports on each side, op paths in a graph, record fields;
- named types exist and are not defined through themselves; unions, enums and tuples are not empty;
- the entry graph and every graph, source and op named by an op exist; graphs do not call themselves;
- every edge joins existing ports of equal flow, and the target type accepts the source type; only a stream edge reads `live`;
- stream ports take no default, and every stream input of an op has a producer;
- every input and graph output has at most one producer, and every required one without a default has a producer;
- data edges and `after` dependencies form no cycle, and every dependency lists a status;
- a task has a command, uses only its own inputs, and has exactly one fitting capture per output;
- an expr has one output, and an argo expr gives what its form says; a select and a flatten have one array input and one output, and a merge at least one input and one array output, each output accepting what the op gives; a branch has a bool condition, the body's other ports and optional outputs; a call matches the called graph's ports, and a map the ports of its body, with arrays for the `over` inputs and the outputs;
- a Snakemake job names its `targets` from its `entry` on, no empty target, and the jobs of its `group` once each, in its source;
- a native op's source and config sources exist in its language, and none of its ports uses `dynamic`, apart from the outputs of a Nextflow block;
- the stream files of a task are relative paths that stay inside the working folder;
- a default sits on an input port and fits its type;

Not yet: the readable text form, and lowering passes other than the CWL lowering of the crate `cwl-lowering` and the WDL lowering of the crate `wdl-lowering`. The runner is the crate `wir-runtime`.
