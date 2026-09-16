<!-- The inputs of a workflow as one JSON Schema: how each language describes them, the check of a run, and where the schema goes; and the outputs as a second schema. -->
<!-- Copyright (c) 2026 The iunctus Contributors -->
<!-- SPDX-License-Identifier: MIT or Apache-2.0 -->

# The inputs of a workflow

Status: implemented for Nextflow, Snakemake, CWL and Argo, 2026-09-30. This applies [decision](../decisions.md) 24.

iunctus describes the inputs of every workflow, in every language, as one JSON Schema of draft 2020-12, the draft that nf-core uses. The schema says which values a workflow takes, their types and defaults, which are files or directories, and which files are tables with typed columns, like sample sheets. Forms, command-line help and later the WES service can be built on it, the same way for every language.

- `iunctus inputs <workflow>` prints the schema, or the inputs as help text.
- Every `iunctus run` and `iunctus graph` checks its values against the schema before any task starts, with the same messages in every language.
- A run warns where the schema and the inputs that the workflow code reads differ.
- The run database keeps the schema and the values of each run, and the RO-Crate of a run holds the schema and one formal parameter per input.
- `iunctus outputs <workflow>` prints a second schema: the outputs of the workflow, so that one workflow can feed another (section 9).

The code is in the `input-schema` crate (the check, the comparison, schemas from defaults, older drafts, help text), in one module per language (`nextflow-runtime/src/inputs.rs`, `snakemake-runtime/src/inputs.rs`, `cwl-runtime/src/schema.rs`, `argo-runtime/src/schema.rs`) and in `iunctus/src/inputs.rs`.

## 1. The interface

A driver describes the inputs of its workflow with `Driver::inputs`, which returns a `workflow_graph::Inputs`:

| Field | Meaning |
|---|---|
| `schema` | the JSON Schema of the inputs: an object with one property per input |
| `values` | the values of this run by input name, in the form the schema describes; `null` stands for no value |
| `base` | the folder that relative paths in the values start from |
| `read` | the inputs that the workflow code reads, as the driver finds them in the code |
| `assumes_values` | the run makes up values for required inputs without one, as a simulated run of Argo or of CWL without a job file does |
| `advisory` | the workflow itself does not hold its values to the schema, so a check only warns |
| `ignored` | inputs whose values the workflow leaves unchecked on purpose, like nf-schema's `validation.ignoreParams` |

The binary asks for the inputs after it made the driver and before the engine starts it. A driver loads its workflow there and keeps it for `Driver::start`: Nextflow loads its config and main script once, and Snakemake evaluates its Snakefile once. A Snakefile that runs a command while it is loaded, which only a run can do as a task, gives no inputs; its session starts again for the run.

## 2. The schema

Every schema is an object of draft 2020-12 (`$schema` names it), with a `title` and, where the workflow has one, a `description`. Its inputs are its `properties`, and `required` lists those without a default. nf-core pipelines keep their layout: groups of inputs under `$defs`, joined by `allOf`, each with its title; `input_schema::properties` lists the inputs of both layouts. Plain JSON Schema keywords describe types, enums, defaults, patterns, titles and descriptions.

Files and directories use the keywords of nf-schema, which nf-core pipelines use already:

| Keyword | Meaning |
|---|---|
| `format: file-path` | the text names a file |
| `format: directory-path` | the text names a directory |
| `format: path` | the text names a file or a directory |
| `format: file-path-pattern` | the text is a glob pattern of files |
| `exists: true` | the file or directory must exist before the run |
| `mimetype` | the media type of a file, like `text/csv` |
| `schema` | the schema of the content of a file, like the rows of a sample sheet: an array of objects with typed columns. nf-schema names a schema file there; iunctus puts the schema itself there, so that a schema needs no other files |

Two keywords are iunctus's own, for what CWL declares and JSON Schema has no word for:

| Keyword | Meaning |
|---|---|
| `fileFormat` | the format of a file as the IRI of a concept, like `http://edamontology.org/format_1930`, or a list of them |
| `secondaryFiles` | the patterns of the files that come with a file, like `.bai` |

