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

## src/nf_irods.rs

nf-irods 0.1.2 hands its `irods` scope (`host`, `port`, `username`, `password`, `zone`,
`authenticationScheme`, `sslNegotiationPolicy`) to the iRODS provider of the `storage` crate
when the run starts, which then reads and writes `irods://` paths for every language, over
uwe's own client of the iRODS protocol of iRODS 4.3 and later, without icommands. Unset values
come from the `IRODS_*` variables and the iRODS environment file, as in the plugin. uwe does not
read the obfuscated password file `.irodsA`, so a PAM password must come from the config or
`IRODS_PASSWORD`. Without a policy, uwe asks the server for `CS_NEG_REFUSE`. A Nextflow path
keeps one slash after the scheme, so `irods:///zone/a` reaches uwe as `irods://zone/a`, which
names the same data object. A copy between `irods://` paths goes through this machine instead
of the server, and making a directory makes nothing, as a write makes its collections.

## src/nf_python/mod.rs

nf-python 0.1.4 runs each `pyFunction` call as a fresh Python process in a container, never on
the host: the image of `nf_python.container`, else the Python 3.12 image of uwe's Snakemake
support, with the launch folder and the call folder mounted at their own paths. The program
of `nf_python.executable` or `_executable` is the one inside the image; `conda_env` fails with
a message, as uwe runs no conda program. The call folder is `<workDir>/.nf-python/<h2>/<h30>`,
where the work directory is `workDir` of the config, else `work` in the launch folder; uwe's
own `nf_python` module goes to `<workDir>/.nf-python/lib`. The container gets only the call's
variables, not the environment of the run. Integers of any size come back, where the plugin
fails above 32 bits, and a `VersionNumber` comes back as its text.

## src/nf_r_ipc/mod.rs

nf-r-ipc 0.2.2 sends the arguments and gets the result of each call as one Arrow IPC stream,
with the plugin's tables and value graph, and runs uwe's own R launcher, `launcher.R`, in a new
container per call: the image of `_docker_image` or `nfR.docker_image` for the `docker`
runtime, else `rocker/r-ver:4.5.1`; `_executable` and `_container_rscript` name the R program
inside it. The launcher installs `nanoarrow` into `<workDir>/.nf-r-ipc/library` when the image
has none. The scratch folder of a call is `<workDir>/.nf-r-ipc/nfr-<n>`, kept after the call,
and is mounted at its own path, so `docker_workdir` has no effect. Refused with a message: the
`conda` runtime and `_conda_env` (no conda program) and `_docker_container`, as uwe runs no
command in a running container; `docker_args` are ignored with a warning. `asZonedDateTime`
gives an object that prints as Java's `ZonedDateTime`, without its methods.

## src/nf_report/mod.rs

nf-report 1.1.1 and nf-crg-report 1.1.10 write the plugins' execution, task status and sample
status reports as JSON and TSV with their keys and texts, after each event as the plugins do.
The HTML is uwe's own page of the same data, as the plugins' templates are their content; a
user template of `htmlTemplatePath` gets the report's values for `$name` and `${name.key}`
placeholders, and one with other Groovy code is skipped with a warning. The email is not sent
(a warning says so). The trace has no `error_action`, so a failed attempt is `RETRIED` when a
later attempt of the same task id follows, else `IGNORED` unless it ended the run. uwe keeps no
task folders, so `workdir`, `log`, `out` and `err` are null, and `rchar` and `wchar` stay null.
In meta map mode the first entry of the meta map is the first in key order, as observers get
inputs as JSON objects. nf-crg-report fetches its prices once a run, when the run starts, and
writes its raw costs as JSON numbers like `4.35e-8` where the plugin writes `4.35E-8`.

## src/nf_fl86/mod.rs

nf-fl86 0.6.4 guards the output folder, prints its two lines and copies the sample sheet and the
Benchling file when the run starts, writes `params.json` when the workflow begins and
`workflow.json` and `<workDir>/pipeline.complete` after a successful run, as the plugin does. uwe
does not tell plugins about a resumed run yet, so the guard stops any run into an existing
folder. `getAWSSecret` and the Action's token call Secrets Manager's `GetSecretValue` with the
shared AWS signer instead of the `aws` program, with the region of the call, `aws.region` or
the environment, and the keys of the `aws` scope or AWS's default chain. Published S3 objects
keep their tags through `GetObjectTagging` and `PutObjectTagging`; `aws.client.endpoint`
selects a path-style endpoint. The Notion and Seqera requests run on a background worker.
Not done: Fusion tags (uwe has no Fusion), the per-task `resourceLabels` (no plugin hook for
process directives), tags of published S3 folders, and the Platform watch URL, which is empty.

