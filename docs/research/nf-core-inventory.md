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

### 8.4 Outputs

| Output | Pipelines | Own code | Occurrences | Modules repo uses | Modules repo files |
|---|---:|---:|---:|---:|---:|
| `tuple element: val` | 132 | 118 | 14295 | 10177 | 2087 |
| `tuple` | 132 | 118 | 11803 | 7675 | 2087 |
| `tuple element: path` | 132 | 116 | 9873 | 5429 | 2012 |
| `path` | 128 | 111 | 4152 | 266 | 189 |
| `tuple element: eval` | 76 | 39 | 2038 | 2064 | 1832 |
| `stdout` | 6 | 6 | 8 | 1 | 1 |
| `tuple element: env` | 5 | 4 | 17 | 6 | 5 |
| `env` | 5 | 2 | 11 | 7 | 4 |
| `env with an unquoted name` | 5 | 3 | 9 | 0 | 0 |
| `env with a quoted name` | 4 | 2 | 19 | 13 | 9 |
| `tuple element: stdout` | 3 | 3 | 3 | 2 | 2 |
| `val` | 2 | 2 | 5 | 0 | 0 |
| `tuple element: file` | 2 | 1 | 2 | 3 | 3 |
| (other statement: ERROR) | 1 | 1 | 1 | 0 | 0 |
| (other statement: variable_declaration) | 1 | 1 | 1 | 0 | 0 |
| `publishDir` | 1 | 1 | 1 | 0 | 0 |

Options on a whole output declaration (`docs/reference/process/inputs-outputs.mdx`, "Generic options", line 146):

| Option | Pipelines | Own code | Occurrences | Modules repo uses | Modules repo files |
|---|---:|---:|---:|---:|---:|
| `emit:` | 131 | 118 | 15524 | 7949 | 2091 |
| `optional:` | 127 | 69 | 4117 | 2043 | 591 |
| `topic:` | 75 | 48 | 2615 | 2361 | 2083 |
| `includeInputs:` | 3 | 3 | 15 | 0 | 0 |
| `enabled:` | 1 | 1 | 1 | 0 | 0 |
| `type:` | 1 | 1 | 1 | 0 | 0 |

Options on a single element inside `tuple ...`:

| Option | Pipelines | Own code | Occurrences | Modules repo uses | Modules repo files |
|---|---:|---:|---:|---:|---:|
| `path(includeInputs:)` | 6 | 3 | 23 | 15 | 5 |
| `path(type:)` | 6 | 2 | 11 | 10 | 6 |
| `path(arity:)` | 3 | 1 | 7 | 9 | 2 |
| `path(hidden:)` | 0 | 0 | 0 | 7 | 4 |

### 8.5 Script, when and stub

| Feature | Pipelines | Own code | Occurrences | Modules repo uses | Modules repo files |
|---|---:|---:|---:|---:|---:|
| `script: section ends with a triple double-quoted string` | 134 | 119 | 4053 | 1960 | 1960 |
| `if/else at the top level of the script section` | 114 | 59 | 1165 | 586 | 586 |
| `script: section ends with a statement (if_statement)` | 64 | 24 | 210 | 38 | 38 |
| `template file (template ...)` | 44 | 20 | 304 | 78 | 77 |
| `script: section ends with a template call (template 'file')` | 43 | 20 | 302 | 76 | 76 |
| `exec: section ends with a statement (assignment)` | 3 | 3 | 3 | 1 | 1 |
| `script: section ends with a statement (assignment)` | 2 | 0 | 2 | 1 | 1 |
| `script: section ends with a statement (assert_statement)` | 1 | 0 | 1 | 14 | 14 |
| `script: section ends with an expression (binary_expression)` | 1 | 0 | 1 | 1 | 1 |
| `exec: section ends with a statement (if_statement)` | 1 | 1 | 1 | 0 | 0 |
| `script: section ends with an expression (call_expression)` | 1 | 1 | 1 | 0 | 0 |
| `script: section ends with an expression (command_expression)` | 1 | 1 | 1 | 0 | 0 |

`when:` conditions:

| Condition | Pipelines | Own code | Occurrences | Modules repo uses | Modules repo files |
|---|---:|---:|---:|---:|---:|
| `task.ext.when == null \|\| task.ext.when` | 130 | 109 | 3865 | 2089 | 2089 |
| `!params.skip_alignment && !params.skip_bigbed && (params.protocol == 'directRNA' \|\| params.protocol == 'cDNA')` | 1 | 1 | 2 | 0 | 0 |
| `task.ext.when == null \|\| task.ext.when alignment_bam.size() > 0` | 1 | 1 | 1 | 0 | 0 |

