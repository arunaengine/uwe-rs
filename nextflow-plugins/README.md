<!-- Contracts for native nf-schema and nf-validation parameter and sample-sheet checks. -->
<!-- Copyright (c) 2026 The iunctus Contributors -->
<!-- SPDX-License-Identifier: MIT or Apache-2.0 -->

# Source contracts

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

## src/nf_schema/mod.rs:173

The errors of an instance, in nf-schema's words, by the path of the value they concern.
Missing required fields become one error of the object, like `Missing required field(s):
a, b`; `what` names the fields, like `field` or `parameter`. With `lenient`, types are
checked leniently.

## src/nf_schema/params.rs:12

`validateParameters`: checks the params against the schema. Params that the schema does
not know give a warning, unless the config ignores them. Ignored params that the schema
knows are checked like the others, but as in nf-schema, the errors that start with the
name of an ignored param are dropped. Text from the command line stays text, so `--n 7`
is not an integer.

## src/nf_schema/samplesheet.rs:433

The samplesheet as a structure: YAML and JSON as they are, CSV and TSV as a list of
maps when the schema's items have properties, else of lists, or of single values for
rows with one field. The text of CSV and TSV fields becomes numbers, booleans or null
where it reads as such.

