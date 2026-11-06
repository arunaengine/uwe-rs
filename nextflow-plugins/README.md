<!-- Contracts for the plugin catalogue, run observers and native plugin functions. -->
<!-- Copyright (c) 2026 The uwe Contributors -->
<!-- SPDX-License-Identifier: MIT or Apache-2.0 -->

# Source contracts

## src/catalogue.rs

Every plugin of the Nextflow registry, read 2026-11-06, with its kind (functions, observer,
file system, executor, other) and what uwe does for it: implemented, partial, planned, not
implementable or out of scope, the last two with a reason. A run warns once for each plugin
of the config's `plugins` block that uwe does not run, and skips its features; `uwe check`
lists the same warnings. An include from `plugin/<id>` of such a plugin fails to load, with
the plugin's state in the message. `capabilities()` reports the catalogue.

### Adding a plugin

Each plugin is one module, `src/nf_<plugin>.rs` with the plugin id's hyphens as
underscores (at most three terms), beside the others; `nf_` is a prefix of distinct plugins,
not a domain. A plugin with three or four files goes into `src/nf_<plugin>/` and gets a
`SMALL_DOMAINS` entry in `scripts/dev/check_style.py`, like `nf_schema`; five or more files
need none. The module defines its `Plugin` value, then two shared lines register it:
`mod nf_<plugin>;` in `lib.rs`, and its value in place of its `planned(...)` line in
`PLUGINS`.

```rust
//! nf-example 1.0: what the plugin does, in one line.
pub(crate) const PLUGIN: Plugin = Plugin {
    functions: &["exampleText"],          // functions that scripts include
    call: Some(call),                     // runs them by name
    methods: Some(methods),               // runs the methods of the objects they give
    factories: &["fromExample"],          // channel factories, `Channel.fromExample`
    scopes: &["example"],                 // config scopes it reads
    observer: Some(activate),             // makes its run observer from the config
    ..Plugin::new("nf-example", Kind::Observer, State::Implemented)
};
fn call(name: &str, arguments: Vec<Value>, context: &mut Context) -> Result<Value, Error>;
fn methods(object: &Value, name: &str, arguments: Vec<Value>, context: &mut Context)
    -> Result<Option<Value>, Error>;
fn activate(config: &Config) -> Result<Option<Box<dyn Observer>>, String>;
```

A config scope is read with `config.get(&["example", "enabled"])` in `activate` or `call`;
`scopes` lists it for `capabilities()`. `activate` returns `None` when the config turns the
plugin off, and an error, which stops the run before it starts, for a config it cannot use.

A function may give an object of its own class: `crate::object(class, state)`, whose state
implements `Native` (its text, and its equality). Scripts call its methods through `methods`,
read a property `x` through `getX()` or `x()`, and set it through `setX(value)`.

`start(context)` runs before the script and `finish(context)` at the end of the run, after
the `workflow.onComplete` handlers, also of a failed run. Each calls the plugins of the
config that need it, by id; they may print, like nf-bactopia's banner and summary.
`finish` gets the finished `workflow` object in `context.workflow`, with its success,
completion time and duration.

A plugin operator, like nf-bids' `groupTupleBy`, is a function of the plugin's `script` that
takes the channel first. When a script includes it from `plugin/<id>`, `ch.groupTupleBy { }`
calls that function with `ch`, which builds on the built-in operators. A name that the plugin
runs natively, like nf-boost's `scan`, stays a built-in operator.

nf-gc and nf-boost's `boost.cleanup` turn on the cleanup of the runtime, whose contract is in
the README of `nextflow-runtime`, section `src/cleanup.rs`.

## src/nf_fairscape/mod.rs

nf-fairscape 0.2.0 writes the EVI RO-Crate and `provenance-graph.json` of a successful run,
with the plugin's ids, entities, inverse links, inputs and outputs. Where they differ:

- The engine Software is uwe, with uwe's version, not Nextflow; the run Computation keeps
  `nextflowVersion` 26.04.6, the Nextflow version whose language uwe follows.
- Task results are in uwe's store, so a task output that is not published has the store path
  as its `localPath`, and its id comes from that path. Task scripts have uwe's indentation.