Long tail, as name (pipelines/occurrences/modules repo uses): `task.ext.when == null \|\| task.ext.when fasta.size() > 0` (1/1/0), `task.ext.when == null \|\| task.ext.when gtf.size() > 0` (1/1/0).

## 9. Workflows

| Section | Pipelines | Own code | Occurrences | Modules repo uses | Modules repo files |
|---|---:|---:|---:|---:|---:|
| `main: (named workflow)` | 133 | 133 | 1947 | 124 | 124 |
| `take: (named workflow)` | 133 | 133 | 1926 | 124 | 124 |
| `emit: (named workflow)` | 133 | 133 | 1765 | 124 | 124 |
| `main: (entry workflow)` | 103 | 103 | 105 | 0 | 0 |
| `implicit main (no section labels)` | 34 | 34 | 74 | 0 | 0 |
| `publish: (entry workflow)` | 12 | 12 | 12 | 0 | 0 |
| `publish: (named workflow)` | 1 | 1 | 1 | 0 | 0 |

| Entry form | Pipelines | Own code | Occurrences | Modules repo uses | Modules repo files |
|---|---:|---:|---:|---:|---:|
| `take: name` | 133 | 133 | 10851 | 640 | 124 |
| `emit: name = value` | 133 | 132 | 5428 | 697 | 122 |
| `emit: name` | 90 | 45 | 894 | 41 | 3 |
| `publish: name = value` | 11 | 11 | 72 | 0 | 0 |
| `emit: other expression` | 2 | 2 | 2 | 0 | 0 |

| Form | Pipelines | Own code | Occurrences | Modules repo uses | Modules repo files |
|---|---:|---:|---:|---:|---:|
| `process call` | 134 | 134 | 5933 | 474 | 117 |
| `workflow call` | 134 | 134 | 2320 | 38 | 22 |
| `PROCESS.out or WORKFLOW.out access` | 133 | 133 | 16485 | 1215 | 118 |

Output block (`docs/workflow.mdx`, section "Outputs", line 50; output directives at lines 298 to 330):

| Setting | Pipelines | Own code | Occurrences | Modules repo uses | Modules repo files |
|---|---:|---:|---:|---:|---:|
| `declaration` | 11 | 11 | 72 | 0 | 0 |
| `setting: path` | 11 | 11 | 71 | 0 | 0 |
| `setting: index` | 4 | 4 | 18 | 0 | 0 |
| `setting: index { path }` | 4 | 4 | 18 | 0 | 0 |
| `setting: mode` | 2 | 2 | 8 | 0 | 0 |
| `setting: enabled` | 2 | 2 | 4 | 0 | 0 |
| `setting: index { sep }` | 2 | 2 | 3 | 0 | 0 |
| `setting: index { header }` | 1 | 1 | 2 | 0 | 0 |

`params { }` block entries:

| Entry | Pipelines | Own code | Occurrences | Modules repo uses | Modules repo files |
|---|---:|---:|---:|---:|---:|
| `typed parameter` | 3 | 3 | 179 | 0 | 0 |
| `untyped parameter` | 1 | 1 | 1 | 0 | 0 |

Topic channels: `channel.topic(...)` is used in 76 pipelines and the output option `topic:` in 75 pipelines (sections 2 and 8.4).
Records, enums, agents and typed processes, workflows or outputs are not used.
The tool found no `record`, `enum` or `agent` definition and no typed `take:` or `emit:` entry.
The one "typed input" in section 8.3 is a misparse in a file with a parse error.

## 10. Statements and expressions

| Statement | Pipelines | Own code | Occurrences | Modules repo uses | Modules repo files |
|---|---:|---:|---:|---:|---:|
| `def x = ...` | 134 | 134 | 28513 | 8912 | 2050 |
| `if` | 134 | 134 | 12709 | 1620 | 667 |
| `return` | 133 | 132 | 3829 | 95 | 30 |
| `else` | 133 | 131 | 2828 | 329 | 253 |
| `else if` | 127 | 85 | 1084 | 181 | 77 |
| `catch` | 115 | 11 | 366 | 4 | 3 |
| `try` | 115 | 11 | 260 | 3 | 3 |
| `multiple assignment: def (a, b) = ...` | 94 | 93 | 143 | 4 | 4 |
| `assert` | 30 | 10 | 86 | 94 | 53 |
| `throw` | 26 | 14 | 55 | 11 | 8 |
| `for (x in y) (not in strict syntax)` | 22 | 15 | 50 | 0 | 0 |
| `Groovy-style typed declaration (def Type x)` | 20 | 19 | 30 | 0 | 0 |
| `Groovy-style typed declaration without def (Type x = ...)` | 19 | 12 | 112 | 0 | 0 |
| `import (not in strict syntax)` | 15 | 5 | 52 | 0 | 0 |

