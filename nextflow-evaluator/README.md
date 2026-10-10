<!-- Contracts for legacy Groovy lowering, interpretation, and library stand-ins. -->
<!-- Copyright (c) 2026 The iunctus Contributors -->
<!-- SPDX-License-Identifier: MIT or Apache-2.0 -->

# Source contracts

## src/interpreter/library/org_json.rs

Stand-ins for `org.json` (`JSONObject`, `JSONArray`, `JSONTokener`) and the everit JSON
schema validator (`SchemaLoader`, `Schema`, `ValidationException`). Nextflow releases
before 24 shipped both, and the `lib/` classes of older nf-core pipelines use them
(decision 14). The host validates, as the
evaluator has no JSON schema validator of its own.

## src/lower/mod.rs:59

Lowers the value of one config setting. Outside closures, config code may only use
`params`, the directory constants, `secrets` and `env()`. Closures may also use the names of
scripts. In the `process` scope, a closure may use any other name too, like the task input
`meta`, which is looked up when the task runs.

## src/lower/process.rs:645

The script of a `shell:` section, whose text interpolates `!{expression}` instead of
`${expression}` and keeps `$` for the shell, as Nextflow's `TaskTemplateEngine` does with
the placeholder `!`. The strings that make the value of the section become strings that
interpolate each `!{expression}`.

## src/lower/workflow.rs:41

Legacy syntax: `name = value` also sets the variable, which later
emits may read, as in `sample_size = meta.count()`. A variable of
the body with that name keeps its value, for the closures that read
it later.

## src/interpreter/classes.rs:446

The method that Groovy chooses for the arguments, with the arguments it takes: the right
number, remaining arguments collected for a method with variable arguments, and of the
methods whose parameter types fit, the one with the closest types. A method of the
objects has `this` as its first parameter, which the arguments leave out.

## src/interpreter/mod.rs:426

Starts a script that an include with params loads: its `params.x = value` declarations
set what its params lack, then its statements run, as Nextflow runs the script when the
include runs.
Runs the statements of a script, then those of the scripts it includes without
params and outside `if` statements, once each, as scripts run them when they load.

## src/interpreter/task.rs:222

The values of a legacy `file` input, where each value that is not a path becomes a
file with its text, named `input.N` after the files of the task before it, as
Nextflow's `normalizeInputToFile` does. The file goes below `work/tmp` of the launch
directory, in a folder named by the MD5 hash of its text.

## src/interpreter/library/config_slurper.rs:18

`new ConfigSlurper().parse(text)`: the settings of a config text as nested maps. Blocks
and selectors like `withName: 'FOO' { ... }` are maps of their own, by name or
pattern. As a `ConfigObject`, each map makes an empty map of a key it lacks, and a name
that the text does not define, like `params`, is such a map too, so that
`"${params.outdir}"` gives `[:]`.

## src/interpreter/library/yaml.rs:14

`yaml.load(text)`: mappings become maps and sequences lists; plain scalars are resolved
as YAML 1.1 does, so `1.13` is a number and `yes` a boolean. An empty text gives `null`.
An alias gives the value of its anchor, and a merge key `<<` adds the entries of a map, or
of a list of maps, that the mapping does not have, as SnakeYAML does.