The standard keyword `readOnly` marks inputs that the workflow sets itself, like the artifacts of an Argo workflow or the `pepfile:` of a Snakefile.

A file is a text, its path or address, in every language, also in CWL, whose job files give files as objects: `{"class": "File", "path": "a.fq"}` becomes `"a.fq"`, and a literal file with only `contents` becomes a `data:` address of its content. The values of a form or of a WES request are then the same for every language.

The properties of an object come in the order of their names, as serde_json keeps its maps sorted; the groups of nf-core pipelines keep their order, as `allOf` is a list. The Argo runtime needs sorted maps to follow Go, so the order of the workflow files is not kept.

Older drafts become draft 2020-12 (`input_schema::upgrade`): `definitions` become `$defs` with their references, `dependencies` become `dependentRequired` and `dependentSchemas`, a list of `items` becomes `prefixItems`, and the boolean `exclusiveMinimum`, `exclusiveMaximum` and `required` of drafts 3 and 4 take their later forms. `input_schema::valid` tells whether a schema fits the meta-schema of draft 2020-12 and whether a validator can be made of it.

## 3. The four languages

### Nextflow

| Source | Schema |
|---|---|
| `nextflow_schema.json`, or the file of `validation.parametersSchema` | the schema of the pipeline in draft 2020-12, with its groups; each sample sheet schema that a `schema` keyword names, like `assets/schema_input.json`, in its place |
| no schema file | each param of the config and of the `params.x = value` statements of the scripts, with the type of its default (`input_schema::infer`), without the params of the command line; each param of the typed `params` block with its type (`Path` is a `path`), required when it has no default and may not be `null`, except a `Boolean` |

A text default that names an existing file becomes a `file-path` that must exist, and one that names a directory a `directory-path`. The title is the pipeline's name from its manifest.

The values are the params as nf-schema checks them: those of the config and the command line, with the `params.x = value` statements of the scripts where the config leaves them unset, as Nextflow sets them before the workflow runs. A value that Groovy counts as false, like `null`, `false` or `''`, is left out unless it is zero. In lenient mode (`validation.lenientMode`) a number or boolean counts as text where the schema asks for text. Nextflow itself checks no param against a schema, so a schema is advisory unless the pipeline checks its params: a script it loads includes `validateParameters` of nf-schema or nf-validation, and `validate_params` is not `false`. The params that the pipeline tells its plugin to leave out are ignored: nf-schema's `validation.ignoreParams` and `validation.defaultIgnoreParams`, and nf-validation's `params.validationSchemaIgnoreParams`. Lenient mode is nf-schema's `validation.lenientMode` or nf-validation's `params.validationLenientMode`.

### Snakemake

| Source | Schema |
|---|---|
| `validate(config, schema)` | that schema in draft 2020-12; several calls are joined with `allOf` |
| no `validate` of the config | the values of the config files (`configfile:`, `--configfile`, profiles) with their types, without `--config` values; advisory |
| `validate(table, schema)` of a table, like a sample sheet read with pandas | the config value that names the file of the table, found by the columns of its header, becomes a `file-path` whose `schema` is an array of rows of that schema |
| `pepfile:` and `pepschema:` | the input `pepfile`, a file whose `schema` is the schema of the PEP project; `readOnly` unless the config names it |

Config values that name existing files or directories are marked as such. The values are the config as the last `validate` of it saw it, with the defaults it set, since workflow code may change the config afterwards; without a `validate`, the config as the Snakefile left it. Every Snakemake schema is advisory: Snakemake has held the config to the schemas of `validate` while it evaluated the Snakefile, before the check, and a schema made from the config files only describes the config.

### CWL