| Function feature | Pipelines | Own code | Occurrences | Modules repo uses | Modules repo files |
|---|---:|---:|---:|---:|---:|
| `untyped parameter` | 122 | 118 | 2817 | 85 | 20 |
| `parameter default value` | 114 | 15 | 537 | 8 | 2 |
| `function that builds a channel` | 114 | 20 | 161 | 4 | 2 |
| `Groovy-style typed parameter (Type x)` | 32 | 31 | 92 | 3 | 3 |
| `Groovy-style return type (Type name(...))` | 3 | 3 | 4 | 3 | 3 |

| Operator | Pipelines | Own code | Occurrences | Modules repo uses | Modules repo files |
|---|---:|---:|---:|---:|---:|
| `=` | 134 | 134 | 35090 | 3105 | 852 |
| `+=` | 122 | 109 | 2013 | 84 | 35 |
| `\|=` | 8 | 0 | 8 | 0 | 0 |
| `-=` | 1 | 1 | 1 | 0 | 0 |

| Target | Pipelines | Own code | Occurrences | Modules repo uses | Modules repo files |
|---|---:|---:|---:|---:|---:|
| `variable (x = ...)` | 134 | 134 | 26578 | 3100 | 865 |
| `property (a.b = ...)` | 131 | 128 | 743 | 17 | 5 |
| `index (a[i] = ...)` | 117 | 115 | 9773 | 70 | 2 |
| `multiple assignment ((a, b) = ...)` | 3 | 2 | 16 | 2 | 2 |
| `other` | 2 | 2 | 2 | 0 | 0 |

Operators.
`| (on channels)` and `& (on channels)` are the channel pipe and fork operators.
`|` and `&` without that note work on numbers or booleans.

| Operator | Pipelines | Own code | Occurrences | Modules repo uses | Modules repo files |
|---|---:|---:|---:|---:|---:|
| `==` | 134 | 132 | 10965 | 3052 | 2121 |
| `\|\|` | 134 | 134 | 6045 | 2576 | 2103 |
| `a ? b : c (ternary)` | 133 | 133 | 21151 | 5433 | 2073 |
| `?: (elvis)` | 133 | 118 | 11623 | 6368 | 2049 |
| `unary !` | 133 | 132 | 8367 | 2540 | 2053 |
| `&&` | 133 | 132 | 7778 | 2372 | 2041 |
| `+` | 133 | 130 | 4403 | 364 | 114 |
| `a[i] (index)` | 132 | 130 | 17253 | 825 | 214 |
| `-` | 131 | 118 | 1086 | 135 | 90 |
| `instanceof` | 127 | 85 | 962 | 113 | 79 |
| `!=` | 125 | 85 | 1062 | 89 | 52 |
| `>=` | 124 | 120 | 515 | 137 | 87 |
| `<<` | 123 | 32 | 1601 | 82 | 16 |
| `unary -` | 123 | 86 | 416 | 24 | 17 |
| `as (cast)` | 122 | 39 | 1466 | 80 | 38 |
| `in` | 117 | 59 | 1922 | 2027 | 1959 |
| `<=` | 117 | 21 | 224 | 10 | 6 |
| `\|` | 116 | 8 | 158 | 10 | 5 |
| `..` | 112 | 109 | 245 | 12 | 11 |
| `>` | 108 | 83 | 702 | 103 | 69 |
| `<` | 78 | 24 | 171 | 26 | 22 |
| `=~` | 76 | 28 | 299 | 98 | 46 |
| `/` | 65 | 15 | 117 | 30 | 18 |
| `*` | 56 | 21 | 376 | 154 | 136 |
| `==~` | 50 | 10 | 136 | 65 | 35 |
| `&` | 45 | 8 | 80 | 8 | 7 |
| `~ (pattern from string)` | 31 | 12 | 70 | 32 | 17 |
| `<=>` | 30 | 28 | 87 | 11 | 8 |
| `!in` | 19 | 5 | 25 | 9 | 6 |
| `%` | 12 | 5 | 18 | 7 | 7 |
| `unary +` | 9 | 1 | 17 | 0 | 0 |
| `\| (on channels)` | 8 | 8 | 186 | 27 | 2 |
| `..<` | 7 | 5 | 10 | 2 | 2 |
| `>> (publish target in an output block path closure)` | 6 | 6 | 39 | 0 | 0 |
| `>>` | 1 | 1 | 4 | 0 | 0 |
| `& (on channels)` | 1 | 1 | 3 | 0 | 0 |
| `^` | 1 | 0 | 1 | 1 | 1 |
| `unary ~` | 1 | 1 | 1 | 0 | 0 |

