<!-- Contracts for workflow schema inference, upgrading, and input validation. -->
<!-- Copyright (c) 2026 The iunctus Contributors -->
<!-- SPDX-License-Identifier: MIT or Apache-2.0 -->

# Source contracts

## src/check.rs

Checks the values of a run against the schema of its inputs before any task starts, with the
same messages in every language: a required input without a value, a value of the wrong
type, a value outside an enum, and a file or directory that does not exist or is of the other
kind. Files at remote addresses are not looked at yet.

## src/lib.rs

The inputs of a workflow as one JSON Schema (decision 24). Every driver describes the inputs
of its workflow as a `workflow_graph::Inputs`: a JSON Schema of draft 2020-12, the values
of the run, and the inputs that the workflow code reads. This crate checks the values
against the schema before any task starts (`check`), compares the schema with what the
code reads (`compare`), makes a schema from default values (`infer`), brings schemas of
older drafts to 2020-12 (`upgrade`), tells whether a schema is valid (`valid`), and writes
the inputs as text for help (`text`).

Plain JSON Schema keywords describe types, enums, defaults, `title` and `description`. Files
and directories use the keywords of nf-schema, which nf-core pipelines use:

- `format`: `file-path` for a file, `directory-path` for a directory, `path` for either, and
  `file-path-pattern` for a glob pattern of files;
- `exists`: `true` when the file or directory must exist before the run;
- `mimetype`: the media type of a file, like `text/csv`;
- `schema`: the schema of the content of a file, like the rows of a sample sheet: an array of
  objects with typed columns. nf-schema names a schema file there; iunctus puts the schema
  itself there, so that the schema of a workflow needs no other files.

Two keywords are iunctus's own, for what CWL declares and JSON Schema has no word for:

- `fileFormat`: the format of a file as the IRI of a concept, like
  `http://edamontology.org/format_1930`, or a list of such IRIs;
- `secondaryFiles`: the patterns of the files that come with a file, like `.bai`.

## src/upgrade.rs

Brings a schema of an older draft, like the draft 7 of nf-validation or the drafts 4 to 7 of
Snakemake workflows, to draft 2020-12: `definitions` become `$defs` with their references,
`dependencies` become `dependentRequired` and `dependentSchemas`, a list of `items` becomes
`prefixItems`, and the boolean `exclusiveMinimum` and `exclusiveMaximum` of draft 4 and the
boolean `required` of draft 3 take the forms of draft 2020-12. Parts that are no schema, like a
list of names under `properties` or `additionalProperties: null`, which validators skip, are
left out.

## src/check.rs:78

Checks the values of a run against the schema of its inputs. A simulated run assumes its
input files, so a file that does not exist is only a warning there, and so is a required
input without a value when the run makes up such values (`Inputs::assumes_values`). Every
problem of an advisory schema (`Inputs::advisory`) is a warning, and the values of ignored
inputs (`Inputs::ignored`) have none. A schema that is not valid gives one error.

## src/infer.rs:32

The schema of one value: its JSON type, with the items of a list whose items share a type
and the properties of an object. A text that names an existing file is a `file-path` that
must exist; one that names an existing directory is a `directory-path`, which may be made by
the run, like a folder of results. `null` gives a schema without a type.