- Left out, with one warning that names them: the files that port `fairscape-cli`
  (`ro-crate-datasheet.html`, `ai_ready_score.json`, `provenance-graph.html`,
  `ro-crate-linkml.yaml`), and the root's `localEvidenceGraph`, which would name the viewer.
  Also `schemas`, `containerProvenance` and `expandDirectories`, the per-process
  `ext fairscape`, which the run events do not carry, and the grouping of many sibling
  Datasets in the evidence graph.

## src/nf_runinsights/mod.rs

nf-runinsights 0.2.0 keeps its history, findings and report as the plugin does, and prints its
lines on standard output at the end of the run: observers have no host there. It writes no
`~/.nf-runinsights/debug.log`. The history must be a local folder; `s3://` and other stores are
not read. The AI section is one Anthropic Messages API call through the shared HTTP client,
with uwe's own instruction, `ANTHROPIC_API_KEY` or `ANTHROPIC_AUTH_TOKEN`, and
`ANTHROPIC_BASE_URL`; the run's end waits up to 150 seconds for it.

## src/nf_flowviewer.rs

nf-flowviewer 1.0.0 sends the plugin's requests from a `Background` worker. Two fields are
uwe's own: `fileSystem` of an artifact is the URI scheme of its source, `file` for a local
file, where the plugin sends a Java object text; and the graph of `POST /api/processes` is
uwe's process graph: a `PROCESS` vertex per process, numbered from 0, an edge per channel
between processes and an open end for each input and output, all without labels, since uwe's
graph has no operator vertices. `hash` of an artifact is empty, as stored files are not in a
Nextflow work directory. Trace fields that uwe does not measure are null.

## src/nf_arts/mod.rs

nf-arts 1.0.0 checks its `arts` settings with the plugin's messages, uses its palettes, blends
and caption layout, and writes SVG or PNG. The pictures are uwe's own, not the plugin's:
`radial`, `mondrian`, `phyllotaxis` and `ridgeline` are drawn from the tasks' process, start,
time, peak memory and status; `hilbert`, `flowfield`, `tree` and `ginkgo` are drawn as
`radial`, with a warning. The second caption line names uwe. A PNG is drawn by uwe's own small
encoder, without the caption text, since uwe has no font. The file is written on this
machine; the `render` command and cloud paths are not supported.

## src/nf_emoji.rs

nf-emoji 0.2.0: `printEmoji`, `emojiView` and `emojiDump` are Nextflow script in
`nf_emoji.nf`. The observer writes the plugin's themed progress bar, error emoji and summary
on standard error, the bar at the start and after each task that ended rather than every
second. uwe ships none of the plugin's greeting texts: `emoji.greeting = true` prints nothing,
a greeting text of the config is printed. `emoji.confetti` runs no outside program and warns
once.

## src/nf_drs.rs

nf-drs 0.1.0 hands its `drs` scope (`authProvider`, `authSecret`, `tokenEndpointPath`,
`accessProtocol`) to the DRS provider of the `storage` crate when the run starts, which then
reads `drs://` paths for every language. `authSecret` names the environment variable that
holds the credential; uwe does not look that name up among its secrets. Without `authProvider`, uwe uses `gen3` when that
variable is set and reads without a token otherwise, where the plugin fails. uwe sends the
headers of an access URL, which the plugin leaves out. A path's name is the last part of the
address, not the object's `name`, so a staged input is named after its object id.

## src/nf_synapse.rs

nf-synapse 0.1.3 hands its `synapse` scope (`endpoint`, `authToken`) to the Synapse provider of
the `storage` crate when the run starts, which then reads `syn://` paths and publishes into
Synapse folders for every language. Without `authToken`, the token comes from the environment
variable `SYNAPSE_AUTH_TOKEN`; uwe does not look that name up among its secrets, while
`authToken = secrets.NAME` works. uwe follows every page of a folder's children, where the
plugin reads the first one. A path's name is the last part of the address, like
`syn100.fastq.gz`, not the entity's `name`, so a staged input keeps the address's name.

## src/observer.rs

The events of a real run that an `Observer` hears, in order, modelled on Nextflow's trace
observers. A simulated run, like `uwe check`, has no observers.

| Event | Carries |
| --- | --- |
| `RunStarted(Start)` | run name, session id, launch, work and project directories, main script, project name, repository, commit id and revision of a pulled pipeline, command line, profile, config files, params, config, `manifest` scope, process graph, start time |
| `TaskSubmitted`, `TaskStarted`, `TaskCompleted`, `TaskCached`, `TaskFailed` (`Task`) | trace record, process id, full hash, executor, staged input files, output files, standard output and error (end events), input values |
| `FilePublished { source, target }` | each file of a `publishDir` or a workflow output |
| `RunEnded(End)` | success, error message, counts, the task whose failure ended the run, workflow outputs with their files and index file, end time |