## src/nf_seqlab_progress/mod.rs

nf-seqlab-progress 0.2.0 keeps the plugin's task states, stages, completed files, error count
and publish counts, prints the plugin's plain and JSON lines on standard output when they
change, and appends every state to `<outdir>/pipeline_info/progress.jsonl` in every mode. In a
terminal it prints uwe's own text view of the same data instead of the plugin's animation. uwe
does not tell observers where a running task's folder is, so the tasks' progress snapshot files
are not read: active files are always indeterminate. A task's identity comes from its meta map,
as uwe gives observers no names of `env` inputs. The number of publish copies in flight is
unknown in uwe, and the audit file is written only to a local folder.

## src/nf_jev.rs

nf-jev 0.2.1 sends the plugin's request bytes, so a cache folder can be shared with Nextflow
runs. Its settings are checked, and the warning about caching a floating model is printed, when
a run of a config whose `plugins` block names nf-jev starts, where the plugin does it when the
include is loaded. A network error after three attempts gives the error's text where the
plugin gives `null`.

## src/nf_labrobotics.rs

nf-labrobotics 0.1.0 needs Nextflow 26.10, so it was specified, not run. Its `labrobotics`
executor runs a task with uwe's own executor; for a script task whose `executor` directive is
`labrobotics`, the driver adds `NF_LAB_ADAPTER`, `NF_LAB_POLL` and `NF_LAB_GRACE` of the
`labrobotics` scope, `NF_LAB_SESSION` (uwe's session id is the run name), the capability of the
`hints` directive as `NF_LAB_CAPABILITY` and the `queue` as `NF_LAB_QUEUE`. uwe accepts `hints`
like the other placement hints, which it reports and ignores. A stopped container task gets
`SIGTERM` and 10 seconds before it is killed, which gives `labctl` time for its safe stop. The
plugin's start and end lines are template greetings and are not printed.

## src/nf_databricks.rs

nf-databricks 0.1.0 is nf-sqldb 0.7.0 with Databricks' JDBC driver. uwe runs the same functions
and maps `jdbc:databricks:` URLs of SQL warehouses (`httpPath=/sql/1.0/warehouses/<id>`) onto
the SQL Statement Execution API: the password is the personal access token, `ConnCatalog` and
`ConnSchema` or the URL's schema pick the catalog and schema, and `ssl=0` selects HTTP. A batch
of `sqlInsert` is one statement with named parameters. Refused with a message: clusters
(`/sql/protocolv1/...`), OAuth (`AuthMech` other than 3), and results larger than the inline
limit of the API (`EXTERNAL_LINKS` is not read).

## src/nf_gpt.rs

nf-gpt 0.4.0 fails on Nextflow 26.04.6 before it sends a request, so uwe builds the working
calls from its documented behaviour: all functions use `gpt.endpoint`, where the plugin's data
functions always call `api.openai.com`, and a configured `gpt.temperature` and `gpt.maxTokens`
apply, where the plugin loses them. Requests go through the shared HTTP client, with the
retries of `gpt.retryPolicy` on network errors, 429 and 5xx.

## src/observer.rs

The events of a real run that an `Observer` hears, in order, modelled on Nextflow's trace
observers. A simulated run, like `uwe check`, has no observers.

| Event | Carries |
| --- | --- |
| `RunStarted(Start)` | run name, session id, launch, work and project directories, main script, project name, repository, commit id and revision of a pulled pipeline, command line, profile, config files, params, config, `manifest` scope, process graph, start time, whether the run resumes |
| `TaskSubmitted`, `TaskStarted`, `TaskCompleted`, `TaskCached`, `TaskFailed` (`Task`) | trace record, process id, full hash, executor, staged input files, output files, standard output and error (end events), input values, output declarations with their values and file patterns |
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

`Background::ticking` starts the same worker with a tick: its handler gets `Some(event)` for
each event and `None` whenever the tick passes without one, so it can send on a timer, like
nf-tower's batched task records and heartbeats or nf-slack's delayed progress updates. A
stream of events delays the ticks, so the handler checks its own deadlines on every call.

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

## src/nf_cloudcache.rs

nf-cloudcache 0.6.2 is a mapping onto uwe's store: a Nextflow run with `cloudcache.enabled =
true` keeps its store at `cloudcache.path`, else `NXF_CLOUDCACHE_PATH`, else `workDir`, unless
`-store` names one; uwe reads the config for it before the run opens the store. Only the
boolean `true` turns it on, also without a `plugins` line. A local place fails the run with the
plugin's message. The store keeps uwe's format, not Nextflow's `index.<run name>` and task
entries. The warning for a path without `enabled` has a colon where the plugin has a long dash.
`-with-cloudcache` is not an option of uwe.

## src/nf_oci.rs

nf-oci 0.0.1 hands its `oci` scope (`profile`, `region`, `tenantId`, `userId`, `fingerprint`,
`privateKey`) to the OCI provider of the `storage` crate when the run starts, which then reads
and writes `oci://` paths for every language through the native Object Storage API, with the
API key of `~/.oci/config` or the four values of the scope. Each object is written with one
`PUT`, so objects over 50 GiB fail; copies between `oci://` paths go through this machine.

## src/nf_lakefs.rs

nf-lakefs 0.7.0 hands its `lakefs` scope (`apiUrl`, `accessKey`, `secretKey`, `autoCreateBranch`,
`autoCreateBranchSource`, `readTimeout`) to the lakeFS provider of the `storage` crate, which
then reads and writes `lakefs://` paths for every language. `lakefsCommit(path, message)`
commits the branch of the path. With a `lakefs://` `outputDir`, the branch is made when the run
starts (`autoCreateBranch`) and committed when it succeeds (`autoCommit`), with
`params.lakefs_commit_message`, else uwe's message `Run <run name>`. Where it differs: data
always moves through presigned addresses, so `transferMode = 'physical_path'` and
`allowedSchemaBucketsForLinking` only warn; `connectTimeout` is uwe's own 30 seconds; the
staging link sends the ETag of the upload as `checksum`; a ref is used as given, without the
plugin's branch, tag and commit lookups; deleting a `lakefs://` path is not supported.

## src/nf_lamin.rs

nf-lamin 0.9.0 is partly implemented: its `lamin` scope (`api_key`, `api.supabase_api_url`,
`api.supabase_anon_key`, `features.manage_s3_credentials`) goes to the LaminDB provider of the
`storage` crate, which then reads `lamin://` paths for every language. The run is not recorded
in LaminDB yet, with one warning. The prod hub's anon key is not shipped with uwe, so it must
come from the scope or `SUPABASE_ANON_KEY`; `lamin.env` other than `prod` needs
`api.supabase_api_url`. A 401 answer does not fetch a new token, and S3 session credentials are
not renewed during a run.

## src/nf_quilt.rs

nf-quilt 1.0.1 publishes a Quilt package for each package that the run's files went to:
`quilt+s3://` targets, and plain `s3://<bucket>/<p1>/<p2>/<rest>` targets as package `p1/p2`.
At the end of a successful run (or with `force`), it writes `nf-quilt/config.json`,
`params.json` and `workflow.json`, `README_NF_QUILT.md` and `quilt_summarize.json`, then the
manifest with the metadata of the `quilt` scope and the run, and prints `SUCCESS: <uri>`.
Where it differs: files go to S3 as they are published, not at the end, so a failed run leaves
them without a package version; templates take `${name}`, `.get('key')`, `['key']`, `.key`
and `?.` lookups only, other Groovy fails as `compileMessage.FAILED`; JSON keys are sorted; the
`workflow` values are uwe's (plain strings, `nextflow` 26.04.6, `stats` counts) and the README
lists them; bucket access is not checked before; Quilt workflows are not checked. A plain
`s3://` file needs a local copy to be hashed.

## src/nf_blocks.rs

nf-blocks 0.11.0-beta.1 is implemented in its first step: the stores of the `cas` scope go to
the `cas://` provider of the `storage` crate, the writable one by `lineage.store.location`.
Files published to `cas://<alias>/...` become content blocks and coordinates. At the end of the
run the observer writes, in strict DAG-CBOR, an `OutputItem` per published file of each
workflow output, the `OutputCollection`s, `InputSet`, `ItemLineage`, `RunManifest` and
`RunCompletion`, then the Store Log entry. `channel.fromStore(run: ..., output: ..., where:
[...])` reads a run's items before any task runs. Where it differs: uwe cannot default
`outputDir` to the store, so it must be set; items hold the published file alone, not the
published value with its maps, so `where:` finds nothing in uwe's own runs; the input set is
empty, the lineage names a task by the file name, `nf_run_hash` is null and the manifest
config is JSON; `records: true` gives plain maps; a path prints as `/<cid>/<name>` like other
remote paths; `asserted_by` does not come from Seqera Platform or `git config`. Not
implemented (step 2, after the beta): Selections, Claims, DirectoryManifest folders,
`lid://` references, the SQLite index, the explorer page and the plugin commands.