| Closure form | Pipelines | Own code | Occurrences | Modules repo uses | Modules repo files |
|---|---:|---:|---:|---:|---:|
| `closure without parameters` | 134 | 134 | 3673 | 102 | 46 |
| `closure with parameters` | 132 | 131 | 11789 | 839 | 245 |
| `closure with 2 or more parameters` | 132 | 127 | 6786 | 422 | 100 |
| `implicit parameter 'it' used` | 93 | 87 | 2191 | 18 | 9 |
| `closure with a typed parameter` | 25 | 3 | 106 | 4 | 1 |

Labeled statements, by the operator that owns the closure (`branch` and `multiMap` criteria):

| Owner | Pipelines | Own code | Occurrences | Modules repo uses | Modules repo files |
|---|---:|---:|---:|---:|---:|
| `branch` | 100 | 99 | 1232 | 77 | 27 |
| `multiMap` | 45 | 45 | 666 | 59 | 12 |
| `multiMapCriteria` | 2 | 1 | 16 | 13 | 2 |

| Expression | Pipelines | Own code | Occurrences | Modules repo uses | Modules repo files |
|---|---:|---:|---:|---:|---:|
| `list literal [a, b]` | 134 | 134 | 16382 | 3008 | 2108 |
| `map literal [k: v]` | 133 | 132 | 5180 | 154 | 67 |
| `new Class(...)` | 117 | 115 | 1791 | 37 | 20 |
| `number with a memory or duration unit (2.GB, 1.h)` | 16 | 1 | 22 | 6 | 5 |

| Call form | Pipelines | Own code | Occurrences | Modules repo uses | Modules repo files |
|---|---:|---:|---:|---:|---:|
| `trailing closure argument` | 134 | 134 | 15111 | 909 | 248 |
| `command call without parentheses (x.method arg)` | 126 | 108 | 1012 | 46 | 29 |
| `spread call (list*.method())` | 80 | 9 | 92 | 1 | 1 |
| `safe call (x?.method())` | 45 | 18 | 214 | 95 | 14 |
| `call on parenthesized_expression` | 2 | 2 | 6 | 0 | 0 |
| `call on string` | 1 | 1 | 2 | 0 | 0 |
| `call on integer_literal` | 1 | 1 | 1 | 0 | 0 |
| `call on closure` | 0 | 0 | 0 | 1 | 1 |

| Access form | Pipelines | Own code | Occurrences | Modules repo uses | Modules repo files |
|---|---:|---:|---:|---:|---:|
| `safe property (x?.prop)` | 16 | 14 | 60 | 6 | 2 |
| `spread property (list*.prop)` | 2 | 2 | 2 | 0 | 0 |

Types used in `as` casts:

| Type | Pipelines | Own code | Occurrences | Modules repo uses | Modules repo files |
|---|---:|---:|---:|---:|---:|
| `Boolean` | 115 | 5 | 246 | 4 | 3 |
| `Set` | 114 | 5 | 243 | 4 | 3 |
| `String` | 108 | 9 | 342 | 4 | 3 |
| `Map` | 106 | 2 | 339 | 5 | 2 |
| `MemoryUnit` | 103 | 0 | 110 | 2 | 2 |
| `int` | 22 | 8 | 50 | 27 | 14 |
| `nextflow.util.MemoryUnit` | 11 | 0 | 12 | 0 | 0 |
| `Integer` | 9 | 8 | 49 | 3 | 3 |
| `List` | 6 | 6 | 14 | 0 | 0 |
| `double` | 4 | 1 | 21 | 7 | 1 |
| `long` | 2 | 2 | 18 | 16 | 8 |
| `boolean` | 2 | 2 | 4 | 3 | 2 |
| `Double` | 2 | 2 | 4 | 1 | 1 |
| `float` | 2 | 0 | 2 | 1 | 1 |
| `char` | 1 | 1 | 4 | 0 | 0 |
| `Float` | 1 | 1 | 2 | 3 | 1 |
| `BigDecimal` | 1 | 1 | 2 | 0 | 0 |
| `PREPARE_GENOME` | 1 | 1 | 2 | 0 | 0 |
| `ArrayList` | 1 | 1 | 1 | 0 | 0 |
| `LinkedHashSet` | 1 | 1 | 1 | 0 | 0 |