Each input of the process becomes a property: `boolean`, `int` and `long`, `float` and `double`, `string`, `File` and `Directory` (paths that must exist, with `fileFormat` and `secondaryFiles`), enums, arrays, records and unions; named types of `SchemaDefRequirement` are resolved. An input whose type allows `null`, or that has a default, is not required. `label` is the title and `doc` the description. The values are the input object of the job file and the command line. Without a job file, a simulated run makes up the required inputs that nothing sets.

### Argo

The parameters of `spec.arguments` are texts, with their `enum`, their `value` or `default` as the default, and their `description`. A parameter without a value, default or `valueFrom` is required, and so is an input parameter of the entrypoint that no argument supplies. The input artifacts of the arguments and of the entrypoint are paths that the workflow sets itself (`readOnly`), with their location as the default. The title is the name of the workflow and the description the annotation `workflows.argoproj.io/description`. A Workflow that runs a WorkflowTemplate gets the arguments of both, joined as a run joins them.

## 4. The check of a run

`input_schema::check` checks the values against the schema before any task starts. It uses the `jsonschema` crate with nf-schema's `format` and `exists` as keywords of their own, which look at local files; other formats are not checked, and remote addresses and glob patterns are left alone until runs may use the network again. Before the check, texts are read as the numbers and booleans that the schema asks for, as the values of the command line are texts. Nextflow and CWL leave out `null` values, which stand for no value there; in Snakemake a `null` is a value, as in JSON Schema.

The messages are the same in every language:

| Problem | Message |
|---|---|
| a required input without a value | the input `input` is required, but has no value |
| a wrong type | the input `threads` must be an integer, but it is the text `many` |
| a value outside an enum | the input `aligner` must be one of `star` or `hisat2`, but it is `bwa` |
| a missing file | the input `reads` names the file `a.fq`, which does not exist |
| a file where a directory belongs | the input `outdir` names `results`, which is a file, not a directory |
| none of the choices of `anyOf` or `oneOf` | the inputs fit none of the choices of the schema: ...; or ... |
| any other keyword | the input `email` is not valid: ... |

A problem is an error, which stops the run with exit code 1 and one line per error, or a warning, which the run prints where it prints the messages of the workflow, and goes on:

- In a simulated run, a file that does not exist is a warning, as the simulator assumes the input files; so is a required input without a value when the run makes up such values.
- Every problem of an advisory schema is a warning: the workflow does not hold its values to it, and a run must not fail where the workflow itself accepts its values.
- The values of ignored inputs have no problems, but they count for required inputs, as in nf-schema.

The checks of each language keep working as they are: nf-schema's `validateParameters`, Snakemake's `validate`, the CWL job check and the checks of Argo. Where both fail, the check of iunctus comes first, before the workflow runs any code.

## 5. The inputs that the code reads

Each driver finds the inputs that its workflow code reads, and the run warns where they differ from the schema: an input that the code reads but the schema does not declare, which a user cannot know about, and one that the schema declares but the code never reads, which does nothing. The drivers look at the code rather than at one run, so that a branch that a run does not take still counts.

| Language | Reads |
|---|---|
| Nextflow | `params.name`, `params?.name`, `params['name']`, `params.get('name')` and `$params.name` in the tokens of every script, config file and Groovy class of the project, outside the `tests` folders of nf-test, which a run does not load; an assignment declares, and a method like `params.each` reads no name. nf-schema and nf-validation read their own params, like `help`, by name, which count where the schema declares them. Ignored params are left out. |
| Snakemake | `config["key"]`, `config.get("key")`, `"key" in config`, `{config[key]}` in shell commands, `snakemake.config["key"]` and `snakemake@config[["key"]]` in scripts, and `lookup(dpath="key/...")`, in the Snakefiles and in the scripts and notebooks of the workflow folder |
| CWL | in a workflow, the inputs that a step or an output takes as a source; in a tool, the inputs bound to the command line and those that its expressions name as `inputs.name` or `inputs["name"]` |
| Argo | `workflow.parameters.name` in the spec and in the templates that it refers to with `templateRef`, `inputs.parameters.name` in the entrypoint, and the input artifacts of the entrypoint; `{{workflow.parameters}}` reads all |