The process graph lists the processes in the order the workflow calls them, numbered from
1, each with its qualified and definition name, its script file and the processes that its
outputs reach through channels and operators. Operators are not vertices of it.

The trace record has Nextflow's field names, with times in milliseconds since 1970,
durations in milliseconds and sizes in bytes. uwe fills `task_id`, `name`, `process`,
`status`, `exit`, `attempt`, `container`, `submit`, `start`, `complete`, `duration`,
`realtime`, `cpus`, `memory`, `disk`, `time`, `%cpu`, `peak_rss`, `read_bytes`,
`write_bytes`, `script`, `tag`, `queue` and `hash`. `hash` is the short form, like
`ab/cdef01`, of the SHA-256 by which a resumed run finds the task, not Nextflow's own hash.
These stay null, because uwe does not know or measure them: `workdir` (executors keep task
folders to themselves), `native_id`, `cpu_model`, `rchar`, `wchar`, `vol_ctxt` and
`inv_ctxt`. `rss`, `vmem`, `peak_vmem` and `%mem` stay null too, unless nf-cgroup-metrics is
on: the wrapper samples the cgroup of each container task once per second, and the trace then
takes them, and `peak_rss`, from the samples as that plugin does. Input digests are known at the end of a task
when the executor hashed its inputs.

Nextflow's pending event has no event of its own: it marks a task that waits for its
executor, which is the moment uwe submits the task to the engine, so `TaskSubmitted` covers
it, and `TaskStarted` comes when the executor takes the attempt.

`Observer::check` runs at the end of a run whose tasks succeeded, before `RunEnded`. An
error fails the run with its message, and the observers then hear a failed
`RunEnded`; nf-snapshot uses it.

A task is submitted when the driver hands it to the engine, and an attempt starts when the
executor has it. The executor answers a task from the cache on resume, so such a task is
submitted then cached, without a start. Only `publishDir` and workflow outputs publish files;
the files that `workflow.onComplete` copies are not published.

An observer that talks to a network service wraps its sender in `Background`: a worker
thread that gets the events through a channel, so the run never waits for the service. The
run end waits a bounded time for the events that are left, then leaves the worker behind.
The sender uses the one HTTP client of the process, `storage::http_client()`, never a new
client, and a timeout per request.

## src/validation.rs

nf-validation 1.1.3, the plugin that nf-schema replaced: parameter validation, summaries
and help from a JSON schema of draft-07 with `definitions`, and `fromSamplesheet`. Older
nf-core pipelines use it, and Nextflow 26.04.6 still runs it with the legacy parser
(decision 14). It shares the JSON helpers of
`crate::nf_schema`.

## src/everit.rs:97

Validates the JSON text of an instance against the JSON text of a schema, as
`SchemaLoader.load(schema).validate(instance)` does. The result is `None` when the
instance is valid, else the violation as everit's `ValidationException.toJSON()` gives
it: one problem, or a violation of the whole instance that lists them.

## src/nf_schema/mod.rs:415

The errors of an instance, in nf-schema's words, by the path of the value they concern.
Missing required fields become one error of the object, like `Missing required field(s):
a, b`; `what` names the fields, like `field` or `parameter`. With `lenient`, types are
checked leniently. The `errorMessage` of the failing schema is added in brackets.

## src/nf_schema/params.rs:13

`validateParameters`: checks the params against the schema. When they are valid, the
params that no keyword of the schema evaluates, nested ones as `a.b`, are reported as
`validation.logging.unrecognisedParams` asks (a warning by default), unless the config
ignores them; a core Nextflow option given with two hyphens fails. Ignored params that the
schema knows are checked like the others, but as in nf-schema, the errors that start with
the name of an ignored param are dropped. Before 2.7.2, text from the command line stays
text, so `--n 7` is not an integer.

## src/nf_schema/samplesheet.rs:618

The samplesheet as a structure: YAML and JSON as they are, CSV and TSV as a list of
maps when the schema's items have properties, else of lists, or of single values for
rows with one field. The text of CSV and TSV fields becomes numbers, booleans or null
where it reads as such.