Types in `catch` clauses:

| Type | Pipelines | Own code | Occurrences | Modules repo uses | Modules repo files |
|---|---:|---:|---:|---:|---:|
| `IOException` | 114 | 1 | 115 | 1 | 1 |
| `NullPointerException` | 114 | 0 | 114 | 1 | 1 |
| `Exception` | 107 | 6 | 121 | 2 | 2 |
| (untyped) | 10 | 1 | 18 | 0 | 0 |
| `java.lang.reflect.InvocationTargetException` | 2 | 2 | 2 | 0 | 0 |
| `MissingPropertyException` | 1 | 1 | 2 | 0 | 0 |
| `URISyntaxException` | 1 | 1 | 2 | 0 | 0 |

Memory and duration unit suffixes on numbers (`modules/nf-lang/src/main/java/nextflow/script/types/shim/Integer.java`):

| Suffix | Pipelines | Own code | Occurrences | Modules repo uses | Modules repo files |
|---|---:|---:|---:|---:|---:|
| `B` | 10 | 0 | 11 | 1 | 1 |
| `MB` | 8 | 0 | 8 | 3 | 3 |
| `GB` | 2 | 1 | 3 | 2 | 1 |

## 11. Strings and GStrings

| String form | Pipelines | Own code | Occurrences | Modules repo uses | Modules repo files |
|---|---:|---:|---:|---:|---:|
| `single-quoted string` | 134 | 134 | 72099 | 19857 | 2213 |
| `triple double-quoted GString (with interpolation)` | 134 | 128 | 8229 | 4051 | 2068 |
| `double-quoted string (no interpolation)` | 133 | 133 | 44677 | 11091 | 2076 |
| `double-quoted GString (with interpolation)` | 133 | 133 | 36621 | 17260 | 2121 |
| `slashy string /.../` | 130 | 85 | 800 | 195 | 99 |
| `triple double-quoted string (no interpolation)` | 76 | 33 | 261 | 166 | 122 |
| `triple single-quoted string` | 2 | 2 | 3 | 0 | 0 |

Interpolation forms in all strings (`$x` and `$x.y` are the short forms, `${...}` the long form):

| Form | Pipelines | Own code | Occurrences | Modules repo uses | Modules repo files |
|---|---:|---:|---:|---:|---:|
| `${name}` | 133 | 132 | 42539 | 22650 | 2112 |
| `${a.b.c}` | 133 | 133 | 23339 | 7864 | 2099 |
| `${ternary or elvis}` | 133 | 126 | 4632 | 2102 | 2015 |
| `${index_expression}` | 130 | 49 | 846 | 281 | 113 |
| `${method call}` | 129 | 125 | 2024 | 275 | 176 |
| `$name` | 126 | 99 | 10780 | 3189 | 843 |
| `$name.path` | 118 | 90 | 3585 | 1584 | 1253 |
| `${binary_expression}` | 87 | 19 | 298 | 42 | 31 |
| `${member_expression}` | 82 | 82 | 136 | 3 | 2 |
| `${cast_expression}` | 4 | 2 | 18 | 2 | 2 |
| `${string}` | 1 | 1 | 2 | 0 | 0 |
| `${unary_expression}` | 1 | 1 | 1 | 0 | 0 |

The same, only inside process `script:` sections:

| Form | Pipelines | Own code | Occurrences | Modules repo uses | Modules repo files |
|---|---:|---:|---:|---:|---:|
| `${name}` | 133 | 114 | 21470 | 11474 | 1899 |
| `${a.b.c}` | 133 | 114 | 8601 | 2761 | 1853 |
| `$name` | 124 | 87 | 9060 | 2655 | 812 |
| `${method call}` | 108 | 41 | 745 | 219 | 162 |
| `$name.path` | 102 | 52 | 1110 | 346 | 303 |
| `${binary_expression}` | 86 | 14 | 277 | 36 | 28 |
| `${index_expression}` | 73 | 29 | 556 | 263 | 105 |
| `${ternary or elvis}` | 26 | 12 | 59 | 28 | 20 |
| `${member_expression}` | 3 | 2 | 5 | 3 | 2 |
| `${cast_expression}` | 2 | 0 | 2 | 2 | 2 |
| `${string}` | 1 | 1 | 2 | 0 | 0 |