On the test workflows the comparison finds real mistakes, like the CWL tools that read `inputs.apdater1` where they declare `adapter1`, or read `is_paired_end` where they declare `is_spaired_end`.

## 6. The command

```sh
iunctus inputs <workflow file or directory> [-format json|text] [-check] [options of iunctus run]
```

| Option | Meaning |
|---|---|
| `-format json` | the JSON Schema of the inputs (the default) |
| `-format text` | the inputs as help text: each input with its kind, whether it is required, its choices, default and description, in the groups of the schema; hidden inputs are counted |
| `-check` | also checks the values of the launch, as `iunctus run` with the same options checks them: simulated, unless `-executor local` |

The options of `iunctus run` that describe a launch apply, like `-profile`, `-params-file`, `-inputs`, `--<param> <value>`, `-configfile` and `-lang`. The warnings where the schema and the code differ go to standard error. No task runs.

## 7. The run database and the RO-Crate

Version 2 of the run database has two more columns in `runs`: `input_schema`, the JSON Schema of the inputs, and `inputs`, the values that the run checked; `Database::run_inputs` reads them. The RO-Crate of a run holds the schema as `inputs.schema.json`, and each input at its top as a Bioschemas `FormalParameter` of the workflow (`input` of the `ComputationalWorkflow`), with its name, its type as the Workflow Run Crate names it (`File`, `Dataset`, `Collection`, `Text`, `Integer`, `Float`, `Boolean` or `PropertyValue`), whether it is required, its default, its description and its formats. The parameters of the run and the input files that an input names refer to their formal parameter with `exampleOfWork`. The crate stays valid at the level REQUIRED of the Provenance Run Crate profile 0.5.

## 8. Checks and results

- `input-schema`: unit tests of each module, and `tests/check.rs` for the check.
- The drivers: unit tests of the reads of Nextflow scripts and configs, CWL expressions and Argo texts, and of Snakemake tables and paths.
- `run-database/tests/database.rs` and `run-crate/tests/export.rs`: the inputs in the database, the migration from version 1, and the crate.
- The test workflows, without network and without running any task: `just inputs-test-workflows-test` describes every Nextflow and CWL test workflow, and the Argo and Snakemake test-workflows tests describe the inputs of each workflow before its run, as the binary does. Each writes a fixture of counts per workflow, and every schema must be valid.

| Language | Workflows | With a schema | With sample sheets | Stopped by the check | Code reads an undeclared input | Schema declares an unread input |
|---|---|---|---|---|---|---|
| Nextflow (nf-core and WorkflowHub, `-profile test`) | 345 | 280 (235 from `nextflow_schema.json`) | 200 | 7, which nf-schema stops too | 157 | 250 |
| Snakemake | 892 | 685 | 89 | 0 (advisory) | 85 | 65 |
| CWL (the documents of the graph test) | 5,974 | 5,956 | 0 | 0 | 58 | 524 |
| Argo (documents with an entrypoint) | 1,350 | 1,347 | 0 | 0 | 24 | 31 |

Every schema is valid JSON Schema of draft 2020-12. The workflows without a schema are those that cannot load: DSL1 scripts, broken workflows, Snakefiles whose evaluation stops, packed CWL files without `#main`, and 12 CWL documents that load `run` files from the web, which the tests read without network. The seven Nextflow runs that stop at the check fail in nf-schema too, before any task: the check of iunctus now gives the first error, like ``the input `input` is required, but has no value``. With the check before each run, all Argo and Snakemake runs have the outcomes they had before, and the Nextflow runs too, apart from those seven (checked without network, where only reads of remote files differed).

The schemas of some workflows hold parts that are no schema, like a list named `required` under `properties` in nine Snakemake workflows; validators skip them, and so does `upgrade`. The comparison with the code finds real mistakes: in 58 CWL documents the code reads an input that the process does not declare.

## 9. The outputs

