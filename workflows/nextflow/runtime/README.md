<!-- Contracts for pipeline inputs, output collection, reporting, and task construction. -->
<!-- Copyright (c) 2026 The uwe Contributors -->
<!-- SPDX-License-Identifier: MIT or Apache-2.0 -->

# Source contracts

## src/cleanup.rs

The cleanup that nf-gc and nf-boost's `boost.cleanup` turn on. Task results stay: they are in
the store, which other runs, the cache, published files and workflow outputs share. It deletes
only the run's own files that tasks read, like those of `collectFile`, the splitters and
`exec:` sections, once every node that sent such a file and every node that reads from those
nodes have stopped, and none of them is a workflow output. A file that a delivery takes stays.
A path that moves outside channels, like in a variable or in a plugin object, stays unseen and
so stays too. The modes of `nfGc.gc_mode` and `boost.cleanup` free files at the same moments;
`boost.cleanupFailedTasks` and `boost.cleanupInterval` change nothing, since the executor
removes the folder of every attempt when it ends, also of a failed one.

## src/calls.rs

The model calls of agent tasks (nf-agent spec). The driver makes each call on a thread of its
own and looks for answers every 20 ms through the engine's wake-up, so the run goes on while a
model answers; an agent runs at most 10 tasks at a time unless `maxForks` says otherwise. The
engine then records the task, which has neither a command nor an image, like the task of an
`exec:` section. uwe's own cache keys tasks by their image, so answers are kept apart: one file
per call in `<scratch>/nextflow/agents`, named by the SHA-256 of the task name, model, endpoint,
messages, output schema and the size and change time of the input files. A run with `-resume`
reads them for first attempts and sets the time of each file it reads; `uwe clean` removes the
answers that no kept or ongoing run used since it started. `cache false` neither reads nor
writes them. A missing or withheld credential stops the run, as in Nextflow, while a wrong
model id fails only its task. Prompts and keys are never logged, and addresses in messages
leave out credentials.

## src/inputs.rs

The inputs of a Nextflow pipeline as one JSON Schema (decision 24). A pipeline with a
`nextflow_schema.json`, like every nf-core pipeline, has that schema in draft 2020-12, with
the schema of each sample sheet that nf-schema's `schema` keyword names put in its place.
For other pipelines the schema comes from what they declare: the typed `params` block with
its types, and the params of the config and of `params.x = value` statements with the types
of their defaults. The values of a run are its params as nf-schema checks them, and the
params that the code reads are found in the scripts and config files of the project.

## src/notes.rs

What a Nextflow run notes while it plans, for the markers of the graph: the files that the
workflow reads or lists through the host, and the files that operators like `flatten` turn
into items of their own. The engine marks the tasks that made such files. Reads of the
`versions.yml` files of nf-core modules are left out: pipelines read them for the report of
tool versions, never to plan tasks.

## src/output_schema.rs

The outputs of a Nextflow pipeline as one JSON Schema (decision 24): the outputs of the
`output { }` block of the main script, which the pipeline publishes into its output folder,
else the `emit`s of the workflows of the main script, which a pipeline that includes it
gets as channels.

## src/outputs.rs

The workflow outputs of the `output { }` block, as Nextflow's `PublishOp` publishes them:
the task files in each value that a published channel gives go below the output
directory, into the folder of the output's `path`, or where the publish statements of a
`path` closure say. Once the channel ends, an index file lists the values, with the files
at their new places.

## src/reports.rs

Nextflow's own reports of a real run, as the `trace`, `timeline`, `report` and `dag` scopes
of the config turn them on: the trace of every task attempt, a timeline, a report with a
summary and the tasks, and the graph of the processes. nf-core pipelines turn all four on,
with files in `pipeline_info/`. The driver writes them at the end of the run into its own
folder and asks the engine to deliver them, so that a simulated run writes none.

The trace has the columns that Nextflow's has. What uwe does not measure yet, like the
CPU and memory that a task used, or has no counterpart for, like the hash of a work folder,
is `-`, which Nextflow writes for unknown values too.