### Top 30 interpolation shapes in process scripts

This counts every interpolation inside a string in a `script:` section.
That includes the command template and `def` lines such as `def prefix = task.ext.prefix ?: "${meta.id}"`.
The shape is the interpolation text with white space collapsed.

| # | Shape | Pipelines | Own code | Occurrences | Modules repo uses | Modules repo files |
|---:|---|---:|---:|---:|---:|---:|
| 1 | `${prefix}` | 127 | 89 | 7661 | 3597 | 1692 |
| 2 | `${meta.id}` | 126 | 99 | 3530 | 1810 | 1758 |
| 3 | `$args` | 119 | 58 | 1858 | 722 | 698 |
| 4 | `${args}` | 111 | 54 | 1463 | 1231 | 1197 |
| 5 | `${task.cpus}` | 110 | 56 | 1102 | 753 | 655 |
| 6 | `${task.process}` | 107 | 82 | 2570 | 19 | 19 |
| 7 | `$task.cpus` | 98 | 46 | 972 | 336 | 294 |
| 8 | `${task.ext.prefix}` | 96 | 3 | 110 | 6 | 6 |
| 9 | `${sample_names}` | 96 | 2 | 101 | 4 | 4 |
| 10 | `${multiqc_logo}` | 96 | 2 | 99 | 2 | 2 |
| 11 | `${replace_names}` | 96 | 1 | 99 | 2 | 2 |
| 12 | `${fasta}` | 95 | 45 | 807 | 479 | 422 |
| 13 | `$prefix` | 86 | 32 | 402 | 141 | 120 |
| 14 | `$fasta` | 84 | 34 | 475 | 167 | 133 |
| 15 | `${extension}` | 75 | 11 | 418 | 126 | 79 |
| 16 | `${reads.extension}` | 74 | 0 | 85 | 3 | 3 |
| 17 | `${index + 1}` | 74 | 1 | 74 | 1 | 1 |
| 18 | `${args2}` | 73 | 9 | 223 | 110 | 101 |
| 19 | `${entry.extension}` | 73 | 0 | 73 | 1 | 1 |
| 20 | `${samples}` | 71 | 0 | 100 | 15 | 14 |
| 21 | `${reference}` | 67 | 12 | 259 | 112 | 100 |
| 22 | `${archive}` | 67 | 3 | 175 | 10 | 6 |
| 23 | `${config}` | 67 | 4 | 94 | 21 | 21 |
| 24 | `$args2` | 64 | 15 | 186 | 39 | 37 |
| 25 | `${logo}` | 63 | 1 | 66 | 2 | 2 |
| 26 | `${multiqc_config}` | 63 | 1 | 66 | 2 | 2 |
| 27 | `${replace}` | 63 | 0 | 65 | 2 | 2 |
| 28 | `${input}` | 62 | 13 | 281 | 160 | 125 |
| 29 | `$reads` | 61 | 17 | 259 | 59 | 53 |
| 30 | `${reads[1]}` | 58 | 17 | 159 | 66 | 65 |

Distinct shapes in total: 6614.

## 12. Strict syntax status

Constructs that the strict parser rejects.
Sources: statements next to declarations (`modules/nf-lang/src/main/java/nextflow/script/parser/ScriptAstBuilder.java`, line 263; `docs/strict-syntax.mdx`, lines 62 to 107), `import` (`ScriptAstBuilder.java`, lines 308 to 310), `for` and `finally` (the keywords are commented out in `modules/nf-lang/src/main/antlr/ScriptLexer.g4`, lines 295 and 300), unquoted `env` names (`docs/strict-syntax.mdx`, lines 400 to 424), slashy strings with interpolation or several lines (`docs/strict-syntax.mdx`, lines 328 to 382).
The row "file that tree-sitter-nextflow cannot parse" counts files.
Section 13 shows that almost all of these files use a form that the strict syntax removed.

| Construct | Pipelines | Own code | Occurrences | Modules repo uses | Modules repo files |
|---|---:|---:|---:|---:|---:|
| (any construct in this table) | 45 | 38 | 585 | 0 | 0 |
| `top-level statement next to declarations` | 31 | 29 | 419 | 0 | 0 |
| `file that tree-sitter-nextflow cannot parse` | 26 | 18 | 44 | 0 | 0 |
| `for loop` | 22 | 15 | 50 | 0 | 0 |
| `import declaration` | 15 | 5 | 52 | 0 | 0 |
| `slashy string with $ interpolation` | 8 | 0 | 8 | 0 | 0 |
| `env input or output with an unquoted name` | 6 | 4 | 12 | 0 | 0 |

