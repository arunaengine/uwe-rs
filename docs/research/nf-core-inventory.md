<!-- Inventory of Nextflow language and runtime features used by the nf-core .nf scripts -->
<!-- Copyright (c) 2026 The iunctus Contributors -->
<!-- SPDX-License-Identifier: MIT or Apache-2.0 -->

# nf-core script feature inventory

This document counts which Nextflow language and runtime features the nf-core `.nf` scripts use.
The goal is to decide the order in which iunctus implements them.
It covers scripts only.
Config files are covered in a separate document (see [Config files](#config-files)).

## Key findings

- **Scope.** 134 of the 144 pipelines are DSL2 code and are counted.
  The other 10 are DSL1 code, which current Nextflow cannot run.
  The 134 pipelines have 6423 `.nf` files; about half are copies of nf-core modules and subworkflows.
  The modules repo has 2215 `.nf` files and all of them parse.
- **Processes look the same everywhere.** Almost every process uses the directives `label`, `tag`, `container` and `conda`, `tuple` inputs with `val` and `path`, `tuple` outputs with `emit:`, and `when: task.ext.when == null || task.ext.when`.
  Other directives (`memory`, `publishDir`, `errorStrategy` and so on) appear in at most 15 pipelines.
  nf-core sets most of them in config files instead (see nf-core-config.md; not checked here).
  `eval(...)` outputs are in 76 pipelines and 1832 modules repo files.
  The `topic:` output option is in 75 pipelines and 2083 modules repo files.
  `stub:` is in 125 pipelines.
- **Directive values are mostly dynamic.** 11729 directive values in 132 pipelines are a single argument that references a variable, for example `tag "${meta.id}"` or the `container` string that tests `workflow.containerEngine`.
  The strict syntax evaluates these for each task.
  Closure values are rare (28 pipelines).
- **About 20 channel operators do most of the work.** 42 of the 48 documented operators are used.
  21 operators are used by 29 or more pipelines: `map`, `mix`, `unique`, `collect`, `collectFile`, `groupTuple`, `toList`, `set`, `join`, `branch`, `combine`, `first`, `filter`, `flatten`, `ifEmpty`, `distinct`, `splitCsv`, `flatMap`, `multiMap`, `transpose`, `concat`.
  The channel pipe `|` is used by 8 pipelines only.
- **Channel factories.** `empty`, `value`, `of`, `fromPath`, `fromList` and `topic` cover almost all calls.
  The deprecated `Channel.from` is still in 24 pipelines.
  `fromFilePairs` is in 1.
- **Value code is broad.** Pipelines call 217 distinct methods on values, plus 10 getters written as properties (such as `file.text`).
  60 of these 227 names are used by 50 or more pipelines.
  Many are in the Nextflow standard library documentation, but common ones such as `toString`, `split`, `sort`, `get`, `flatten`, `equals` and `eachWithIndex` are not (section 4).
- **The nf-core utility subworkflows carry most of the Java use.** `utils_nextflow_pipeline`, `utils_nfcore_pipeline` and `utils_nfschema_plugin` are vendored in about 114 pipelines.
  They use `org.yaml.snakeyaml.Yaml`, `groovy.json.JsonOutput`, `groovy.text.GStringTemplateEngine`, `nextflow.extension.FilesEx.copyTo`, `new File(...)`, `new URL(...).openConnection()` for Slack and Teams messages, `String.execute()` and `List.execute()` (they run `conda config --show channels`, `sendmail -t` and `mail`), `System.exit` and the `workflow.onComplete` and `workflow.onError` handlers.
  For most of these methods the "Own code" column is between 0 and 5.
- **Plugins.** `nf-schema` is included by 110 pipelines (`paramsSummaryMap`, `paramsSummaryLog`, `validateParameters`, `samplesheetToList`, `paramsHelp`).
  The older `nf-validation` is in 16, `nf-core-utils` in 5, `nf-fgbio` in 2 and `nf-anndata` in 1.
- **Workflow run metadata.** About 114 pipelines read some 20 `workflow.*` properties (for example `workflow.manifest`, `workflow.success`, `workflow.duration`), mostly in the completion functions of the utility subworkflow, and 113 call `workflow.toMap()`.
  100 pipelines call `getVal()` on the result of the `toList` operator, which reads a dataflow value inside the completion handler.
- **New language features are rare.** The output block and the `publish:` section are in 12 pipelines, the `params { }` block in 3.
  No pipeline uses typed processes, records or enums.
  Topic channels are the exception: `channel.topic(...)` is in 76 pipelines.
- **Strict syntax.** 45 pipelines contain at least one construct that the strict parser rejects.
  The most common are statements next to declarations (31 pipelines), files that tree-sitter-nextflow cannot parse (26), `for` loops (22) and `import` (15).
  The modules repo has none.
  Deprecated forms are common: the `when:` section (130 pipelines), the implicit closure parameter `it` (93) and `Channel` instead of `channel` (84).
  The strict parser is the default from Nextflow 26.04 (`docs/strict-syntax.mdx`, lines 14 to 24).
- **GStrings.** Process scripts contain 6614 distinct interpolation shapes, but the top shapes are simple: `${prefix}`, `${meta.id}`, `$args`, `${task.cpus}`, `${task.process}`.
  In scripts, `${name}` and `${a.b.c}` are the most common forms.
  Method calls inside `${...}` occur in 108 pipelines.

## Method

### Corpus

- Pipelines: the text-only nf-core corpus in `~/.cache/iunctus/corpus/nf-core-text/pipelines/<name>/`, 144 pipelines.
  Only `.nf`, `.config` and `nextflow_schema.json` files are present.
  This report reads only the `.nf` files.
- Modules repo: `~/.cache/iunctus/corpus/nf-core-text/modules` (the nf-core/modules repository, `.nf` and `.config` files only), 2215 `.nf` files.
- 10 pipelines are DSL1 code: none of their `.nf` files defines a workflow.
  DSL1 support was removed in Nextflow 22.12.0-edge (`docs/migrations/dsl1.mdx`, line 8), so these pipelines cannot run on any current Nextflow.
  They are left out of every count below.
  They are: cageseq, clipseq, diaproteomics, dualrnaseq, eager, imcyto, kmermaid, mnaseseq, proteogenomicsdb, slamseq.
  Each has a single `main.nf` file.
- That leaves **134 pipelines** with 6423 `.nf` files.
  Of these files, 3135 are the pipelines' own code and the rest are copies of nf-core modules and subworkflows (`modules/nf-core/` and `subworkflows/nf-core/`).

### Corpus revision note

`~/.cache/iunctus/corpus/nf-core-text/revisions.txt` lists, for each pipeline, the branch and the commit that was read.
83 pipelines are at `master`, 18 at `main` and 43 at `dev`.
The file is dated 2026-09-27.
The commit of the modules repo is not recorded in `revisions.txt` (unverified; its files are dated 2026-09-27).
The Nextflow sources cited below are the checkout at `/tmp/nf-research/nextflow`, commit `bbf6ff0` (2026-09-25).
Paths such as `docs/...` and `modules/nf-lang/...` are relative to that checkout.

### Parser

Every `.nf` file was parsed with tree-sitter-nextflow v0.4.0 (`/tmp/tree-sitter-nextflow`) and tree-sitter 0.25.
The grammar mirrors the strict parser grammar `modules/nf-lang/src/main/antlr/ScriptParser.g4` (see the header of `/tmp/tree-sitter-nextflow/grammar.js`).
It also accepts a few older forms that nf-core still uses, for example `for` loops, `finally` and `import`.
All counts come from the syntax tree.
No regular expression runs on raw source text.
Two checks look at the text inside a single syntax node: `!{...}` placeholders inside `shell:` strings, and `$` inside slashy strings, because the grammar does not split those nodes further.

44 of the 6423 pipeline files (in 26 pipelines) contain a syntax error for tree-sitter-nextflow.
They are still counted, with the broken part skipped.
No modules repo file has a parse error.
Section 13 lists the causes.

The tool is the Rust crate `/tmp/iunctus-inventory` (`src/main.rs`).
Run it as `inventory <corpus-root> <out.json>`.
The raw counts are in `/tmp/iunctus-inventory/scripts.json`.
`report.py` turns them into the tables of this document.

### What is counted

- **Channel operators.** A method call counts as a channel operator when its name is in the operator list of `docs/reference/operator.mdx` (the `##` headings) or `docs/reference/stdlib-types/channel.mdx` (adds `groupBy`), and its receiver is a channel.
  The tool decides "is a channel" with a small type inference over each workflow body and over each function that builds a channel.
  A receiver is a channel when it is a channel factory call, a process or workflow call, `X.out`, a `take:` input, a variable assigned from one of these, a name set with `.set { name }` or `.tap { name }`, a helper function called with a channel argument, or the result of an operator on a channel.
  Inside closures, all receivers are treated as values.
  In addition, a few operator names that do not exist on lists, maps or strings (for example `mix`, `groupTuple`, `ifEmpty`) count as channel operators in a workflow body even when the receiver type is unknown.
- **Channel factories.** Method calls on `channel` or `Channel`, checked against `docs/reference/stdlib-namespaces/channel.mdx`.
- **Methods on values.** Every other method call, plus property reads that call a getter on a standard type (for example `file.text`, `path.baseName`, `task.memory.mega`).
  The column "Documented" says whether the name appears in `docs/reference/stdlib-types/*.mdx` or in the typed standard library shims in `modules/nf-lang/src/main/java/nextflow/script/types/shim/`.
- **Classes.** Capitalized receivers (`Math.max`), `new X(...)`, fully qualified names (`groovy.json.JsonOutput.toJson`), `import` declarations and type annotations (`as Map`, `instanceof List`, `catch (IOException e)`, `def foo(Map meta)`).
  Names of processes, workflows, functions, local variables and process inputs are excluded.
- **Global functions.** Calls without a receiver, grouped by where the function comes from: the Nextflow global namespace (`docs/reference/stdlib-namespaces/global.mdx`), a plugin (`include { ... } from 'plugin/...'`), an nf-core utility subworkflow (a file under `subworkflows/nf-core/utils_*`), another function in the same pipeline, or nowhere in the `.nf` files.
  Calls on the `log` and `workflow` namespaces are listed here too.
- **Process features.** Statements before the first section label are directives.
  Inputs and outputs are read from the `input:` and `output:` sections.
- **Everything else** is counted by syntax node kind.

### Columns

- **Pipelines**: number of the 134 pipelines that use the feature in any `.nf` file, including vendored nf-core modules and subworkflows.
- **Own code**: number of pipelines that use it outside `modules/nf-core/` and `subworkflows/nf-core/`.
- **Occurrences**: total uses in all 134 pipelines.
- **Modules repo uses** and **Modules repo files**: total uses and number of `.nf` files in the modules repo.

Tables are sorted by pipelines, then occurrences.
Rows that do not fit a table are given after it as a "long tail" line in the form name (pipelines/occurrences/modules repo uses).

### Limits of the method

- The channel inference is a heuristic.
  A method on an unknown receiver in a workflow body is counted as a value method unless its name exists only on channels.
  Section 3 shows the size of the two uncertain groups.
- The same text counts once per copy.
  A module vendored by 100 pipelines counts in 100 pipelines.
  The "Own code" column and the modules repo columns separate the two sources.
- Groovy classes in a pipeline `lib/` directory are not in the text corpus.
  Calls into them (for example `WorkflowMain.initialise(...)`) are counted as class references, but their bodies are not analysed.

## 1. Definitions, includes and feature flags

| Feature | Pipelines | Own code | Occurrences | Modules repo uses | Modules repo files |
|---|---:|---:|---:|---:|---:|
| `process` | 134 | 123 | 4574 | 2091 | 2091 |
| `workflow (named)` | 134 | 134 | 1987 | 124 | 124 |
| `workflow (entry, unnamed)` | 134 | 134 | 140 | 0 | 0 |
| `function` | 132 | 132 | 2826 | 45 | 23 |
| `legacy params.x = value` | 54 | 53 | 320 | 0 | 0 |
| `output block (output { })` | 12 | 12 | 12 | 0 | 0 |
| `params block (params { })` | 3 | 3 | 3 | 0 | 0 |

| Feature | Pipelines | Own code | Occurrences | Modules repo uses | Modules repo files |
|---|---:|---:|---:|---:|---:|
| `included name` | 134 | 134 | 9806 | 514 | 121 |
| `include from relative path` | 134 | 134 | 8857 | 507 | 118 |
| `include from plugin/...` | 125 | 125 | 820 | 7 | 3 |
| `alias (A as B)` | 99 | 98 | 2156 | 123 | 38 |
| `source path ends with .nf` | 38 | 38 | 384 | 17 | 5 |
| `include from other source` | 1 | 1 | 6 | 0 | 0 |

Plugins included with `include { ... } from 'plugin/...'`:

| Plugin and function | Pipelines | Own code | Occurrences | Modules repo uses | Modules repo files |
|---|---:|---:|---:|---:|---:|
| `plugin/nf-schema (include declarations)` | 110 | 110 | 724 | 4 | 2 |
| `plugin/nf-schema: paramsSummaryMap` | 110 | 110 | 220 | 0 | 0 |
| `plugin/nf-schema: paramsSummaryLog` | 109 | 4 | 110 | 1 | 1 |
| `plugin/nf-schema: validateParameters` | 109 | 3 | 109 | 1 | 1 |
| `plugin/nf-schema: samplesheetToList` | 104 | 104 | 117 | 1 | 1 |
| `plugin/nf-schema: paramsHelp` | 93 | 78 | 168 | 1 | 1 |
| `plugin/nf-validation (include declarations)` | 16 | 16 | 60 | 3 | 1 |
| `plugin/nf-validation: paramsSummaryMap` | 16 | 16 | 22 | 0 | 0 |
| `plugin/nf-validation: paramsSummaryLog` | 16 | 8 | 17 | 1 | 1 |
| `plugin/nf-validation: paramsHelp` | 15 | 7 | 16 | 1 | 1 |
| `plugin/nf-validation: validateParameters` | 15 | 7 | 16 | 1 | 1 |
| `plugin/nf-validation: fromSamplesheet` | 8 | 8 | 8 | 0 | 0 |
| `plugin/nf-core-utils (include declarations)` | 5 | 5 | 29 | 0 | 0 |
| `plugin/nf-core-utils: softwareVersionsToYAML` | 5 | 5 | 5 | 0 | 0 |
| `plugin/nf-core-utils: checkCondaChannels` | 3 | 3 | 3 | 0 | 0 |
| `plugin/nf-core-utils: checkConfigProvided` | 3 | 3 | 3 | 0 | 0 |
| `plugin/nf-core-utils: checkProfileProvided` | 3 | 3 | 3 | 0 | 0 |
| `plugin/nf-core-utils: completionEmail` | 3 | 3 | 3 | 0 | 0 |
| `plugin/nf-core-utils: completionSummary` | 3 | 3 | 3 | 0 | 0 |
| `plugin/nf-core-utils: dumpParametersToJSON` | 3 | 3 | 3 | 0 | 0 |
| `plugin/nf-core-utils: getGenomeAttribute` | 3 | 3 | 3 | 0 | 0 |
| `plugin/nf-core-utils: getWorkflowVersion` | 3 | 3 | 3 | 0 | 0 |
| `plugin/nf-fgbio (include declarations)` | 2 | 2 | 2 | 0 | 0 |
| `plugin/nf-fgbio: readStructure` | 2 | 2 | 2 | 0 | 0 |
| `plugin/nf-anndata (include declarations)` | 1 | 1 | 5 | 0 | 0 |
| `plugin/nf-anndata: anndata` | 1 | 1 | 5 | 0 | 0 |
| `plugin/nf-schema: validate` | 1 | 1 | 1 | 0 | 0 |

Feature flags (`nextflow.*` assignments at the top of a script, `docs/reference/feature-flags.mdx`):

| Flag | Pipelines | Own code | Occurrences | Modules repo uses | Modules repo files |
|---|---:|---:|---:|---:|---:|
| `nextflow.enable.dsl = 2` | 26 | 26 | 31 | 0 | 0 |
| `nextflow.enable.dsl=2` | 1 | 1 | 1 | 0 | 0 |
| `nextflow.preview.output = true` | 1 | 1 | 1 | 0 | 0 |

`nextflow.enable.dsl` is deprecated since Nextflow 25.04 (`docs/reference/feature-flags.mdx`, lines 22 to 26).

No script sets `nextflow.enable.types`, which enables typed processes and workflows (`docs/reference/feature-flags.mdx`, lines 60 to 64).

## 2. Channel factories

| Factory | Pipelines | Own code | Occurrences | Modules repo uses | Modules repo files |
|---|---:|---:|---:|---:|---:|
| `empty` | 132 | 132 | 3575 | 228 | 46 |
| `value` | 121 | 121 | 781 | 2 | 2 |
| `of` | 116 | 46 | 280 | 12 | 6 |
| `fromPath` | 112 | 112 | 764 | 3 | 2 |
| `fromList` | 102 | 102 | 186 | 3 | 2 |
| `topic` | 76 | 76 | 258 | 0 | 0 |
| `from (not in docs)` | 24 | 24 | 50 | 0 | 0 |
| `fromSamplesheet (not in docs)` | 9 | 9 | 10 | 0 | 0 |
| `fromFilePairs` | 1 | 1 | 1 | 0 | 0 |
| `map (not in docs)` | 1 | 1 | 1 | 0 | 0 |

`from` and `fromSamplesheet` are not in `docs/reference/stdlib-namespaces/channel.mdx`.
`channel.from` is deprecated (`docs/tutorials/static-types.mdx`, line 580) but still exists at runtime (`modules/nextflow/src/main/groovy/nextflow/Channel.groovy`, lines 162 and 189).
`fromSamplesheet` is included from `plugin/nf-validation` (section 1) and then called as `Channel.fromSamplesheet("input")`, so this plugin adds a channel factory.
`map (not in docs)` is a single call written `channel.map` in differentialabundance.

Receiver spelling.
The strict parser warns about `Channel` and asks for `channel` (`modules/nf-lang/src/main/java/nextflow/script/control/VariableScopeVisitor.java`, line 744; `docs/strict-syntax.mdx`, lines 506 to 515):

| Receiver | Pipelines | Own code | Occurrences | Modules repo uses | Modules repo files |
|---|---:|---:|---:|---:|---:|
| `channel` | 90 | 88 | 3527 | 247 | 52 |
| `Channel` | 84 | 79 | 2379 | 1 | 1 |

Named options passed to factories:

| Option | Pipelines | Own code | Occurrences | Modules repo uses | Modules repo files |
|---|---:|---:|---:|---:|---:|
| `fromPath(checkIfExists:)` | 89 | 86 | 412 | 2 | 1 |
| `fromPath(type:)` | 5 | 5 | 5 | 0 | 0 |
| `fromFilePairs(size:)` | 1 | 1 | 1 | 0 | 0 |
| `fromPath(maxDepth:)` | 1 | 1 | 1 | 0 | 0 |
| `fromSamplesheet(parameters_schema:)` | 1 | 1 | 1 | 0 | 0 |

## 3. Channel operators

| Operator | Pipelines | Own code | Occurrences | Modules repo uses | Modules repo files |
|---|---:|---:|---:|---:|---:|
| `map` | 133 | 133 | 7451 | 346 | 85 |
| `mix` | 133 | 130 | 6627 | 180 | 43 |
| `unique` | 128 | 44 | 382 | 10 | 3 |
| `collect` | 127 | 125 | 1863 | 65 | 20 |
| `collectFile` | 127 | 127 | 595 | 3 | 3 |
| `groupTuple` | 118 | 118 | 657 | 42 | 27 |
| `toList` | 116 | 116 | 473 | 2 | 1 |
| `set` | 114 | 111 | 1380 | 50 | 17 |
| `join` | 100 | 99 | 1660 | 170 | 58 |
| `branch` | 100 | 99 | 546 | 36 | 27 |
| `combine` | 98 | 96 | 954 | 98 | 39 |
| `first` | 89 | 84 | 1052 | 21 | 11 |
| `filter` | 79 | 76 | 763 | 69 | 20 |
| `flatten` | 74 | 74 | 220 | 1 | 1 |
| `ifEmpty` | 70 | 69 | 753 | 18 | 13 |
| `distinct` | 70 | 70 | 80 | 0 | 0 |
| `splitCsv` | 52 | 51 | 113 | 8 | 7 |
| `flatMap` | 51 | 51 | 185 | 6 | 6 |
| `multiMap` | 45 | 45 | 263 | 21 | 11 |
| `transpose` | 45 | 43 | 140 | 9 | 8 |
| `concat` | 29 | 29 | 76 | 0 | 0 |
| `toSortedList` | 21 | 19 | 66 | 6 | 4 |
| `dump` | 15 | 13 | 178 | 1 | 1 |
| `subscribe` | 15 | 14 | 51 | 8 | 6 |
| `cross` | 11 | 11 | 26 | 0 | 0 |
| `splitText` | 11 | 11 | 26 | 0 | 0 |
| `splitFasta` | 11 | 11 | 14 | 0 | 0 |
| `tap` | 10 | 10 | 28 | 0 | 0 |
| `collate` | 7 | 7 | 12 | 0 | 0 |
| `count` | 7 | 7 | 9 | 0 | 0 |
| `merge` | 7 | 7 | 9 | 0 | 0 |
| `view` | 4 | 4 | 7 | 2 | 2 |
| `reduce` | 4 | 4 | 6 | 0 | 0 |
| `buffer` | 4 | 4 | 5 | 0 | 0 |
| `countLines` | 2 | 2 | 3 | 0 | 0 |
| `last` | 2 | 2 | 2 | 1 | 1 |
| `sum` | 2 | 2 | 2 | 0 | 0 |
| `take` | 2 | 2 | 2 | 0 | 0 |
| `randomSample` | 1 | 1 | 1 | 1 | 1 |
| `splitJson` | 1 | 1 | 1 | 1 | 1 |
| `min` | 1 | 1 | 1 | 0 | 0 |
| `splitFastq` | 1 | 1 | 1 | 0 | 0 |

42 of the 48 documented operators are used on channels. Not used on channels by any pipeline: `countFasta`, `countFastq`, `countJson`, `max`, `until`, `groupBy`.

Named options passed to operators:

| Option | Pipelines | Own code | Occurrences | Modules repo uses | Modules repo files |
|---|---:|---:|---:|---:|---:|
| `collectFile(name:)` | 127 | 127 | 515 | 2 | 2 |
| `collectFile(sort:)` | 114 | 114 | 296 | 0 | 0 |
| `collectFile(storeDir:)` | 114 | 114 | 207 | 0 | 0 |
| `collectFile(newLine:)` | 114 | 114 | 196 | 1 | 1 |
| `groupTuple(by:)` | 92 | 92 | 194 | 4 | 3 |
| `join(by:)` | 58 | 47 | 335 | 13 | 10 |
| `join(remainder:)` | 46 | 33 | 258 | 38 | 11 |
| `splitCsv(header:)` | 46 | 45 | 87 | 8 | 7 |
| `combine(by:)` | 39 | 39 | 251 | 42 | 15 |
| `splitCsv(sep:)` | 37 | 36 | 70 | 7 | 6 |
| `join(failOnMismatch:)` | 20 | 15 | 189 | 45 | 18 |
| `collectFile(keepHeader:)` | 19 | 19 | 52 | 0 | 0 |
| `join(failOnDuplicate:)` | 17 | 14 | 184 | 37 | 15 |
| `dump(tag:)` | 14 | 13 | 177 | 1 | 1 |
| `collectFile(skip:)` | 13 | 13 | 34 | 0 | 0 |
| `groupTuple(sort:)` | 10 | 8 | 40 | 5 | 3 |
| `collectFile(seed:)` | 9 | 9 | 27 | 0 | 0 |
| `groupTuple(size:)` | 9 | 7 | 15 | 2 | 1 |
| `collect(flat:)` | 8 | 7 | 9 | 2 | 1 |
| `splitFasta(record:)` | 7 | 7 | 8 | 0 | 0 |

Long tail, as name (pipelines/occurrences/modules repo uses): `transpose(by:)` (6/28/1), `collect(sort:)` (6/15/0), `collectFile(cache:)` (5/21/0), `groupTuple(remainder:)` (5/9/0), `splitFasta(file:)` (5/6/0), `buffer(size:)` (4/5/0), `splitFasta(by:)` (4/5/0), `splitCsv(quote:)` (3/9/0), `splitCsv(elem:)` (3/8/1), `splitCsv(skip:)` (3/7/3), `splitFasta(elem:)` (3/3/0), `splitText(by:)` (3/3/0), `buffer(remainder:)` (2/3/0), `splitCsv(strip:)` (2/2/0), `splitText(file:)` (2/2/0), `splitText(limit:)` (2/2/0), `splitText(elem:)` (1/3/0), `dump(pretty:)` (1/1/1), `splitJson(path:)` (1/1/1), `splitCsv(by:)` (1/1/0), `splitCsv(limit:)` (1/1/0), `splitFasta(compress:)` (1/1/0), `splitFasta(size:)` (1/1/0), `splitFastq(by:)` (1/1/0), `splitFastq(compress:)` (1/1/0), `splitFastq(file:)` (1/1/0), `splitFastq(pe:)` (1/1/0), `transpose(remainder:)` (1/1/0).

Operators used through the pipe operator `|` or the fork operator `&` (`docs/workflow.mdx`, section "Special operators", line 659):

| Target | Pipelines | Own code | Occurrences | Modules repo uses | Modules repo files |
|---|---:|---:|---:|---:|---:|
| `\| (process or workflow)` | 3 | 3 | 38 | 0 | 0 |
| `\| filter` | 3 | 3 | 8 | 0 | 0 |
| `\| map` | 2 | 2 | 49 | 10 | 2 |
| `\| combine` | 2 | 2 | 12 | 13 | 2 |
| `\| ifEmpty` | 2 | 2 | 6 | 0 | 0 |
| `\| multiMap` | 2 | 2 | 5 | 2 | 1 |
| `\| groupTuple` | 2 | 2 | 5 | 0 | 0 |
| `\| concat` | 2 | 2 | 4 | 0 | 0 |
| `\| branch` | 2 | 2 | 3 | 0 | 0 |
| `\| collect` | 1 | 1 | 11 | 0 | 0 |
| `\| mix` | 1 | 1 | 9 | 0 | 0 |
| `\| join` | 1 | 1 | 8 | 0 | 0 |
| `\| set` | 1 | 1 | 8 | 0 | 0 |
| `\| collectFile` | 1 | 1 | 3 | 2 | 1 |
| `\| multimapChannel` | 1 | 1 | 3 | 0 | 0 |
| `\| first` | 1 | 1 | 2 | 0 | 0 |
| `\| flatMap` | 1 | 1 | 2 | 0 | 0 |

Operators called inside functions that build a channel (for example `softwareVersionsToYAML` in the nf-core utility subworkflow).
These are included in the operator table above:

| Operator | Pipelines | Own code | Occurrences | Modules repo uses | Modules repo files |
|---|---:|---:|---:|---:|---:|
| `unique` | 114 | 1 | 229 | 2 | 1 |
| `map` | 114 | 17 | 183 | 4 | 2 |
| `mix` | 114 | 2 | 118 | 1 | 1 |
| `groupTuple` | 8 | 8 | 9 | 1 | 1 |
| `filter` | 7 | 7 | 9 | 0 | 0 |
| `combine` | 6 | 6 | 8 | 0 | 0 |
| `collectFile` | 6 | 6 | 6 | 0 | 0 |
| `collect` | 5 | 5 | 8 | 0 | 0 |
| `flatMap` | 5 | 4 | 6 | 1 | 1 |
| `branch` | 4 | 4 | 7 | 0 | 0 |
| `concat` | 4 | 4 | 5 | 0 | 0 |
| `splitCsv` | 3 | 2 | 6 | 1 | 1 |

Long tail, as name (pipelines/occurrences/modules repo uses): `multiMap` (3/4/0), `flatten` (3/3/0), `join` (2/3/1), `transpose` (2/3/1), `first` (2/2/0), `ifEmpty` (2/2/0), `distinct` (1/1/0), `dump` (1/1/0), `splitFasta` (1/1/0), `subscribe` (1/1/0).

Uncertain group 1: operator names on a receiver of unknown type in a workflow body, counted as channel operators by name only (included in the operator table above):

| Operator | Pipelines | Own code | Occurrences | Modules repo uses | Modules repo files |
|---|---:|---:|---:|---:|---:|
| `map` | 6 | 5 | 94 | 2 | 1 |
| `mix` | 4 | 1 | 17 | 3 | 1 |
| `collectFile` | 3 | 3 | 3 | 0 | 0 |
| `filter` | 2 | 1 | 3 | 2 | 1 |
| `set` | 2 | 2 | 3 | 0 | 0 |
| `splitCsv` | 2 | 2 | 3 | 0 | 0 |
| `branch` | 1 | 1 | 24 | 0 | 0 |
| `toSortedList` | 1 | 0 | 2 | 2 | 1 |
| `combine` | 1 | 1 | 1 | 0 | 0 |
| `dump` | 1 | 0 | 1 | 0 | 0 |
| `multiMap` | 1 | 1 | 1 | 0 | 0 |

Uncertain group 2: methods that are not operators, called on a receiver that the inference took for a channel.
These are almost all `take:` inputs that hold a plain value (for example a path string).
They are counted as value methods in section 4:

| Method | Pipelines | Own code | Occurrences | Modules repo uses | Modules repo files |
|---|---:|---:|---:|---:|---:|
| `endsWith` | 25 | 25 | 129 | 1 | 1 |
| `contains` | 13 | 11 | 184 | 2 | 1 |
| `split` | 10 | 10 | 149 | 0 | 0 |
| `tokenize` | 6 | 6 | 10 | 0 | 0 |
| `toString` | 5 | 5 | 21 | 0 | 0 |
| `size` | 4 | 4 | 5 | 0 | 0 |
| `toLowerCase` | 4 | 4 | 4 | 0 | 0 |
| `each` | 3 | 2 | 7 | 1 | 1 |
| `any` | 3 | 3 | 5 | 0 | 0 |
| `sort` | 3 | 3 | 5 | 0 | 0 |

Long tail, as name (pipelines/occurrences/modules repo uses): `getName` (3/4/0), `intersect` (2/7/0), `length` (2/4/0), `toInteger` (2/4/0), `findAll` (2/3/1), `readLines` (2/2/0), `equals` (1/45/0), `matches` (1/12/0), `toBoolean` (1/5/0), `substring` (1/3/0), `isDirectory` (1/2/0), `replace` (1/2/0), `trim` (1/2/0), `getClass` (1/1/1), `bind` (1/1/0), `every` (1/1/0), `find` (1/1/0), `getSimpleName` (1/1/0), `isEmpty` (1/1/0), `minus` (1/1/0), `subMap` (1/1/0), `toSet` (1/1/0), `toUpperCase` (1/1/0).

## 4. Methods on values

These are method calls on values (strings, lists, maps, paths, numbers, Java objects), including calls inside closures, functions and process scripts.
Names that are also operators (for example `collect`, `join`, `first`) appear here when the receiver is a value.
Rows marked "(property)" are getter calls written as a property read.

| Method | Documented | Pipelines | Own code | Occurrences | Modules repo uses | Modules repo files |
|---|---:|---:|---:|---:|---:|---:|
| `join` | yes | 132 | 126 | 1866 | 320 | 221 |
| `size` | yes | 132 | 126 | 1432 | 261 | 149 |
| `collect` | yes | 131 | 123 | 1222 | 173 | 126 |
| `toString` | no | 130 | 124 | 2125 | 161 | 87 |
| `tokenize` | yes | 127 | 125 | 953 | 195 | 119 |
| `name (property)` | yes | 126 | 115 | 2366 | 208 | 97 |
| `endsWith` | yes | 126 | 74 | 973 | 275 | 149 |
| `trim` | yes | 121 | 121 | 845 | 30 | 21 |
| `contains` | yes | 120 | 70 | 3682 | 1209 | 250 |
| `intersect` | yes | 120 | 120 | 348 | 118 | 75 |
| `toBytes` | yes | 120 | 7 | 172 | 10 | 8 |
| `text (property)` | yes | 119 | 115 | 416 | 12 | 8 |
| `containsKey` | yes | 118 | 78 | 405 | 14 | 7 |
| `substring` | yes | 118 | 110 | 404 | 9 | 8 |
| `replace` | yes | 117 | 114 | 799 | 57 | 39 |
| `keySet` | yes | 117 | 75 | 544 | 18 | 14 |
| `createTemplate` | no | 116 | 113 | 509 | 3 | 1 |
| `make` | no | 116 | 113 | 509 | 3 | 1 |
| `get` | no | 116 | 20 | 315 | 7 | 6 |
| `withWriter` | no | 115 | 3 | 347 | 3 | 1 |
| `delete` | yes | 115 | 0 | 343 | 3 | 2 |
| `each` | yes | 114 | 109 | 652 | 19 | 12 |
| `sort` | no | 114 | 89 | 506 | 40 | 32 |
| `execute` | yes | 114 | 0 | 342 | 3 | 2 |
| `toPath` | no | 114 | 2 | 321 | 2 | 1 |
| `stripIndent` | yes | 114 | 5 | 248 | 10 | 6 |
| `load` | no | 114 | 4 | 233 | 2 | 2 |
| `collectEntries` | no | 114 | 12 | 134 | 4 | 4 |
| `format` | no | 114 | 1 | 115 | 3 | 3 |
| `dumpAsMap` | no | 114 | 0 | 114 | 1 | 1 |
| `toMap` | no | 114 | 114 | 114 | 0 | 0 |
| `findAll` | yes | 113 | 50 | 385 | 28 | 19 |
| `length` | yes | 111 | 111 | 120 | 0 | 0 |
| `unique` | yes | 110 | 110 | 272 | 14 | 10 |
| `first` | yes | 106 | 15 | 243 | 4 | 3 |
| `baseName (property)` | yes | 103 | 74 | 870 | 167 | 91 |
| `getVal` | no | 100 | 89 | 104 | 0 | 0 |
| `replaceAll` | yes | 96 | 90 | 374 | 29 | 21 |
| `extension (property)` | yes | 95 | 22 | 375 | 67 | 35 |
| `replaceFirst` | yes | 93 | 15 | 215 | 17 | 12 |
| `lastIndexOf` | yes | 84 | 73 | 134 | 10 | 8 |
| `getExtension` | yes | 80 | 14 | 403 | 80 | 47 |
| `flatten` | no | 79 | 76 | 252 | 20 | 13 |
| `withIndex` | yes | 78 | 9 | 88 | 5 | 5 |
| `split` | no | 75 | 64 | 585 | 44 | 22 |
| `toUnit` | yes | 60 | 0 | 64 | 2 | 2 |
| `intValue` | yes | 59 | 6 | 279 | 125 | 117 |
| `getText` | yes | 58 | 2 | 60 | 2 | 2 |
| `equals` | no | 57 | 4 | 159 | 2 | 1 |
| `setRequestProperty` | no | 56 | 0 | 58 | 2 | 1 |
| `openConnection` | no | 56 | 0 | 56 | 1 | 1 |
| `write` | yes | 55 | 3 | 59 | 0 | 0 |
| `getBytes` | no | 54 | 0 | 54 | 0 | 0 |
| `getErrorStream` | no | 54 | 0 | 54 | 0 | 0 |
| `getOutputStream` | no | 54 | 0 | 54 | 0 | 0 |
| `getResponseCode` | no | 54 | 0 | 54 | 0 | 0 |
| `setDoOutput` | no | 54 | 0 | 54 | 0 | 0 |
| `setRequestMethod` | no | 54 | 0 | 54 | 0 | 0 |
| `matches` | yes | 51 | 8 | 210 | 44 | 35 |
| `eachWithIndex` | no | 50 | 11 | 90 | 8 | 7 |
| `toLowerCase` | yes | 49 | 27 | 198 | 28 | 16 |
| `any` | yes | 49 | 25 | 142 | 35 | 19 |
| `getName` | yes | 48 | 41 | 158 | 68 | 36 |
| `exists` | yes | 45 | 43 | 138 | 5 | 3 |
| `mega (property)` | no | 41 | 5 | 240 | 120 | 111 |
| `getBaseName` | yes | 41 | 20 | 150 | 31 | 26 |
| `toGiga` | yes | 36 | 6 | 78 | 60 | 56 |
| `toList` | yes | 36 | 22 | 45 | 8 | 6 |
| `subMap` | yes | 35 | 32 | 326 | 23 | 17 |
| `resolve` | yes | 32 | 12 | 103 | 7 | 4 |

Long tail, as name (pipelines/occurrences/modules repo uses): `copyTo` (30/60/2), `toUriString` (30/43/2), `toInteger` (28/97/7), `startsWith` (27/84/19), `simpleName (property)` (27/78/8), `readLines` (27/71/2), `find` (27/69/29), `isEmpty` (25/86/3), `clone` (24/86/2), `addConverter` (23/92/4), `getCount` (23/51/15), `toJson` (23/24/1), `build` (23/23/1), `excludeNulls` (23/23/1), `toMillis` (23/23/1), `isDirectory` (21/31/11), `add` (20/97/5), `every` (20/40/8), `transpose` (18/41/8), `toFloat` (17/64/5), `last` (17/32/5), `toBoolean` (17/27/0), `getClass` (17/18/2), `remove` (16/32/11), `indexOf` (15/39/6), `findResults` (15/15/1), `giga (property)` (14/48/39), `removeAll` (14/15/1), `getSimpleName` (13/49/16), `map` (13/41/2), `listFiles` (13/37/0), `parseText` (13/25/5), `count` (13/23/0), `drop` (12/30/7), `eachLine` (12/26/1), `collate` (12/16/5), `toMega` (12/14/13), `values` (11/25/2), `isFile` (10/30/2), `collectMany` (10/23/7). Another 117 less used names are listed in `scripts.json` under `other_methods`.

## 5. Java and Groovy classes

Nextflow imports `groovy.lang.*`, `groovy.util.*`, `java.io.*`, `java.lang.*`, `java.math.BigDecimal`, `java.math.BigInteger`, `java.net.*` and `java.util.*` by default.
Any other class needs its fully qualified name (`docs/reference/stdlib-groovy.mdx`; `modules/nf-lang/src/main/java/nextflow/script/control/ResolveVisitor.java`, line 217).
Scripts also see `Path`, `Duration`, `MemoryUnit`, `VersionNumber` and `Channel` without an import (`modules/nextflow/src/main/groovy/nextflow/script/parser/v2/ScriptCompiler.java`, lines 234 to 239 and 310 to 314).

All class names, in any position (static call, `new`, cast, `instanceof`, `catch`, type annotation, `import`).
Lower-case names such as `int` and `long` are primitive types.

| Class | Pipelines | Own code | Occurrences | Modules repo uses | Modules repo files |
|---|---:|---:|---:|---:|---:|
| `System` | 125 | 14 | 147 | 2 | 2 |
| `String` | 124 | 50 | 674 | 31 | 18 |
| `Path` | 124 | 71 | 343 | 10 | 7 |
| `File` | 115 | 11 | 851 | 6 | 1 |
| `Boolean` | 115 | 11 | 334 | 4 | 3 |
| `List` | 114 | 42 | 502 | 93 | 65 |
| `Map` | 114 | 12 | 429 | 5 | 2 |
| `nextflow.extension.FilesEx` | 114 | 0 | 311 | 2 | 1 |
| `Set` | 114 | 5 | 243 | 4 | 3 |
| `org.yaml.snakeyaml.Yaml` | 114 | 4 | 235 | 2 | 2 |
| `groovy.json.JsonOutput` | 114 | 7 | 212 | 3 | 2 |
| `groovy.text.GStringTemplateEngine` | 114 | 0 | 168 | 1 | 1 |
| `IOException` | 114 | 1 | 115 | 1 | 1 |
| `java.util.Date` | 114 | 1 | 115 | 1 | 1 |
| `NullPointerException` | 114 | 0 | 114 | 1 | 1 |
| `groovy.text.SimpleTemplateEngine` | 113 | 113 | 114 | 0 | 0 |
| `MemoryUnit` | 107 | 0 | 160 | 4 | 3 |
| `Exception` | 107 | 11 | 142 | 2 | 2 |
| `org.codehaus.groovy.GroovyException` | 106 | 0 | 106 | 1 | 1 |
| `URL` | 56 | 0 | 56 | 1 | 1 |
| `int` | 32 | 11 | 66 | 27 | 14 |
| `Math` | 25 | 12 | 85 | 24 | 17 |
| `Duration` | 23 | 0 | 46 | 2 | 1 |
| `nextflow.script.types.VersionNumber` | 23 | 0 | 46 | 2 | 1 |
| `LinkedHashMap` | 23 | 23 | 34 | 0 | 0 |
| `groovy.json.JsonGenerator.Options` | 23 | 0 | 23 | 1 | 1 |
| `WorkflowMain` | 18 | 18 | 87 | 0 | 0 |
| `NfcoreTemplate` | 17 | 17 | 75 | 0 | 0 |
| `groovy.json.JsonSlurper` | 16 | 9 | 31 | 5 | 4 |
| `Integer` | 14 | 10 | 76 | 4 | 4 |
| `ArrayList` | 13 | 2 | 13 | 3 | 3 |
| `nextflow.util.MemoryUnit` | 11 | 0 | 12 | 0 | 0 |
| `Yaml` | 9 | 0 | 33 | 0 | 0 |
| `IllegalArgumentException` | 9 | 5 | 20 | 9 | 7 |
| `FilesEx` | 8 | 0 | 24 | 0 | 0 |
| `JsonOutput` | 8 | 0 | 16 | 0 | 0 |
| `java.util.zip.GZIPInputStream` | 8 | 8 | 13 | 1 | 1 |
| `NfcoreSchema` | 8 | 8 | 8 | 0 | 0 |
| `Collection` | 6 | 6 | 13 | 4 | 4 |
| `Float` | 5 | 5 | 14 | 3 | 1 |
| `groovy.yaml.YamlBuilder` | 5 | 1 | 6 | 5 | 5 |
| `double` | 4 | 1 | 21 | 7 | 1 |
| `boolean` | 4 | 4 | 7 | 3 | 2 |
| `Utils` | 3 | 3 | 320 | 0 | 0 |
| `BufferedReader` | 3 | 3 | 9 | 1 | 1 |
| `Double` | 3 | 3 | 6 | 1 | 1 |
| `InputStreamReader` | 3 | 3 | 5 | 1 | 1 |
| `InputStream` | 3 | 3 | 4 | 0 | 0 |
| `Reader` | 3 | 3 | 4 | 0 | 0 |
| `JsonSlurper` | 3 | 1 | 3 | 0 | 0 |
| `Number` | 3 | 3 | 3 | 0 | 0 |
| `groovy.io.FileType` | 3 | 3 | 3 | 0 | 0 |
| `long` | 2 | 2 | 18 | 16 | 8 |
| `BigDecimal` | 2 | 2 | 6 | 0 | 0 |
| `Nextflow` | 2 | 1 | 4 | 0 | 0 |
| `groovy.json.JsonBuilder` | 2 | 2 | 3 | 0 | 0 |
| `float` | 2 | 0 | 2 | 1 | 1 |
| `Arrays` | 2 | 2 | 2 | 0 | 0 |
| `java.lang.reflect.InvocationTargetException` | 2 | 2 | 2 | 0 | 0 |

Long tail, as name (pipelines/occurrences/modules repo uses): `Constants` (1/165/0), `WorkflowOncoanalyser` (1/69/0), `WorkflowCommons` (1/21/0), `URI` (1/10/0), `WorkflowRadseq` (1/8/0), `WorkflowRnasplice` (1/7/0), `RelativePublishFolder` (1/6/0), `HashSet` (1/4/0), `WorkflowIllumina` (1/4/0), `char` (1/4/0), `DumperOptions` (1/3/0), `WorkflowAtacseq` (1/3/0), `WorkflowCircdna` (1/3/0), `WorkflowCutandrun` (1/3/0), `WorkflowHgtseq` (1/3/0), `WorkflowMarsseq` (1/3/0), `WorkflowVariantcatalogue` (1/3/0), `WorkflowViralintegration` (1/3/0), `java.nio.file.Files` (1/3/0), `java.lang.IllegalArgumentException` (1/2/2), `java.util.UUID` (1/2/2), `ANNDATA_MAKEH5AD_single` (1/2/0), `ArrayBag` (1/2/0), `ExposureTime` (1/2/0), `ExposureTimeUnit` (1/2/0), `MissingPropertyException` (1/2/0), `Object` (1/2/0), `PREPARE_GENOME` (1/2/0), `PhysicalSizeX` (1/2/0), `PhysicalSizeXUnit` (1/2/0), `PhysicalSizeY` (1/2/0), `PhysicalSizeYUnit` (1/2/0), `SizeC` (1/2/0), `SizeX` (1/2/0), `SizeY` (1/2/0), `TheC` (1/2/0), `Type` (1/2/0), `URISyntaxException` (1/2/0), `WorkflowGenomeannotator` (1/2/0), `WorkflowGenomeskim` (1/2/0), `WorkflowHic` (1/2/0), `WorkflowHicar` (1/2/0), `java.util.regex.Pattern` (1/2/0), `java.time.OffsetDateTime` (1/1/2), `CharSequence` (1/1/1), `UUID` (1/1/1), `FileType` (1/1/0), `IllegalStateException` (1/1/0), `LinkedHashSet` (1/1/0), `RuntimeException` (1/1/0), `True` (1/1/0), `WorkflowDrugresponseeval` (1/1/0), `WorkflowNanoseq` (1/1/0), `WorkflowOmicsgenetraitassociation` (1/1/0), `XmlSlurper` (1/1/0), `groovy.json.JsonSlurperClassic` (1/1/0), `groovy.transform.Field` (1/1/0), `groovy.xml.XmlSlurper` (1/1/0), `groovy.yaml.YamlSlurper` (1/1/0), `groovyx.gpars.dataflow.DataflowVariable` (1/1/0), `java` (1/1/0), `java.io.BufferedReader` (1/1/0), `java.io.InputStreamReader` (1/1/0), `java.io.Serializable` (1/1/0), `java.net.URL` (1/1/0), `java.nio.file.Path` (1/1/0), `java.util.Locale` (1/1/0), `meta.single_end` (1/1/0), `nextflow.Nextflow` (1/1/0), `nextflow.util.ArrayBag` (1/1/0), `org.yaml.snakeyaml.DumperOptions` (1/1/0), `java.time.format.DateTimeFormatter` (0/0/2).

Some long-tail names are not classes.
`SizeX`, `PhysicalSizeX` and similar come from XML attribute access (`node.@SizeX`) in a file with a parse error.
`PREPARE_GENOME` comes from an `include` inside an `if` block (also a parse error).
Names such as `WorkflowMain`, `NfcoreTemplate`, `NfcoreSchema`, `Utils`, `Constants` and `Workflow<Pipeline>` are most likely Groovy classes in the pipeline `lib/` directory, which the text corpus does not contain (unverified).

Static members used on classes (method calls end with `()`):

| Member | Pipelines | Own code | Occurrences | Modules repo uses | Modules repo files |
|---|---:|---:|---:|---:|---:|
| `System.exit()` | 125 | 11 | 135 | 2 | 2 |
| `nextflow.extension.FilesEx.copyTo()` | 106 | 0 | 295 | 2 | 1 |
| `groovy.json.JsonOutput.prettyPrint()` | 106 | 2 | 109 | 2 | 2 |
| `groovy.json.JsonOutput.toJson()` | 87 | 6 | 94 | 1 | 1 |
| `NfcoreTemplate.summary()` | 17 | 17 | 18 | 0 | 0 |
| `String.format()` | 16 | 5 | 17 | 17 | 9 |
| `NfcoreTemplate.email()` | 16 | 16 | 17 | 0 | 0 |
| `WorkflowMain.initialise()` | 16 | 16 | 16 | 0 | 0 |
| `NfcoreTemplate.IM_notification()` | 13 | 13 | 13 | 0 | 0 |
| `WorkflowMain.getGenomeAttribute()` | 12 | 12 | 44 | 0 | 0 |
| `Math.ceil()` | 11 | 3 | 12 | 7 | 7 |
| `Math.round()` | 8 | 5 | 51 | 4 | 4 |
| `FilesEx.copyTo()` | 8 | 0 | 24 | 0 | 0 |
| `NfcoreTemplate.logo()` | 8 | 8 | 16 | 0 | 0 |
| `WorkflowMain.citation()` | 8 | 8 | 16 | 0 | 0 |
| `JsonOutput.prettyPrint()` | 8 | 0 | 8 | 0 | 0 |
| `JsonOutput.toJson()` | 8 | 0 | 8 | 0 | 0 |
| `NfcoreSchema.paramsSummaryMap()` | 8 | 8 | 8 | 0 | 0 |
| `NfcoreTemplate.dashedLine()` | 8 | 8 | 8 | 0 | 0 |
| `Math.max()` | 6 | 2 | 8 | 9 | 7 |
| `Integer.MAX_VALUE` | 5 | 1 | 5 | 1 | 1 |
| `Math.floor()` | 4 | 1 | 4 | 2 | 2 |
| `MemoryUnit.of()` | 4 | 0 | 4 | 0 | 0 |
| `Math.abs()` | 3 | 0 | 3 | 1 | 1 |
| `NfcoreTemplate.dump_parameters()` | 3 | 3 | 3 | 0 | 0 |
| `System.err` | 2 | 2 | 9 | 0 | 0 |
| `Math.min()` | 2 | 1 | 5 | 1 | 1 |
| `Nextflow.error()` | 2 | 1 | 4 | 0 | 0 |
| `Arrays.asList()` | 2 | 2 | 2 | 0 | 0 |
| `groovy.io.FileType.FILES` | 2 | 2 | 2 | 0 | 0 |

Long tail, as name (pipelines/occurrences/modules repo uses): `Constants.INPUT` (1/145/0), `Utils.selectCurrentOrExisting()` (1/99/0), `Utils.getTumorDnaSampleName()` (1/56/0), `WorkflowOncoanalyser.restoreMeta()` (1/44/0), `Utils.hasExistingInput()` (1/36/0), `WorkflowOncoanalyser.groupByMeta()` (1/25/0), `Utils.getNormalDnaSampleName()` (1/15/0), `Utils.getTumorReduxDirAlignment()` (1/11/0), `Utils.hasNormalDna()` (1/11/0), `Constants.RunMode` (1/10/0), `Utils.getInput()` (1/10/0), `Utils.getNormalReduxDirAlignment()` (1/9/0), `Utils.hasTumorDna()` (1/9/0), `WorkflowCommons.multiqcTsvFromList()` (1/9/0), `Utils.getTumorRnaSampleName()` (1/8/0), `RelativePublishFolder.getPublishedFolder()` (1/6/0), `Utils.getTumorReduxTsvs()` (1/5/0), `Utils.hasTumorRna()` (1/5/0), `Utils.getTumorDnaSample()` (1/4/0), `WorkflowCommons.getColFromFile()` (1/4/0), `WorkflowMain.getPrepConfigFromSamplesheet()` (1/4/0), `Constants.InfoField` (1/3/0), `Float.parseFloat()` (1/3/0), `Utils.cleanArgs()` (1/3/0), `Utils.getDonorDnaSampleName()` (1/3/0), `Utils.getDonorReduxDirAlignment()` (1/3/0), `Utils.getEnumFromString()` (1/3/0), `Utils.getNormalDnaSample()` (1/3/0), `Utils.getNormalReduxTsvs()` (1/3/0), `Utils.getSequencingPlatformPons()` (1/3/0), `WorkflowRadseq.addRefIdToChannels()` (1/3/0), `java.util.UUID.randomUUID()` (1/2/2), `ANNDATA_MAKEH5AD_single.out` (1/2/0), `Constants.DEFAULT_ISOFOX_READ_LENGTH_TARGETED` (1/2/0), `Constants.FileType` (1/2/0), `Constants.SequencingPlatform` (1/2/0), `Math.pow()` (1/2/0), `String.join()` (1/2/0), `Utils.getDonorDnaSample()` (1/2/0), `Utils.getDonorReduxTsvs()` (1/2/0). Another 87 less used names are listed in `scripts.json` under `class_static_members`.

Uses by position, for the most common classes:

| Class and position | Pipelines | Own code | Occurrences | Modules repo uses | Modules repo files |
|---|---:|---:|---:|---:|---:|
| `System [static method call]` | 125 | 11 | 136 | 2 | 2 |
| `Path [instanceof]` | 124 | 70 | 287 | 8 | 7 |
| (any fully qualified name) [new] | 116 | 113 | 798 | 19 | 13 |
| `File [new]` | 115 | 11 | 851 | 6 | 1 |
| `Boolean [as (cast)]` | 115 | 5 | 246 | 4 | 3 |
| `List [instanceof]` | 114 | 34 | 477 | 93 | 65 |
| `Set [as (cast)]` | 114 | 5 | 243 | 4 | 3 |
| `groovy.text.GStringTemplateEngine [new]` | 114 | 0 | 168 | 1 | 1 |
| `IOException [catch]` | 114 | 1 | 115 | 1 | 1 |
| `java.util.Date [new]` | 114 | 1 | 115 | 1 | 1 |
| `NullPointerException [catch]` | 114 | 0 | 114 | 1 | 1 |
| `groovy.text.SimpleTemplateEngine [new]` | 113 | 113 | 113 | 0 | 0 |
| `String [as (cast)]` | 108 | 9 | 342 | 4 | 3 |
| `Exception [catch]` | 107 | 6 | 121 | 2 | 2 |
| (any fully qualified name) [static method call] | 106 | 9 | 505 | 9 | 7 |
| `Map [as (cast)]` | 106 | 2 | 339 | 5 | 2 |
| `nextflow.extension.FilesEx [static method call]` | 106 | 0 | 295 | 2 | 1 |
| `org.yaml.snakeyaml.Yaml [new]` | 106 | 4 | 218 | 2 | 2 |
| `groovy.json.JsonOutput [static method call]` | 106 | 7 | 203 | 3 | 2 |
| `org.codehaus.groovy.GroovyException [new]` | 106 | 0 | 106 | 1 | 1 |
| `MemoryUnit [as (cast)]` | 103 | 0 | 110 | 2 | 2 |
| `String [instanceof]` | 95 | 15 | 136 | 4 | 4 |
| `URL [new]` | 56 | 0 | 56 | 1 | 1 |
| `Math [static method call]` | 25 | 12 | 85 | 24 | 17 |
| `Path [typed parameter]` | 25 | 2 | 30 | 1 | 1 |

Long tail, as name (pipelines/occurrences/modules repo uses): `Path [bare reference]` (24/24/1), (any fully qualified name) [bare reference] (23/23/1), (any fully qualified name) [typed parameter] (23/23/1), `Duration [bare reference]` (23/23/1), `Duration [typed parameter]` (23/23/1), `MemoryUnit [bare reference]` (23/23/1), `MemoryUnit [typed parameter]` (23/23/1), `groovy.json.JsonGenerator.Options [new]` (23/23/1), `nextflow.script.types.VersionNumber [bare reference]` (23/23/1), `nextflow.script.types.VersionNumber [typed parameter]` (23/23/1), `int [as (cast)]` (22/50/27), `LinkedHashMap [typed parameter]` (20/30/0), `String [typed variable]` (19/56/0), `WorkflowMain [static method call]` (18/87/0), `NfcoreTemplate [static method call]` (17/75/0), `String [static method call]` (17/19/17), (any fully qualified name) [import] (15/52/0), `groovy.json.JsonSlurper [new]` (13/27/5), `Map [typed variable]` (11/46/0), (any fully qualified name) [as (cast)] (11/12/0), `nextflow.util.MemoryUnit [as (cast)]` (11/12/0), `ArrayList [bare reference]` (11/11/0), `int [other type position]` (10/10/0), `Integer [as (cast)]` (9/49/3), `String [typed parameter]` (9/29/3), `IllegalArgumentException [new]` (9/20/9), `Yaml [new]` (9/17/0), `org.yaml.snakeyaml.Yaml [import]` (9/17/0), `FilesEx [static method call]` (8/24/0), `JsonOutput [static method call]` (8/16/0), `Yaml [typed variable]` (8/16/0), `nextflow.extension.FilesEx [import]` (8/16/0), `java.util.zip.GZIPInputStream [new]` (8/12/1), `NfcoreSchema [static method call]` (8/8/0), `groovy.json.JsonOutput [import]` (8/8/0), `Map [instanceof]` (6/25/0), `List [as (cast)]` (6/14/0), `Collection [instanceof]` (6/13/4), `List [typed parameter]` (6/8/0), `Exception [new]` (5/21/0). Another 139 less used names are listed in `scripts.json` under `class_reference_kinds`.

## 6. Global functions

Groups:

| Group | Pipelines | Own code | Occurrences | Modules repo uses | Modules repo files |
|---|---:|---:|---:|---:|---:|
| `Nextflow global function` | 133 | 132 | 4789 | 825 | 428 |
| `namespace function (workflow.*)` | 133 | 133 | 364 | 0 | 0 |
| `pipeline-defined function` | 130 | 130 | 1694 | 45 | 19 |
| `namespace function (log.*)` | 129 | 124 | 3214 | 209 | 157 |
| `nf-core utility subworkflow function` | 115 | 114 | 2023 | 16 | 4 |
| `plugin function (plugin/nf-schema)` | 110 | 110 | 700 | 4 | 2 |
| `Nextflow template function (process script)` | 44 | 20 | 304 | 78 | 77 |
| `not defined in the .nf files` | 18 | 14 | 56 | 0 | 0 |
| `plugin function (plugin/nf-validation)` | 16 | 16 | 70 | 3 | 1 |
| `not defined in the .nf files (file has a parse error)` | 15 | 9 | 53 | 0 | 0 |
| `plugin function (plugin/nf-core-utils)` | 5 | 5 | 57 | 0 | 0 |
| `plugin function (plugin/nf-fgbio)` | 2 | 2 | 2 | 0 | 0 |
| `plugin function (plugin/nf-anndata)` | 1 | 1 | 7 | 0 | 0 |

All functions called without a receiver, plus `log.*` and `workflow.*`:

| Group and function | Pipelines | Own code | Occurrences | Modules repo uses | Modules repo files |
|---|---:|---:|---:|---:|---:|
| `namespace function (workflow.*): workflow.onComplete` | 133 | 133 | 136 | 0 | 0 |
| `Nextflow global function: error` | 129 | 113 | 2204 | 741 | 394 |
| `Nextflow global function: file` | 129 | 129 | 1717 | 32 | 13 |
| `namespace function (log.*): log.info` | 128 | 38 | 1501 | 152 | 131 |
| `namespace function (log.*): log.warn` | 118 | 53 | 1134 | 26 | 16 |
| `Nextflow global function: exit` | 115 | 43 | 263 | 18 | 12 |
| `nf-core utility subworkflow function: getWorkflowVersion` | 114 | 0 | 407 | 3 | 2 |
| `nf-core utility subworkflow function: logColours` | 114 | 4 | 257 | 2 | 1 |
| `nf-core utility subworkflow function: workflowVersionToYAML` | 114 | 2 | 116 | 1 | 1 |
| `nf-core utility subworkflow function: processVersionsFromYAML` | 114 | 1 | 115 | 1 | 1 |
| `namespace function (workflow.*): workflow.onError` | 114 | 114 | 115 | 0 | 0 |
| `nf-core utility subworkflow function: completionEmail` | 114 | 114 | 115 | 0 | 0 |
| `nf-core utility subworkflow function: checkCondaChannels` | 114 | 0 | 114 | 1 | 1 |
| `nf-core utility subworkflow function: checkConfigProvided` | 114 | 0 | 114 | 1 | 1 |
| `nf-core utility subworkflow function: checkProfileProvided` | 114 | 0 | 114 | 1 | 1 |
| `nf-core utility subworkflow function: dumpParametersToJSON` | 114 | 0 | 114 | 1 | 1 |
| `namespace function (workflow.*): workflow.toMap` | 113 | 113 | 113 | 0 | 0 |
| `nf-core utility subworkflow function: completionSummary` | 113 | 113 | 113 | 0 | 0 |
| `namespace function (log.*): log.error` | 112 | 112 | 154 | 25 | 13 |
| `namespace function (log.*): log.debug` | 111 | 2 | 425 | 6 | 4 |
| `plugin function (plugin/nf-schema): paramsSummaryMap` | 110 | 110 | 211 | 0 | 0 |
| `plugin function (plugin/nf-schema): paramsSummaryLog` | 109 | 3 | 125 | 1 | 1 |
| `plugin function (plugin/nf-schema): validateParameters` | 109 | 3 | 125 | 1 | 1 |
| `nf-core utility subworkflow function: softwareVersionsToYAML` | 107 | 107 | 119 | 0 | 0 |
| `nf-core utility subworkflow function: getSingleReport` | 103 | 0 | 103 | 1 | 1 |
| `plugin function (plugin/nf-schema): samplesheetToList` | 97 | 97 | 143 | 1 | 1 |
| `nf-core utility subworkflow function: paramsSummaryMultiqc` | 97 | 97 | 103 | 0 | 0 |
| `pipeline-defined function: methodsDescriptionText` | 94 | 94 | 100 | 0 | 0 |
| `plugin function (plugin/nf-schema): paramsHelp` | 93 | 3 | 93 | 1 | 1 |
| `pipeline-defined function: validateInputParameters` | 78 | 78 | 78 | 0 | 0 |
| `pipeline-defined function: genomeExistsError` | 59 | 59 | 59 | 0 | 0 |
| `nf-core utility subworkflow function: imNotification` | 53 | 53 | 54 | 0 | 0 |
| `pipeline-defined function: validateInputSamplesheet` | 51 | 51 | 54 | 0 | 0 |
| `pipeline-defined function: toolBibliographyText` | 47 | 47 | 47 | 0 | 0 |
| `pipeline-defined function: toolCitationText` | 47 | 47 | 47 | 0 | 0 |
| `Nextflow template function (process script): template` | 44 | 20 | 304 | 78 | 77 |
| `Nextflow global function: tuple` | 42 | 40 | 470 | 12 | 8 |
| `pipeline-defined function: getGenomeAttribute` | 42 | 42 | 241 | 0 | 0 |
| `Nextflow global function: groupKey` | 17 | 16 | 100 | 16 | 14 |
| `plugin function (plugin/nf-validation): paramsSummaryMap` | 16 | 16 | 22 | 0 | 0 |
| `plugin function (plugin/nf-validation): paramsSummaryLog` | 16 | 8 | 17 | 1 | 1 |
| `plugin function (plugin/nf-validation): paramsHelp` | 15 | 7 | 16 | 1 | 1 |
| `pipeline-defined function: getFileSuffix` | 14 | 1 | 15 | 1 | 1 |
| `plugin function (plugin/nf-validation): validateParameters` | 14 | 6 | 15 | 1 | 1 |
| `nf-core utility subworkflow function: dashedLine` | 11 | 8 | 32 | 0 | 0 |

Long tail, as name (pipelines/occurrences/modules repo uses): `nf-core utility subworkflow function: attachMultiqcReport` (11/11/0), `pipeline-defined function: create_fastq_channel` (10/11/0), `Nextflow global function: println` (9/17/1), `Nextflow global function: files` (9/13/3), `pipeline-defined function: getFastpReadsAfterFiltering` (9/9/2), `nf-core utility subworkflow function: nfCoreLogo` (8/8/0), `nf-core utility subworkflow function: workflowCitation` (8/8/0), `not defined in the .nf files (file has a parse error): i` (8/8/0), `pipeline-defined function: getFastpAdapterSequence` (6/6/1), `pipeline-defined function: getTrimGaloreReadsAfterFiltering` (6/6/1), `pipeline-defined function: multiqcTsvFromList` (5/12/1), `plugin function (plugin/nf-core-utils): softwareVersionsToYAML` (5/5/0), `pipeline-defined function: create_fastq_channels` (4/5/0), `not defined in the .nf files (file has a parse error): GroovyException` (4/4/0), `not defined in the .nf files: GroovyException` (4/4/0), `plugin function (plugin/nf-core-utils): getGenomeAttribute` (3/31/0), `nf-core utility subworkflow function: isCloudUrl` (3/6/2), `pipeline-defined function: calculateStrandedness` (3/4/1), `pipeline-defined function: getSalmonInferredStrandedness` (3/3/1), `pipeline-defined function: checkMaxContigSize` (3/3/0), `pipeline-defined function: defineToolsList` (3/3/0), `plugin function (plugin/nf-core-utils): checkCondaChannels` (3/3/0), `plugin function (plugin/nf-core-utils): checkConfigProvided` (3/3/0), `plugin function (plugin/nf-core-utils): checkProfileProvided` (3/3/0), `plugin function (plugin/nf-core-utils): completionEmail` (3/3/0), `plugin function (plugin/nf-core-utils): completionSummary` (3/3/0), `plugin function (plugin/nf-core-utils): dumpParametersToJSON` (3/3/0), `plugin function (plugin/nf-core-utils): getWorkflowVersion` (3/3/0), `pipeline-defined function: get_file_from_meta` (2/12/0), `not defined in the .nf files (file has a parse error): String` (2/8/0), `pipeline-defined function: resolvedExtension` (2/6/3), `pipeline-defined function: isNonZeroNonEmpty` (2/5/0), `not defined in the .nf files: path` (2/4/0), `pipeline-defined function: channelToSamplesheet` (2/4/0), `Nextflow global function: multiMapCriteria` (2/3/2), `pipeline-defined function: flowcellLaneFromFastq` (2/3/0), `pipeline-defined function: downloadZenodoApiEntry` (2/2/1), `pipeline-defined function: getReadLengthFromSeqkitStats` (2/2/1), `not defined in the .nf files (file has a parse error): val` (2/2/0), `pipeline-defined function: checkSamplesAfterGrouping` (2/2/0). Another 426 less used names are listed in `scripts.json` under `global_function_by_group`.

Named options of `file()` and `files()`:

| Option | Pipelines | Own code | Occurrences | Modules repo uses | Modules repo files |
|---|---:|---:|---:|---:|---:|
| `file(checkIfExists:)` | 120 | 120 | 867 | 5 | 3 |
| `file(type:)` | 13 | 10 | 25 | 2 | 1 |
| `files(checkIfExists:)` | 3 | 2 | 5 | 1 | 1 |
| `file(maxdepth:)` | 1 | 1 | 2 | 0 | 0 |
| `file(checkExists:)` | 1 | 1 | 1 | 0 | 0 |
| `file(dir:)` | 1 | 1 | 1 | 0 | 0 |

## 7. Implicit variables and namespaces

Global constants and namespaces (`docs/reference/stdlib-namespaces/global.mdx`, and `workflow.mdx`, `nextflow.mdx`, `log.mdx` in the same directory):

| Name | Pipelines | Own code | Occurrences | Modules repo uses | Modules repo files |
|---|---:|---:|---:|---:|---:|
| `task` | 134 | 123 | 32751 | 16551 | 2090 |
| `workflow` | 134 | 134 | 14800 | 2218 | 2075 |
| `params` | 134 | 134 | 13681 | 5 | 3 |
| `log` | 134 | 131 | 3346 | 210 | 158 |
| `projectDir` | 132 | 132 | 639 | 1 | 1 |
| `moduleDir` | 119 | 82 | 3279 | 2019 | 2010 |
| `nextflow` | 106 | 1 | 322 | 4 | 3 |
| `secrets` | 5 | 2 | 43 | 40 | 20 |
| `baseDir` | 3 | 3 | 7 | 0 | 0 |
| `workDir` | 1 | 1 | 2 | 0 | 0 |
| `launchDir` | 1 | 1 | 1 | 0 | 0 |

Properties read on `task`, `workflow` and `nextflow`, including short GString forms such as `$task.cpus`:

| Property | Pipelines | Own code | Occurrences | Modules repo uses | Modules repo files |
|---|---:|---:|---:|---:|---:|
| `task.ext` | 132 | 119 | 22987 | 12536 | 2090 |
| `task.process` | 130 | 114 | 6422 | 2396 | 2087 |
| `workflow.containerEngine` | 130 | 111 | 4202 | 2031 | 2011 |
| `workflow.manifest` | 125 | 125 | 3084 | 21 | 2 |
| `task.cpus` | 124 | 77 | 2379 | 1158 | 972 |
| `workflow.profile` | 121 | 121 | 550 | 120 | 76 |
| `workflow.nextflow` | 118 | 6 | 624 | 4 | 1 |
| `workflow.errorReport` | 116 | 2 | 170 | 1 | 1 |
| `workflow.projectDir` | 115 | 1 | 682 | 5 | 1 |
| `workflow.commitId` | 114 | 1 | 795 | 6 | 2 |
| `workflow.success` | 114 | 3 | 524 | 4 | 1 |
| `workflow.launchDir` | 114 | 0 | 456 | 4 | 2 |
| `workflow.runName` | 114 | 3 | 399 | 3 | 1 |
| `workflow.revision` | 114 | 1 | 337 | 2 | 1 |
| `workflow.complete` | 114 | 0 | 336 | 2 | 1 |
| `workflow.repository` | 114 | 0 | 336 | 2 | 1 |
| `workflow.scriptId` | 114 | 1 | 170 | 1 | 1 |
| `workflow.commandLine` | 114 | 1 | 169 | 1 | 1 |
| `workflow.start` | 114 | 1 | 169 | 1 | 1 |
| `workflow.duration` | 114 | 0 | 168 | 1 | 1 |
| `workflow.errorMessage` | 114 | 0 | 168 | 1 | 1 |
| `workflow.exitStatus` | 114 | 0 | 168 | 1 | 1 |
| `workflow.scriptFile` | 114 | 0 | 168 | 1 | 1 |
| `workflow.stats` | 114 | 2 | 116 | 1 | 1 |
| `workflow.configFiles` | 114 | 0 | 115 | 1 | 1 |
| `task.memory` | 89 | 24 | 884 | 394 | 215 |
| `task.attempt` | 16 | 4 | 27 | 4 | 4 |
| `workflow.stubRun` | 12 | 5 | 21 | 3 | 2 |
| `task.accelerator` | 5 | 1 | 20 | 59 | 23 |
| `task.exitStatus` | 4 | 3 | 14 | 0 | 0 |

Long tail, as name (pipelines/occurrences/modules repo uses): `workflow.workDir` (4/4/0), `nextflow.version` (3/4/1), `task.index` (2/4/0), `task.workDir` (2/2/1), `task.conda` (1/2/1), `log.name` (1/2/0), `task.executor` (1/2/0), `task.container` (1/1/0), `task.filter_col` (1/1/0), `task.filter_val` (1/1/0), `task.h5ad` (1/1/0), `task.meta` (1/1/0), `task.method` (1/1/0), `task.obs_key` (1/1/0), `task.rank_key` (1/1/0), `workflow.resume` (1/1/0), `workflow.sessionId` (1/1/0), `workflow.userName` (1/1/0), `task.stageInMode` (0/0/2).

Keys read from `task.ext`.
The `ext` directive holds user-defined properties (`docs/reference/process/directives/ext.mdx`); nf-core sets them in config files (see nf-core-config.md).

| Key | Pipelines | Own code | Occurrences | Modules repo uses | Modules repo files |
|---|---:|---:|---:|---:|---:|
| `when` | 130 | 109 | 7736 | 4178 | 2089 |
| `args` | 130 | 95 | 4210 | 2380 | 1921 |
| `singularity_pull_docker_container` | 130 | 110 | 4150 | 2009 | 2009 |
| `prefix` | 129 | 95 | 5915 | 3714 | 1887 |
| `args2` | 96 | 25 | 439 | 174 | 145 |
| `suffix` | 43 | 7 | 199 | 24 | 12 |
| `args3` | 32 | 6 | 57 | 42 | 36 |
| `args4` | 16 | 0 | 18 | 10 | 9 |
| `use_gpu` | 4 | 3 | 10 | 0 | 0 |
| `log_level` | 2 | 2 | 78 | 0 | 0 |
| `xmx_mod` | 2 | 2 | 30 | 0 | 0 |
| `seed` | 2 | 2 | 7 | 0 | 0 |

Long tail, as name (pipelines/occurrences/modules repo uses): `description` (1/4/0), `section_name` (1/4/0), `use_observed_lib_size` (1/4/0), `ext` (1/3/0), `args_ab` (1/2/0), `args_mq` (1/2/0), `args_pr` (1/2/0), `args_st` (1/2/0), `command` (1/2/0), `dispersion` (1/2/0), `extension` (1/2/0), `fgbio_args` (1/2/0), `fgbio_call_args` (1/2/0), `fgbio_filter_args` (1/2/0), `full_res` (1/2/0), `gene_likelihood` (1/2/0), `genome` (1/2/0), `has_header` (1/2/0), `hd5_plugin_path` (1/2/0), `implicit_params` (1/2/0), `max_epochs` (1/2/0), `meta_params` (1/2/0), `metrics` (1/2/0), `min_coverage` (1/2/0), `n_hidden` (1/2/0), `n_latent` (1/2/0), `n_layers` (1/2/0), `num_output_lines` (1/2/0), `output` (1/2/0), `output_format` (1/2/0), `output_mzidentml` (1/2/0), `output_pepxml` (1/2/0), `output_percolator` (1/2/0), `output_sqt` (1/2/0), `output_txt` (1/2/0), `parametrize` (1/2/0), `plot_basis` (1/2/0), `prior_column` (1/2/0), `prior_confidence` (1/2/0), `rnaplot` (1/2/0). Another 52 less used names are listed in `scripts.json` under `task_ext_keys`.

Where `params` is read.
The strict syntax recommends using params only in the entry workflow and the output block (`docs/strict-syntax.mdx`, lines 597 to 622):

| Region | Pipelines | Own code | Occurrences | Modules repo uses | Modules repo files |
|---|---:|---:|---:|---:|---:|
| `named workflow body` | 131 | 131 | 6841 | 0 | 0 |
| `entry workflow body` | 118 | 118 | 2027 | 0 | 0 |
| `function body` | 115 | 93 | 2805 | 3 | 2 |
| `named workflow body (inside a closure)` | 63 | 63 | 420 | 0 | 0 |
| `process script:` | 45 | 39 | 637 | 2 | 1 |
| `top level` | 33 | 33 | 548 | 0 | 0 |
| `top level (inside a closure)` | 17 | 17 | 98 | 0 | 0 |
| `process directives` | 16 | 16 | 225 | 0 | 0 |
| `function body (inside a closure)` | 11 | 11 | 26 | 0 | 0 |
| `process stub:` | 5 | 2 | 11 | 0 | 0 |
| `entry workflow body (inside a closure)` | 4 | 4 | 10 | 0 | 0 |
| `process output:` | 4 | 4 | 6 | 0 | 0 |
| `output block` | 3 | 3 | 12 | 0 | 0 |
| `process directives (inside a closure)` | 2 | 2 | 4 | 0 | 0 |
| `process when:` | 1 | 1 | 8 | 0 | 0 |
| `params block` | 1 | 1 | 2 | 0 | 0 |
| `output block (inside a closure)` | 1 | 1 | 1 | 0 | 0 |

Where `workflow.onComplete`, `workflow.onError` and other `workflow.*` calls appear.
The nf-core utility subworkflow calls the handlers inside the named workflow `PIPELINE_COMPLETION`:

| Call and region | Pipelines | Own code | Occurrences | Modules repo uses | Modules repo files |
|---|---:|---:|---:|---:|---:|
| `workflow.onComplete in named workflow body` | 117 | 117 | 118 | 0 | 0 |
| `workflow.toMap in function body` | 113 | 113 | 113 | 0 | 0 |
| `workflow.onError in named workflow body` | 112 | 112 | 113 | 0 | 0 |
| `workflow.onComplete in top level` | 17 | 17 | 18 | 0 | 0 |
| `workflow.onError in top level` | 2 | 2 | 2 | 0 | 0 |

`workflow.toMap()` is not in `docs/reference/stdlib-namespaces/workflow.mdx`.
It is a method of `modules/nextflow/src/main/groovy/nextflow/script/WorkflowMetadata.groovy` (line 450).

## 8. Processes

All counted processes are legacy (untyped) processes.
The strict parser lowers them with `modules/nf-lang/src/main/java/nextflow/script/control/ProcessToGroovyVisitorV1.java`.

### 8.1 Sections

| Section | Pipelines | Own code | Occurrences | Modules repo uses | Modules repo files |
|---|---:|---:|---:|---:|---:|
| `script:` | 134 | 122 | 4571 | 2090 | 2090 |
| `output:` | 134 | 123 | 4569 | 2091 | 2091 |
| `input:` | 134 | 122 | 4539 | 2072 | 2072 |
| `when:` | 130 | 109 | 3870 | 2089 | 2089 |
| `stub:` | 125 | 85 | 3411 | 2069 | 2069 |
| `exec:` | 4 | 4 | 4 | 1 | 1 |

### 8.2 Directives

| Directive | Pipelines | Own code | Occurrences | Modules repo uses | Modules repo files |
|---|---:|---:|---:|---:|---:|
| `label` | 132 | 118 | 4718 | 2152 | 2084 |
| `container` | 132 | 120 | 4508 | 2088 | 2088 |
| `tag` | 131 | 118 | 4219 | 2028 | 2027 |
| `conda` | 131 | 112 | 4201 | 2010 | 2010 |
| `memory` | 15 | 0 | 19 | 4 | 4 |
| `publishDir` | 8 | 8 | 36 | 0 | 0 |
| `stageInMode` | 8 | 4 | 9 | 18 | 18 |
| `containerOptions` | 7 | 2 | 10 | 11 | 11 |
| (other statement: if_statement) | 6 | 6 | 22 | 0 | 0 |
| `errorStrategy` | 4 | 3 | 13 | 1 | 1 |
| `container (inside if/else)` | 3 | 3 | 10 | 0 | 0 |
| `beforeScript` | 2 | 1 | 10 | 4 | 4 |
| `maxRetries` | 2 | 2 | 10 | 0 | 0 |
| `secret` | 2 | 2 | 3 | 1 | 1 |
| `debug` | 2 | 2 | 2 | 1 | 1 |
| `echo` | 1 | 1 | 2 | 0 | 0 |
| `maxForks` | 1 | 1 | 2 | 0 | 0 |
| `executor` | 1 | 1 | 1 | 1 | 1 |
| (assignment: containerOptions = ...) | 1 | 1 | 1 | 0 | 0 |
| (assignment: errorStrategy = ...) | 1 | 1 | 1 | 0 | 0 |
| (assignment: stageInMode = ...) | 1 | 1 | 1 | 0 | 0 |
| `cache` | 1 | 1 | 1 | 0 | 0 |
| `maxErrors` | 1 | 1 | 1 | 0 | 0 |
| `storeDir` | 1 | 1 | 1 | 0 | 0 |

"(inside if/else)" marks a directive written inside an `if` block among the directives.
"(other statement: ...)" and "(assignment: ...)" rows are statements in the directive part that are not directive calls.
`echo` is not in the directive list of `docs/reference/process/directives.mdx` (the list has `debug`).

How directive values are written.
In the strict syntax, a directive with one argument that references a variable is evaluated again for each task, as if it were a closure (`docs/process.mdx`, lines 1183 to 1219; `ProcessToGroovyVisitorV1.java`, lines 89 to 105; `modules/nf-lang/src/main/java/nextflow/script/control/ScriptToGroovyHelper.java`, lines 137 to 144).
`executor`, `label`, `maxForks`, `module`, `pod`, `publishDir` and `secret` are never made dynamic this way (`ProcessToGroovyVisitorV1.java`, lines 79 to 87).

| Value form | Pipelines | Own code | Occurrences | Modules repo uses | Modules repo files |
|---|---:|---:|---:|---:|---:|
| `one argument that references a variable` | 132 | 119 | 11729 | 6040 | 2088 |
| `one constant argument` | 132 | 118 | 5927 | 2271 | 2085 |
| `closure` | 28 | 12 | 73 | 8 | 8 |
| `named options or several arguments` | 9 | 9 | 37 | 0 | 0 |

| Directive and value form | Pipelines | Own code | Occurrences | Modules repo uses | Modules repo files |
|---|---:|---:|---:|---:|---:|
| `label [one constant argument]` | 132 | 118 | 4718 | 2152 | 2084 |
| `container [one argument that references a variable]` | 130 | 112 | 4189 | 2009 | 2009 |
| `tag [one argument that references a variable]` | 130 | 115 | 4117 | 2014 | 2013 |
| `conda [one argument that references a variable]` | 122 | 85 | 3410 | 2009 | 2009 |
| `container [one constant argument]` | 53 | 42 | 316 | 79 | 79 |
| `conda [one constant argument]` | 49 | 46 | 790 | 1 | 1 |
| `tag [one constant argument]` | 32 | 29 | 71 | 14 | 14 |
| `memory [closure]` | 15 | 0 | 19 | 4 | 4 |
| `publishDir [named options or several arguments]` | 8 | 8 | 36 | 0 | 0 |
| `stageInMode [one constant argument]` | 8 | 4 | 9 | 18 | 18 |
| `tag [closure]` | 6 | 6 | 31 | 0 | 0 |
| `errorStrategy [closure]` | 4 | 3 | 13 | 0 | 0 |
| `containerOptions [closure]` | 3 | 0 | 6 | 4 | 4 |
| `containerOptions [one argument that references a variable]` | 3 | 1 | 3 | 7 | 7 |
| `container [closure]` | 3 | 3 | 3 | 0 | 0 |

Long tail, as name (pipelines/occurrences/modules repo uses): `maxRetries [one constant argument]` (2/10/0), `debug [one constant argument]` (2/2/1), `beforeScript [one argument that references a variable]` (1/8/0), `beforeScript [one constant argument]` (1/2/4), `echo [one constant argument]` (1/2/0), `maxForks [one constant argument]` (1/2/0), `secret [one constant argument]` (1/2/0), `executor [one constant argument]` (1/1/1), `secret [one argument that references a variable]` (1/1/1), `cache [one constant argument]` (1/1/0), `conda [closure]` (1/1/0), `containerOptions [named options or several arguments]` (1/1/0), `maxErrors [one constant argument]` (1/1/0), `storeDir [one argument that references a variable]` (1/1/0), `errorStrategy [one constant argument]` (0/0/1).

| Option | Pipelines | Own code | Occurrences | Modules repo uses | Modules repo files |
|---|---:|---:|---:|---:|---:|
| `publishDir(mode:)` | 7 | 7 | 35 | 0 | 0 |
| `publishDir(pattern:)` | 1 | 1 | 6 | 0 | 0 |
| `publishDir(path:)` | 1 | 1 | 3 | 0 | 0 |
| `containerOptions(shm-size:)` | 1 | 1 | 1 | 0 | 0 |
| `publishDir(enabled:)` | 1 | 1 | 1 | 0 | 0 |

### 8.3 Inputs

| Input | Pipelines | Own code | Occurrences | Modules repo uses | Modules repo files |
|---|---:|---:|---:|---:|---:|
| `tuple element: path` | 132 | 115 | 8176 | 4398 | 1983 |
| `tuple element: val` | 132 | 118 | 6136 | 3347 | 2008 |
| `tuple` | 132 | 118 | 5665 | 3073 | 2008 |
| `path` | 127 | 97 | 2777 | 819 | 440 |
| `val` | 116 | 85 | 1853 | 667 | 423 |
| `path with a string (file name or pattern)` | 20 | 17 | 266 | 9 | 8 |
| `file` | 5 | 4 | 13 | 1 | 1 |
| `tuple element: file` | 5 | 5 | 9 | 0 | 0 |
| `each` | 4 | 4 | 10 | 0 | 0 |
| `env` | 2 | 2 | 14 | 0 | 0 |
| `each element: a name (each x, a value)` | 2 | 2 | 5 | 0 | 0 |
| `each element: path` | 2 | 2 | 5 | 0 | 0 |
| `env with a quoted name` | 1 | 1 | 11 | 0 | 0 |
| `env with an unquoted name` | 1 | 1 | 3 | 0 | 0 |
| (other statement: ERROR) | 1 | 1 | 1 | 0 | 0 |
| `typed input (name: Type)` | 1 | 1 | 1 | 0 | 0 |

| Option | Pipelines | Own code | Occurrences | Modules repo uses | Modules repo files |
|---|---:|---:|---:|---:|---:|
| `path(stageAs:)` | 118 | 45 | 682 | 189 | 117 |
| `path(arity:)` | 32 | 3 | 56 | 25 | 22 |
| `path(name:)` | 15 | 2 | 20 | 14 | 12 |

