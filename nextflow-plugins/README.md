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

## src/observer.rs

The events of a real run that an `Observer` hears, in order, modelled on Nextflow's trace
observers. A simulated run, like `uwe check`, has no observers.

| Event | Carries |
| --- | --- |
| `RunStarted(Start)` | run name, session id, launch, work and project directories, main script, project name, repository, commit id and revision of a pulled pipeline, command line, profile, config files, params, config, `manifest` scope, process graph, start time |
| `TaskSubmitted`, `TaskStarted`, `TaskCompleted`, `TaskCached`, `TaskFailed` (`Task`) | trace record, process id, full hash, executor, staged input files, output files (end events), input values |
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
folders to themselves), `native_id`, `cpu_model`, `rchar`, `wchar`, `rss`, `vmem`,
`peak_vmem`, `%mem`, `vol_ctxt` and `inv_ctxt`. Input digests are known at the end of a task
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