Constructs that the strict parser reports as deprecated or discouraged (`docs/strict-syntax.mdx`, lines 502 to 534 and 624 to 626; `VariableScopeVisitor.java`, lines 744 and 867):

| Construct | Pipelines | Own code | Occurrences | Modules repo uses | Modules repo files |
|---|---:|---:|---:|---:|---:|
| (any construct in this table) | 133 | 128 | 8440 | 2108 | 2093 |
| `process when: section (discouraged)` | 130 | 109 | 3870 | 2089 | 2089 |
| `implicit closure parameter it` | 93 | 87 | 2191 | 18 | 9 |
| `Channel.* instead of channel.*` | 84 | 79 | 2379 | 1 | 1 |

`shell:` sections do not occur in the 134 pipelines (they occur only in two DSL1 pipelines).
No process leaves out the `script:` label, which the strict syntax requires when other sections are present (`docs/strict-syntax.mdx`, lines 426 to 459).
No `include` uses `addParams` (`docs/strict-syntax.mdx`, lines 248 to 287).
Groovy-style type annotations (`def Map meta`, `String x = ...`, `def f(Map meta)`) are still accepted (`ScriptParser.g4`, lines 386, 454 and 711; `docs/strict-syntax.mdx`, lines 289 to 326).

## 13. Files the parser could not read

44 pipeline files in 26 pipelines have a syntax error for tree-sitter-nextflow.
tree-sitter recovers from the error, so the rest of each file is still counted.
The table groups the files by their first error.
The last column says what the strict parser does with the same code.
The full list, with file and line, is in `scripts.json` under `parse_errors`.

| First error | Files | Pipelines | Strict parser |
|---|---:|---|---|
| C-style or Java-style `for` loop (`for (int i = 0; ...)`, `for (String x: list)`) | 17 | callingcards, chipseq, evexplorer, meerpipe, metaboigniter, panoramaseq, phageannotator, readsimulator, spinningjenny | Rejected: `for` is removed (`docs/strict-syntax.mdx`, lines 139 to 159; `ScriptLexer.g4`, line 300). |
| Several variables in one `def` (`def reads1 = [], reads2 = []`, old copies of nf-core modules) | 5 | denovotranscript, marsseq, nascent, rnasplice | Rejected (`docs/strict-syntax.mdx`, lines 289 to 326). |
| Assignment inside an expression (`f(x = 1)`, `a = b = c`) | 4 | lncpipe, multiplesequencealign, nanoseq, phageannotator | Rejected (`docs/strict-syntax.mdx`, lines 109 to 137). |
| `while` loop | 3 | crisprseq, phageannotator | Rejected (`docs/strict-syntax.mdx`, lines 139 to 159; `ScriptLexer.g4`, line 342). |
| A second statement after the `when:` expression | 3 | viralintegration | Probably rejected: `when:` takes one expression (`ScriptParser.g4`, lines 274 to 276). Not tested. |
| Soft cast `(Map) x` | 2 | denovotranscript, viralrecon | Rejected (`docs/strict-syntax.mdx`, lines 384 to 398). |
| Closure without braces (`.map ( x -> x.trim() )`) | 2 | cutandrun, omicsgenetraitassociation | Rejected: a closure needs braces (`ScriptParser.g4`, lines 701 to 703). |
| `switch` statement | 1 | radseq | Rejected (`docs/strict-syntax.mdx`, lines 161 to 198; `ScriptLexer.g4`, line 333). |
| `include` inside an `if` block | 1 | viralrecon | Rejected: includes are top-level declarations only (`ScriptParser.g4`, lines 100 to 122). |
| XML attribute access `node.@attr` | 1 | mcmicro | Rejected: the `@` token is commented out (`ScriptLexer.g4`, line 843). |
| `emit: html optional true` (option without a colon) | 1 | cutandrun | Unverified. |
| `path "*.bed"` without parentheses inside `tuple` | 1 | deepmodeloptim | Unverified. |
| Backslash at the end of a line | 1 | differentialabundance | Accepted: the strict lexer skips a backslash before a newline (`ScriptLexer.g4`, lines 641 to 643 and 847). This is a gap in tree-sitter-nextflow. |
| `path script` input (`script` used as a name) | 1 | cutandrun | Probably accepted: `script` is a valid identifier (`ScriptParser.g4`, lines 624 to 652). Likely a gap in tree-sitter-nextflow. Not tested. |
| `tuple (` with its elements on the following lines | 1 | scdownstream | Probably accepted: `tuple` is a valid identifier (same lines). Likely a gap in tree-sitter-nextflow. Not tested. |