A driver describes the outputs of its workflow with `Driver::outputs`, which returns one JSON Schema of draft 2020-12, or `None` when it knows no outputs. The schema is an object with one property per output: the results of a run, as another workflow would take them as its inputs. It comes only from what the workflow declares, and no task runs.

| Language | The outputs |
|---|---|
| Nextflow | The outputs of the `output { }` block of the main script: each a list of paths, with the folder that the pipeline publishes it to below its output folder (`-output-dir`, else `outputDir` of the config, else `results`), and its index file if it has one. Without an `output { }` block, each `emit` of the named workflows of the main script is an output: a channel that a pipeline which includes it gets. |
| Snakemake | The files of the target rules, which are the first rule unless the launch names targets. A rule with outputs gives them; a rule without, like `all`, gives the inputs that it asks for. A named group of files is an output by that name; the other files are the output `targets`. Each output holds its files for the config of the launch as `const`, and `directory()` makes a directory path. |
| CWL | The outputs of the process, with the same types as its inputs, their `label`, `doc`, formats and secondary files. Files and directories are paths that the run makes, so they have no `exists`. An output is required unless its type is optional. |
| Argo | The output parameters of the entrypoint as texts, and its output artifacts as paths, required unless `optional`. Then the global outputs of all templates by their `globalName`, as `workflow.outputs` gives them. |

The command prints the schema, or with `-format text` the outputs as text; the options of `iunctus run` that describe a launch apply, as for `iunctus inputs`:

```sh
iunctus outputs <workflow file or directory> [-format json|text] [options of iunctus run]
```

The tests of the test workflows count the outputs of every workflow with a schema of its inputs, and every schema of outputs must be valid:

| Language | Workflows | With outputs | Outputs | Files or directories |
|---|---|---|---|---|
| Nextflow (nf-core and WorkflowHub) | 280 | 153 | 324 | 145 |
| Snakemake | 683 | 605 | 655 | 655 |
| CWL | 5,956 | 5,805 | 16,708 | 15,853 |
| Argo | 1,347 | 153 | 297 | 123 |

The 179 Nextflow outputs that are not files are emits, whose type the script does not declare. Most Argo workflows declare no outputs at all.

## 10. Next steps

- The schema of the outputs in the run database and the RO-Crate, and a run that takes the outputs of another run as its inputs.
- Checking the content of local sample sheets against their `schema`, and remote files once runs may use the network again.
- The inputs on the web page of a run, and a form made from the schema.

## 11. Embedding an input form

`iunctus::forms` exposes a version 1 form contract for a configured native driver. It keeps the original JSON Schema, including references, compositions and file annotations, rather than flattening it into help properties:

```rust
use iunctus::application::Policy;
use iunctus::forms;

let form = forms::describe(driver.as_mut(), Policy::Strict)?;
let description = form.json();
// Send description to the caller's schema renderer.
let response = form.validate(posted_values);
```

The description contains `version`, `language`, `schema`, effective launch `values`, `outputs` and `validation` policy. A validation response contains `accepted`, an optional error code, unchanged typed `values` and structured `diagnostics`. Each diagnostic retains its instance/schema JSON Pointers, keyword, stable code and alternative branches. The server retains the configured schema and policy; a validation request supplies values only.

Forms use typed JSON validation. The number `4` is accepted for an integer; the string `"4"` is rejected. `input_schema::validate_json` and `application::validate_json` expose the same mode separately. Existing CLI checking still converts strings using the declared types. Neither mode inserts JSON Schema defaults.

`form.check(values)` returns the structured admission result without starting tasks or changing the driver. Before execution, configure the submission's driver with those values and admit it through the application API. Strict policy refuses missing/advisory schemas and simulated assumptions; relaxed policy preserves declared advisory and ignored-input behavior. Local path checks use the server's launch base. Remote content checking remains a separate provider-aware phase.

This is an embedding interface, not a hosted submission server or a browser renderer. The embedding application controls workflow selection, profiles, credentials, authorization and disclosure of schemas and effective values. The run viewer remains a viewer.