## tests/exec.rs

Processes with `exec:` sections run in the interpreter when their task is asked for: the
task has no image and no command, its files are those the section made in `task.workDir`,
and the tasks that read them wait for it (tests/fixtures/exec, recorded with Nextflow
26.04.6).

## src/driver.rs:488

In a simulated run, a task only pretends to run, and the simulator makes up the outputs that a
script does not tell how it makes. An output that a script, like a stub, surely does not
make is left out, as Nextflow's stub run finds no file for it: an optional output is then
missing and a required one fails the task, so the run takes the same path. An output that
names an input or the folder of one, without wildcards, keeps it, as Nextflow collects
such an input.

## src/driver.rs:759

What the network reads of the outcome of a task: its output files, its standard output, and
the values of its `env` and `eval` outputs from `.command.env`, which it reads through
`files` when the run has them. A value that a task did not write, as with the simulator, is
empty.
The task of an `exec:` section, which ran in the interpreter when it was asked for (decision
17): it has no image and no command, so the engine finishes it at once (decision 11 asks
no container of it), and it stands in the graph with the files it read.

## src/inputs.rs:222

Whether the pipeline holds its params to its schema itself: a script that it loads includes
`validateParameters` of nf-schema or nf-validation, and the params do not switch the check
off with `validate_params = false`, as nf-core pipelines allow. Nextflow alone checks no param
against `nextflow_schema.json`.

## src/inputs.rs:289

The params that the pipeline tells its check to leave out, like `genomes`, which are kept out
of the schema on purpose: nf-schema's `validation.ignoreParams` and
`validation.defaultIgnoreParams`, and nf-validation's `params.validationSchemaIgnoreParams`,
a text of names joined by commas.

## src/inputs.rs:319

The params that the code of a project reads, found in its scripts, its config files and its
Groovy classes: `params.name`, `params?.name`, `params['name']`, `params.get('name')` and
`$params.name` in a string. An assignment like `params.name = value` declares the param
rather than reading it. A project that includes nf-schema or nf-validation also reads the
params that the plugin reads by name, where its schema declares them, as the plugins read
them only then. A project that calls the plugin's `paramsSummaryLog` or `paramsSummaryMap`
reads every param of its schema: the summary shows each one that is set, like the
`config_profile_name` of an nf-core test profile.

## src/lib.rs:333

What a script declares about its params, before any of its workflow runs: the params of the
config with those of the `params.x = value` statements of the script and its includes where
the config leaves them unset, and the evaluated default of each param of the typed `params`
block, `None` for one without a default.

## src/loader.rs:304

The file an include source names, relative to the including script, as Nextflow's
`realModulePath` finds it: the name with `.nf` added when that file exists, else `main.nf`
of a directory, else the name itself, like `lib/utils.groovy`. Plugins have no file. A
legacy source may start with the directory of the project or of the including script,
like `${projectDir}/modules/x`.

## src/task.rs:83

An image with the registry of the container engine in front, as Nextflow puts it before
images without a registry of their own: `biocontainers/fastqc` becomes
`quay.io/biocontainers/fastqc` with `docker.registry = 'quay.io'`. An image names its own
registry when its first part holds a `.` or a `:`, like `community.wave.seqera.io/...`.

## tests/legacy.rs:128

A `lib/` class like the `NfcoreSchema` of older nf-core pipelines, which reads a schema
with `org.json`, drops an ignored param from it and validates params with everit. The
violations are those of everit 1.14.1, which
`workflows/nextflow/evaluator/tests/fixtures/oracle/org_json/schema.groovy` prints.

## tests/processes.rs:786

The text of a channel, as nf-core/quantms reads `is_sdrf.toString()` of a value that a
workflow emits, which is bound at once, even `null`, while `channel.value` binds its value
when the run starts; and `println()` of a channel in a completion handler, which prints an
empty line. Nextflow 26.04.6 prints the same lines for this script, with the empty line
after `done`, as it writes that line through `System.out` apart.