So 36 of the 44 files use a form that the strict parser rejects, and 3 more probably do.
One file shows a confirmed gap in tree-sitter-nextflow (backslash line continuation), two more are likely gaps, and two are unverified.

## 14. Suggested implementation order

This order follows the counts above.
Each step lists what it adds; the numbers are pipelines unless stated.

### Step 1: what every nf-core module and pipeline needs

- Script structure: `include` with relative paths and aliases, `process`, named workflows with `take:`, `main:` and `emit:`, the entry workflow (with `main:` or without labels), function definitions.
- Statements and expressions: `def`, `if` and `else`, `return`, `def (a, b) = ...`, closures with parameters, list and map literals, indexing, ternary and elvis, `==`, `!=`, `&&`, `||`, `!`, `in`, `instanceof`, `as`, `+`, `-`, `<<`, `..`, `=~`, `==~`, slashy strings as regular expressions.
- Strings: single-quoted, double-quoted and triple-quoted strings, with interpolation in the short forms `$x` and `$x.y` and the long form `${expr}` with any expression inside.
- Processes: the directives `label`, `tag`, `container` and `conda`, evaluated for each task; inputs `tuple`, `val` and `path` with `stageAs:`, `arity:` and `name:`; outputs `tuple`, `val`, `path` and `eval` with `emit:`, `optional:` and `topic:`; `when:`; a `script:` section that ends with a string or with an `if` and `else` that choose the string; `stub:`; `template` (44 pipelines).
- Task context: `task.ext.*` (`args`, `prefix`, `when`, `suffix`, `args2` and so on), `task.cpus`, `task.memory` with `toGiga`, `toMega`, `mega` and `toUnit`, `task.process`, `moduleDir`, `workflow.containerEngine`.
- Channel factories: `empty`, `value`, `of`, `fromPath` with `checkIfExists:`, `fromList`, `topic`.
- The 21 operators used by 29 or more pipelines (section 3), with the options `by:`, `remainder:`, `failOnMismatch:`, `failOnDuplicate:`, `header:`, `sep:`, `name:`, `sort:`, `storeDir:` and `newLine:`.
- Global functions: `file` with `checkIfExists:`, `error`, `exit`, `log.info`, `log.warn`, `log.error`, `log.debug`, `tuple`, `groupKey`.
- The 60 value methods and getters used by 50 or more pipelines (section 4).

### Step 2: the nf-core pipeline template (about 114 pipelines)

- `params.*` reads everywhere, and `params.x = value` at the top of `main.nf` (54 pipelines).
- `workflow.onComplete` and `workflow.onError` called inside a named workflow, the `workflow.*` run metadata, `workflow.toMap()`, `getVal()` on the value returned by `toList`.
- The nf-schema plugin functions `paramsSummaryMap`, `paramsSummaryLog`, `validateParameters`, `samplesheetToList` and `paramsHelp`.
- The Java and Groovy classes used by the utility subworkflows: YAML (`org.yaml.snakeyaml.Yaml`), JSON (`groovy.json.JsonOutput`, `groovy.json.JsonGenerator.Options`, `groovy.json.JsonSlurper`), templates (`groovy.text.GStringTemplateEngine`, `groovy.text.SimpleTemplateEngine`), files (`File`, `nextflow.extension.FilesEx.copyTo`), `java.util.Date`, `URL.openConnection`, `execute()` on strings and lists, `System.exit`.
- iunctus can either run these subworkflows as code or provide their functions natively.
  The counts do not decide this.
  They only show that these three subworkflows are the largest single source of Groovy and Java use.

### Step 3: less common features (roughly 5 to 50 pipelines)

- Operators `toSortedList`, `dump`, `subscribe`, `cross`, `splitText`, `splitFasta`, `tap`, `collate`, `count`, `merge`; the channel pipe `|` (8 pipelines).
- Closure values for directives (28 pipelines), `publishDir` in scripts (8), `each`, `env` and `stdout` inputs and outputs, `exec:` sections (4).
- The output block with `publish:` (12 pipelines), and the `params { }` block (3).
- `try`, `catch` and `throw` in pipeline code, `assert`, spread `*.`, safe navigation `?.`, `<=>`, `..<`, number unit suffixes such as `2.GB`.
- The deprecated `channel.from` factory (24 pipelines).
  It is deprecated, not rejected (`docs/tutorials/static-types.mdx`, lines 568 to 580).

