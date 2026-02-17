<!-- Which Nextflow config features the nf-core pipelines use, measured to order config support in iunctus -->
<!-- Copyright (c) 2026 The iunctus Contributors -->
<!-- SPDX-License-Identifier: MIT or Apache-2.0 -->

# Nextflow config features in nf-core pipelines

This document measures which Nextflow configuration features the nf-core pipelines use in their `.config` files.
The goal is to decide the order in which iunctus implements config support.

Source paths that start with `modules/` or `docs/` are in the Nextflow checkout at commit bbf6ff0 (2026-09-25).
Raw counts are in `/tmp/iunctus-config-inventory/config.json`.
The scanner and helper scripts are in the same directory.

## Summary

- All 144 pipelines use the same core: `params`, `process` with `withName` and `withLabel` selectors, closures in process settings, `includeConfig`, `profiles`, container scopes (`docker`, `singularity`, `apptainer`, `podman`, `charliecloud`, `shifter`, `conda`), `env`, `manifest`, and `timeline`, `report`, `trace` and `dag`.
- Closures are the most common process setting value. 9,097 of 21,383 process settings are closures, and 3,750 more are `publishDir` maps that contain closures.
- Inside process closures, the most used names are `task.attempt` (144 pipelines), `task.exitStatus` (142), `params` (133), `meta` (110), `task.process` (104) and `workflow.profile` (88).
- `includeConfig` appears 1,880 times. Almost all targets are relative paths. 110 pipelines use a ternary include with `System.getenv('NXF_OFFLINE')`. All 144 pipelines include remote nf-core/configs files through a `params` URL.
- Selectors are simple. No pipeline uses negation (`!`) or nested selectors. 81 pipelines use regular expression characters in `withName`, and 53 use colon-qualified names (74 and 47 in loaded files).
- 126 pipelines declare plugins. `nf-schema` is used by 109 pipelines, `nf-validation` by 17.
- 88 of 144 pipelines pass `nextflow lint` on every config file that a run loads. 54 pipelines have strict-syntax errors in loaded files, and 2 more only miss include targets. The scanner finds these rejected constructs in loaded files: function definitions such as `def check_max(...)` (35 pipelines), `try`/`catch` (34), `if` statements (31), `def` variables (31), and references to `manifest` or `validation` settings as variables (16, as reported by lint).
- Not used at all: `google`, `azure`, `k8s`, `tower`, `lineage`, `fusion`, `spack`, `mail`, `notification`, `weblog`, `seqera`, `sarus` and `agent` scopes, negated selectors, nested selectors, and absolute include paths other than `/dev/null`.

## Recommended order for config support

Each step lists the evidence from the tables below. Pipeline counts refer to loaded files unless a note says otherwise.

1. **Parser and value model (144 pipelines).** Parse the strict config grammar (`modules/nf-lang/src/main/antlr/ConfigParser.g4:98-165`): assignments with dotted paths, blocks, selectors and the `plugins` apply block. Support comments, single and double quoted strings, triple-quoted strings, GStrings with `$name.path` and `${expr}` (`modules/nf-lang/src/main/antlr/ConfigLexer.g4:140-196`), slashy strings (13 pipelines), numbers with units (`GB` and `h` in all 144 pipelines), lists and maps. Evaluate `params` in order, with references to other params and to `projectDir` (88 pipelines). Support `new java.util.Date().format(...)`, which 134 pipelines use for `params.trace_report_suffix` or `trace_timestamp`.
2. **Includes and profiles (144 pipelines).** Resolve relative include paths against the including file (`modules/nextflow/src/main/groovy/nextflow/config/parser/v2/ConfigDsl.groovy:247-257`). Allow includes at the top level and inside profiles only (`modules/nf-lang/src/main/java/nextflow/config/control/VariableScopeVisitor.java:199-204`). Evaluate ternary include targets, `/dev/null`, `System.getenv` and `String.startsWith`. Decide early how to treat the remote nf-core/configs include that every pipeline has. Apply profiles in command line order, as the strict parser does (`docs/config.mdx:315-325`).
3. **Process scope and selectors (144 pipelines).** Implement `withName` and `withLabel` as full-match regular expressions (`modules/nextflow/src/main/groovy/nextflow/script/dsl/ConfigSelectorResolver.groovy:98-116`) and the documented priority order (`docs/config.mdx:256-285`). Colon-qualified names and `.*` prefixes are common. Negation and nesting can wait.
4. **Lazy closures in process settings (144 pipelines).** Evaluate closures per task with `task.attempt`, `task.exitStatus`, `task.process`, `task.cpus`, `task.memory`, `task.ext`, `meta`, other process inputs, `params` and `workflow.profile`. Closures need the ternary operator, `in` with ranges and list `+`, memory and duration arithmetic (`6.GB * task.attempt`), negative list indexes, GStrings, closure parameters (`filename -> ...`) and common string and list methods (`equals`, `tokenize`, `toLowerCase`, `contains`, `join`, `trim`, `endsWith`). 26 pipelines also use statements such as `if`, `def` and `return` inside closures.
5. **`publishDir` maps (123 pipelines).** Keys `path`, `mode`, `saveAs`, `pattern` and `enabled`, as a map or a list of maps.
6. **Runtime scopes (all or nearly all pipelines).** Container engine flags and options, `wave`, `env`, `timeline`, `report`, `trace`, `dag`, `manifest`, `cleanup`, `dumpHashes`, `resourceLimits` (107), `nextflow.enable.configProcessNamesValidation` (122), and `outputDir` with `workflow.output.mode` (62).
7. **Plugins and `validation` (126 and 105 pipelines).** Parse `plugins { id 'name@version' }` and the `validation` scope. Whether iunctus implements nf-schema behaviour natively is a separate decision.
8. **Rare features.** `executor` (50 pipelines, mostly in a `gitpod` profile), `aws` (6 loaded), `secrets` (3 loaded), `System.getProperty` (2), `Math` functions (4), `prov`, `co2footprint` and `slack` plugin scopes.

Constructs that the strict parser rejects should produce clear errors (see the section on statements that strict syntax rejects). They affect 54 pipelines today.

## Corpus

- Location: `~/.cache/iunctus/corpus/nf-core-text`, text-only copy with `.nf`, `.config` and `nextflow_schema.json` files.
- 144 pipelines under `pipelines/<name>/`. The file dates show the copy was made on 2026-09-27.
- Revisions come from `revisions.txt`. It records one branch and one commit per pipeline: 83 pipelines on `master`, 43 on `dev` and 18 on `main`. Appendix A lists all of them.
- The nf-core/modules repository copy under `modules/` was scanned too. It is reported separately in the section on the modules repository.

## Method

### Scanner

tree-sitter-nextflow has no config grammar, so I wrote a scanner in Python: `/tmp/iunctus-config-inventory/scan.py`.
It follows the official grammar and lexer:

- The lexer follows `modules/nf-lang/src/main/antlr/ConfigLexer.g4`. It handles `//` and `/* */` comments (lines 822-830), single, double and triple-quoted strings (lines 140-147), GStrings with `$name.path` and `${...}` parts (lines 149-196), slashy strings only where a regular expression may start (lines 87-109 and 143), numbers, identifiers, keywords and all operators. Text inside strings and comments is never counted as code. The expressions inside `${...}` are lexed and analysed as code.
- A bracket pass groups `( )`, `[ ]` and `{ }`, so every count knows its nesting.
- The statement parser follows `modules/nf-lang/src/main/antlr/ConfigParser.g4:98-174`: `configInclude`, `configAssign`, `configBlock`, `configSelector`, `configApplyBlock` and `configIncomplete`. It also recognises the statements that strict syntax rejects (`if`, `try`, `def`, typed declarations, function definitions, expression statements). The bodies of legacy `if` and `try` blocks are parsed as config statements, so settings inside them are still counted and marked as guarded.
- Expression ends follow the grammar's newline rules. A newline may follow a binary operator. It may also come before `.`, `?.`, `?`, `:` and the comparison and logical operators (`ConfigParser.g4:274-342` and `356-363`).
- Expression analysis records root names and property chains (such as `task.attempt`), function calls, method calls, static calls (such as `System.getenv`), constructors, ternaries, GStrings, closures, closure parameters and statements inside closures. Map keys and named arguments are not counted as references.
- Setting values are classified as: closure, constant (literal, unit literal such as `8.GB`, or a list or map of constants), list or map that contains closures, or other expression (evaluated when the config is loaded).

### What is counted

- **Occurrences**: the number of times a feature appears.
- **Pipelines**: the number of pipelines where any `.config` file uses the feature.
- **Loaded**: the same counts, but only for files that a pipeline run can load. These are `nextflow.config` and every file reachable from it through `includeConfig` with a static target. The scanner follows all branches of ternaries and all profiles. Remote targets are not followed. Relative targets are resolved against the including file, as Nextflow does (`ConfigDsl.groovy:247-257`).

The "all files" columns include test configs such as `modules/nf-core/*/tests/nextflow.config`, which nf-test uses but a pipeline run does not.

### Excluded files

The corpus has 4,823 `.config` files. I excluded 113 files that are not Nextflow config files:

- 110 `nf-test.config` files. nf-test reads them, not Nextflow. `nextflow lint` also excludes them by default (`modules/nf-cli-v1/src/main/groovy/nextflow/cli/CmdLint.groovy:69`, `docs/reference/cli/lint.mdx:24`).
- 3 tool config files under `assets/`: `genomeannotator/assets/pasa/alignAssembly.config`, `genomeannotator/assets/pasa/annotationCompare.config` and `provenancereport/tests/assets/quartonotebook_rds.config`.

That leaves 4,710 scanned files. 1,690 of them are loaded files.

### Check against the official parser

I ran `nextflow lint -o json` (Nextflow 26.04.6, the local binary, not built from the checkout) on all 4,823 files. The command and raw output are in `/tmp/iunctus-config-inventory/lint/`.

For files that lint could parse to the end, the scanner matches lint exactly:

| Construct | Scanner | Lint |
|---|---|---|
| Outermost `if` statements | 125 in 24 files | 125 in 24 files |
| `try`/`catch` blocks | 2 in 2 files | 2 in 2 files |
| Variable declarations | 12 in 8 files | 12 in 8 files |
| Files with function definitions | 35 | 35 (`Unexpected input: '('`) |

Lint stops at the first parse error in a file. So in files with a function definition or a selector inside an `if` block, lint reports fewer errors than the scanner finds. The script is `/tmp/iunctus-config-inventory/compare_lint.py`.

Plain text search gives the same totals, minus text in comments: `withLabel` 1,309 in both, `withName` 9,735 against 9,737, `includeConfig` 1,880 against 1,882. The small gaps are mentions inside `/* */` comments.

The scanner found one statement it could not classify (`multiplesequencealign/conf/modules.config:179`, a stray `saveAs:` label inside a selector) and two lexer errors (`hgtseq/conf/test_full.config:15`, an unclosed `{`; `methylarray/conf/test_full.config:22`, a `#` character). Lint reports the same three problems.

## Files

| Location | Scanned files | Loaded files |
|---|---:|---:|
| `modules/` and `subworkflows/` | 2,472 | 70 |
| `conf/` | 1,963 | 1,471 |
| `nextflow.config` at the pipeline root | 144 | 144 |
| `tests/` | 123 | 0 |
| Other | 8 | 5 |
| Total | 4,710 | 1,690 |

Notes:

- 54 pipelines ship eight generated `conf/containers_*.config` files each. Only `seqsubmit` includes them. In the other pipelines they are not loaded, so they only appear in the "all files" columns.
- 21 include targets in 10 pipelines are missing from the corpus. 7 are `seqsubmit` `.conf` files, which the text-only corpus does not keep. The other 14 are `.config` files that are not in the corpus, for example the `eager` test profiles `conf/test_bam.config` and others at `eager/nextflow.config:365-369`. The list is in `config.json` under `missing_include_targets`.

## Strict syntax status

`nextflow lint` errors in the 4,710 Nextflow config files, sorted by pipelines:

| Lint error message | Occurrences | Files | Pipelines | Pipelines (loaded) |
|---|---:|---:|---:|---:|
| `<name>` is not defined | 774 | 64 | 51 | 51 |
| Unexpected input: '(' | 35 | 35 | 35 | 35 |
| If statements cannot be mixed with config statements | 125 | 24 | 10 | 8 |
| Variable declarations cannot be mixed with config statements | 12 | 8 | 8 | 5 |
| Unexpected input: ':' | 8 | 8 | 8 | 8 |
| Invalid include source: `<path>` | 10 | 4 | 4 | 4 |
| Try-catch blocks cannot be mixed with config statements | 2 | 2 | 2 | 0 |
| Unexpected input: '<EOF>' | 1 | 1 | 1 | 1 |
| Unexpected character: '#' | 1 | 1 | 1 | 1 |
| Unexpected input: '{' | 1 | 1 | 1 | 1 |

The undefined names are `check_max` (641 times in 34 pipelines), `manifest` (73 in 16), `validation` (32 in 16), `trace_timestamp` (12 in 3) and a few single cases.
The messages come from `modules/nf-lang/src/main/java/nextflow/config/parser/ConfigAstBuilder.java:348-359` (mixed statements) and `modules/nf-lang/src/main/java/nextflow/config/control/ConfigResolveVisitor.java:89-98` and `VariableScopeVisitor.java:285-296` (undefined names).
"Unexpected input: '('" is the parser failing at `def check_max(` and similar function definitions.
"Unexpected input: ':'" is a `withName:` selector inside an `if` block, where the grammar expects ordinary statements.

Per pipeline, looking only at loaded files:

- 88 pipelines have no lint errors (49 on `master`, 26 on `dev`, 13 on `main`).
- 2 pipelines (`methylseq`, `seqsubmit`) only have errors for missing include targets.
- 54 pipelines have real strict-syntax errors. 35 of them define `check_max` or another function.
- In the other 19 pipelines, 15 refer to `manifest` and `validation` settings as variables in the `validation.help` text. For 6 of them this is the only error. The other causes are `def` variables (5 pipelines), selectors inside `if` blocks (4), `if` statements (3), `trace_timestamp` (3) and single cases.

Lint also reports 64 warnings "Implicit closure parameter is deprecated" (use of `it`), from `VariableScopeVisitor.java:342-343`.

## Scopes

### Top-level scopes

Settings under each top-level scope. Profile blocks are unwrapped, so `profiles { docker { docker.enabled = true } }` counts under `docker`. `(root)` means a setting without a scope, such as `cleanup`.

| Scope | Pipelines | Occurrences | Pipelines (loaded) | Occurrences (loaded) |
|---|---:|---:|---:|---:|
| `params` | 144 | 45,085 | 144 | 39,379 |
| `process` | 144 | 21,383 | 144 | 15,850 |
| `manifest` | 144 | 2,815 | 144 | 1,291 |
| `singularity` | 144 | 1,707 | 144 | 1,683 |
| `docker` | 144 | 1,624 | 144 | 1,593 |
| `trace` | 144 | 307 | 144 | 298 |
| `report` | 144 | 302 | 144 | 293 |
| `timeline` | 144 | 302 | 144 | 293 |
| `dag` | 144 | 298 | 144 | 289 |
| `profiles` (profile blocks) | 144 | 2,577 blocks | 144 | 2,569 blocks |
| `env` | 143 | 663 | 143 | 592 |
| `podman` | 140 | 1,222 | 140 | 1,220 |
| `charliecloud` | 138 | 1,194 | 138 | 1,192 |
| `shifter` | 138 | 1,089 | 138 | 1,087 |
| `conda` | 134 | 1,302 | 133 | 1,296 |
| `(root)` | 133 | 375 | 133 | 362 |
| `apptainer` | 128 | 1,583 | 128 | 1,581 |
| `plugins` (`id` entries) | 126 | 247 | 126 | 144 |
| `nextflow` | 123 | 258 | 123 | 248 |
| `wave` | 113 | 618 | 113 | 618 |
| `validation` | 109 | 552 | 105 | 353 |
| `aws` | 99 | 107 | 6 | 6 |
| `workflow` | 62 | 62 | 62 | 62 |
| `executor` | 50 | 158 | 50 | 158 |
| `prov` | 4 | 12 | 4 | 12 |
| `co2footprint` | 3 | 9 | 3 | 9 |
| Selector outside any scope | 3 | 9 | 3 | 9 |
| `slack` | 1 | 2 | 1 | 2 |
| `google`, `azure`, `k8s`, `tower`, `lineage`, `fusion`, `spack`, `mail`, `notification`, `weblog`, `seqera`, `sarus`, `agent` | 0 | 0 | 0 | 0 |

Notes:

- `aws` is almost only `aws.client.anonymous = true` in test configs (98 pipelines, 5 loaded).
- `executor` is mostly `executor.name`, `executor.cpus` and `executor.memory` in a `gitpod` profile (46 of the 47 pipelines that set `executor.cpus`).
- "Selector outside any scope" means a `withName:` block inside a top-level `if` block, outside `process` (`cutandrun/conf/resources.config:26`, `nanoseq/conf/modules.config:64`, `rnasplice/conf/modules.config:883`). Nextflow only treats selectors as selectors in the `process` and `agent` scopes (`ConfigDsl.groovy:45` and `225`).

### Nested scopes

A nested scope is a scope inside a top-level scope that has settings below it. Selectors are not counted here.

| Nested scope | Pipelines | Occurrences | Pipelines (loaded) | Occurrences (loaded) |
|---|---:|---:|---:|---:|
| `process.ext` | 130 | 8,629 | 127 | 5,084 |
| `nextflow.enable` | 123 | 257 | 123 | 248 |
| `aws.client` | 98 | 106 | 5 | 5 |
| `workflow.output` | 62 | 62 | 62 | 62 |
| `validation.help` | 20 | 107 | 20 | 107 |
| `validation.summary` | 17 | 34 | 17 | 34 |
| `prov.formats` | 4 | 8 | 4 | 8 |
| `process.process` (a `process` block inside `process`, inside an `if`) | 1 | 2 | 1 | 2 |
| `executor.$slurm` (legacy per-executor scope) | 1 | 1 | 1 | 1 |
| `slack.webhook` | 1 | 1 | 1 | 1 |
| `validation.logging` | 1 | 1 | 1 | 1 |

### Settings outside `process` and `params`

Settings used by at least 8 pipelines. The full list is in `config.json` under the `setting_path|` keys.

| Setting | Pipelines | Occurrences | Pipelines (loaded) | Occurrences (loaded) |
|---|---:|---:|---:|---:|
| `singularity.enabled` | 144 | 1,105 | 144 | 1,098 |
| `docker.enabled` | 144 | 1,098 | 144 | 1,094 |
| `manifest.nextflowVersion` | 144 | 369 | 144 | 144 |
| `manifest.description` | 144 | 363 | 144 | 146 |
| `manifest.homePage` | 144 | 362 | 144 | 145 |
| `manifest.name` | 144 | 362 | 144 | 146 |
| `manifest.version` | 144 | 362 | 144 | 146 |
| `report.enabled`, `timeline.enabled`, `trace.enabled` | 144 | 154 each | 144 | 145 each |
| `dag.enabled` | 144 | 153 | 144 | 144 |
| `manifest.mainScript` | 144 | 145 | 144 | 145 |
| `report.file`, `timeline.file`, `trace.file` | 144 | 145 each | 144 | 145 each |
| `dag.file` | 144 | 144 | 144 | 144 |
| `singularity.autoMounts` | 143 | 149 | 143 | 143 |
| `env.PYTHONNOUSERSITE` | 143 | 147 | 143 | 147 |
| `podman.enabled` | 140 | 1,091 | 140 | 1,089 |
| `env.R_ENVIRON_USER`, `env.R_PROFILE_USER` | 140 | 140 each | 140 | 140 each |
| `shifter.enabled` | 138 | 1,089 | 138 | 1,087 |
| `charliecloud.enabled` | 138 | 1,086 | 138 | 1,084 |
| `docker.runOptions` | 138 | 368 | 138 | 352 |
| `manifest.author` | 135 | 271 | 54 | 55 |
| `env.JULIA_DEPOT_PATH` | 133 | 133 | 133 | 133 |
| `conda.enabled` | 131 | 1,048 | 131 | 1,044 |
| `manifest.doi` | 130 | 347 | 130 | 131 |
| `conda.useMamba` | 130 | 131 | 130 | 131 |
| `apptainer.enabled` | 128 | 1,027 | 128 | 1,025 |
| `cleanup` | 127 | 139 | 127 | 131 |
| `docker.registry` | 127 | 131 | 127 | 130 |
| `podman.registry` | 127 | 130 | 127 | 130 |
| `dumpHashes` | 126 | 126 | 126 | 126 |
| `apptainer.registry`, `singularity.registry` | 125 | 126 each | 125 | 126 each |
| `nextflow.enable.configProcessNamesValidation` | 122 | 243 | 122 | 243 |
| `apptainer.autoMounts` | 120 | 122 | 120 | 122 |
| `apptainer.ociAutoPull`, `singularity.ociAutoPull` | 113 | 206 each | 113 | 206 each |
| `wave.enabled`, `wave.freeze`, `wave.strategy` | 113 | 206 each | 113 | 206 each |
| `conda.channels` | 112 | 114 | 112 | 114 |
| `validation.monochromeLogs` | 108 | 200 | 101 | 101 |
| `charliecloud.registry` | 108 | 108 | 108 | 108 |
| `manifest.defaultBranch` | 107 | 108 | 107 | 107 |
| `manifest.contributors` | 106 | 106 | 106 | 106 |
| `validation.defaultIgnoreParams` | 102 | 106 | 102 | 106 |
| `validation.parametersSchema` | 100 | 100 | 0 | 0 |
| `aws.client.anonymous` | 98 | 106 | 5 | 5 |
| `singularity.runOptions` | 95 | 97 | 95 | 97 |
| `apptainer.runOptions` | 94 | 97 | 94 | 97 |
| `outputDir` | 62 | 62 | 62 | 62 |
| `workflow.output.mode` | 62 | 62 | 62 | 62 |
| `executor.name` | 48 | 50 | 48 | 50 |
| `executor.cpus`, `executor.memory` | 47 | 47 each | 47 | 47 each |
| `validation.help.enabled` | 19 | 19 | 19 | 19 |
| `validation.help.*` and `validation.summary.*` text settings | 17 to 18 | 17 to 18 each | 17 to 18 | 17 to 18 each |
| `manifest.diagram` | 15 | 15 | 15 | 15 |
| `docker.userEmulation` | 13 | 14 | 12 | 12 |
| `singularity.pullTimeout` | 8 | 18 | 5 | 10 |
| `channels` (root setting, a mistake for `conda.channels`) | 8 | 8 | 8 | 8 |

Root settings (no scope): `cleanup` (127 pipelines), `dumpHashes` (126), `outputDir` (62), `workDir` (4), `stubRun` (3), and single cases. Some root settings are mistakes in the pipelines, for example `channels` inside a `conda` profile (`circdna/nextflow.config:135`).

Almost all settings outside `process` and `params` are constants: 15,483 constants against 1,078 other expressions. The expressions are mostly GStrings such as `"${params.outdir}/pipeline_info/..."` for the report files.

## `includeConfig`

| Include feature | Pipelines | Occurrences | Pipelines (loaded) | Occurrences (loaded) |
|---|---:|---:|---:|---:|
| All `includeConfig` statements | 144 | 1,880 | 144 | 1,853 |
| Target: relative path string | 144 | 1,645 | 144 | 1,622 |
| Guard: none (unconditional) | 144 | 1,563 | 144 | 1,539 |
| Location: top level | 144 | 1,038 | 144 | 1,011 |
| Location: inside a profile | 144 | 842 | 144 | 842 |
| Target: GString built from `params` (nf-core/configs URL) | 144 | 223 | 144 | 223 |
| Guard: ternary expression | 110 | 241 | 110 | 241 |
| Target: `/dev/null` | 110 | 185 | 110 | 185 |
| Guard: `try`/`catch` (rejected by strict syntax) | 36 | 41 | 34 | 39 |
| Guard: `if` statement (rejected by strict syntax) | 24 | 26 | 24 | 26 |
| Target: literal URL | 3 | 3 | 0 | 0 |
| Guard: closure that is called in place | 2 | 9 | 1 | 8 |
| Target: absolute path other than `/dev/null` | 0 | 0 | 0 | 0 |
| Target: GString built from `projectDir` or `baseDir` | 0 | 0 | 0 | 0 |
| Location: inside another scope (not allowed) | 0 | 0 | 0 | 0 |
| Location: directly in `profiles { }` (not allowed) | 0 | 0 | 0 | 0 |

A statement with a ternary counts all branch targets. For example the common line
`includeConfig params.custom_config_base && (!System.getenv('NXF_OFFLINE') || !params.custom_config_base.startsWith('http')) ? "${params.custom_config_base}/nfcore_custom.config" : "/dev/null"`
counts as one `params` target and one `/dev/null` target.

Notes:

- Every pipeline sets `custom_config_base = "https://raw.githubusercontent.com/nf-core/configs/${params.custom_config_version}"` in `nextflow.config`. So by default every pipeline includes remote files over HTTPS, unless `NXF_OFFLINE` is set. Nextflow opens such a path through its file system layer (`modules/nf-commons/src/main/nextflow/file/FileHelper.groovy:338-354` and the HTTP providers in `modules/nf-httpfs/src/main/nextflow/file/http/`). What happens when the download fails without a `try` block is unverified.
- The other common ternary include is `includeConfig !params.igenomes_ignore ? 'conf/igenomes.config' : 'conf/igenomes_ignored.config'`.
- The strict parser allows includes only at the top level or in a profile (`VariableScopeVisitor.java:199-204`, `docs/config.mdx:94`). The corpus follows this rule.
- The strict way to include conditionally is a closure that is called in place (`docs/strict-syntax.mdx:652-666`). Only `proteinfold/nextflow.config:613` does this in a loaded file.

## Process selectors

| Selector feature | Pipelines | Occurrences | Pipelines (loaded) | Occurrences (loaded) |
|---|---:|---:|---:|---:|
| All selectors | 144 | 11,044 | 144 | 6,996 |
| `withLabel` | 144 | 1,309 | 144 | 1,307 |
| `withLabel` with unquoted label | 143 | 1,293 | 143 | 1,291 |
| `withName` | 142 | 9,735 | 140 | 5,689 |
| `withName` with unquoted name | 131 | 3,263 | 102 | 1,916 |
| `withName` with quoted pattern | 128 | 6,472 | 120 | 3,773 |
| `withName` with regular expression characters | 81 | 1,868 | 74 | 1,613 |
| `withName` containing `.*` | 64 | 1,681 | 57 | 1,451 |
| `withName` with a colon (qualified name) | 53 | 1,985 | 47 | 1,660 |
| `withName` qualified and starting with `.*` | 42 | 1,364 | 38 | 1,182 |
| `withName` with alternation (`\|`) | 41 | 245 | 38 | 213 |
| `withName` qualified with an exact prefix | 28 | 621 | 22 | 478 |
| Selector inside a legacy `if` block | 14 | 453 | 14 | 451 |
| Selector inside a profile | 5 | 39 | 4 | 37 |
| `withLabel` with quoted label | 3 | 16 | 3 | 16 |
| Selector outside `process` | 3 | 4 | 3 | 4 |
| `withLabel` with regular expression characters | 1 | 1 | 1 | 1 |
| Negated selector (`!`) | 0 | 0 | 0 | 0 |
| Selector nested in another selector | 0 | 0 | 0 | 0 |
| Selector written as a quoted block name (`'withName:X' { }`) | 0 | 0 | 0 | 0 |
| Selector target with interpolation | 0 | 0 | 0 | 0 |

Examples: `withName: '.*:MERGED_LIBRARY_MARKDUPLICATES_PICARD:PICARD_MARKDUPLICATES'` (`atacseq/conf/modules.config:357`), `withName: 'NFCORE_ATACSEQ:ATACSEQ:.*:BAM_SORT_STATS_SAMTOOLS:SAMTOOLS_SORT'` (`atacseq/conf/modules.config:205`), `withName: 'MERGE_STATS_STD|MERGE_STATS_FILTERSSU|...'` (`ampliseq/conf/modules.config:482`). The only `withLabel` pattern is `withLabel:'.*'` in `smrnaseq/conf/ci.config:3`.

Behaviour to implement:

- Both selector kinds are full-match regular expressions. A leading `!` negates the match (`ConfigSelectorResolver.groovy:98-116`). So even plain names are regular expressions. This matters for names with `.` or `*`.
- Selectors are only valid in the `process` and `agent` scopes (`ConfigDsl.groovy:45`, `218-233`). The grammar only allows a selector inside a block (`ConfigParser.g4:139-151`).
- A qualified name matches only the exact fully qualified process name. Priority runs from generic settings, to `withLabel`, to `withName` on the simple name, the alias and the fully qualified name (`docs/config.mdx:213-285`).
- Nesting depth is always one. No selector is inside another selector.

## Process settings

Settings inside `process` scopes, with selectors removed from the path. Settings used by at least 3 pipelines are listed. 18,802 of the 21,383 process settings are inside a selector.

| Setting | Pipelines | Occurrences | Pipelines (loaded) | Closure | Constant | List or map with closures | Other expression |
|---|---:|---:|---:|---:|---:|---:|---:|
| `memory` | 144 | 1,522 | 144 | 1,431 | 91 | 0 | 0 |
| `time` | 144 | 1,276 | 144 | 1,251 | 25 | 0 | 0 |
| `cpus` | 144 | 1,220 | 144 | 1,059 | 158 | 0 | 3 |
| `errorStrategy` | 144 | 529 | 144 | 220 | 304 | 0 | 5 |
| `maxRetries` | 144 | 341 | 144 | 0 | 336 | 0 | 5 |
| `beforeScript` | 144 | 202 | 144 | 11 | 171 | 0 | 20 |
| `maxErrors` | 143 | 145 | 143 | 0 | 145 | 0 | 0 |
| `shell` | 143 | 143 | 143 | 0 | 143 | 0 | 0 |
| All `ext.*` settings | 130 | 8,629 | 127 | 4,609 | 3,261 | 0 | 759 |
| `ext.args` | 128 | 4,389 | 124 | 1,290 | 2,474 | 0 | 625 |
| `publishDir` | 126 | 4,449 | 124 | 59 | 593 | 3,749 | 48 |
| `ext.prefix` | 118 | 3,199 | 91 | 2,834 | 349 | 0 | 16 |
| `resourceLimits` | 108 | 499 | 107 | 0 | 499 | 0 | 0 |
| `accelerator` | 92 | 116 | 92 | 96 | 20 | 0 | 0 |
| `arch` | 91 | 92 | 91 | 0 | 92 | 0 | 0 |
| `ext.use_gpu` | 90 | 98 | 90 | 94 | 4 | 0 | 0 |
| `container` | 74 | 1,422 | 25 | 83 | 1,331 | 0 | 8 |
| `ext.args2` | 67 | 351 | 48 | 90 | 204 | 0 | 57 |
| `conda` | 37 | 323 | 13 | 0 | 284 | 0 | 39 |
| `cache` | 32 | 49 | 32 | 0 | 49 | 0 | 0 |
| `ext.suffix` | 29 | 185 | 23 | 36 | 137 | 0 | 12 |
| `ext.when` | 22 | 181 | 21 | 160 | 19 | 0 | 2 |
| `tag` | 19 | 219 | 19 | 219 | 0 | 0 | 0 |
| `containerOptions` | 18 | 53 | 14 | 38 | 15 | 0 | 0 |
| `ext.args3` | 16 | 41 | 13 | 13 | 22 | 0 | 6 |
| `label` | 13 | 29 | 13 | 3 | 26 | 0 | 0 |
| `maxForks` | 10 | 30 | 9 | 0 | 25 | 0 | 5 |
| `scratch` | 5 | 15 | 5 | 0 | 15 | 0 | 0 |
| `queue` | 5 | 9 | 5 | 3 | 5 | 0 | 1 |
| `executor` | 5 | 8 | 4 | 0 | 8 | 0 | 0 |
| `storeDir` | 4 | 13 | 4 | 12 | 0 | 0 | 1 |
| `ext.args4` | 3 | 7 | 3 | 5 | 1 | 0 | 1 |
| `stageInMode` | 3 | 4 | 2 | 0 | 4 | 0 | 0 |
| `clusterOptions` | 3 | 3 | 3 | 2 | 1 | 0 | 0 |

`maxErrors` and `maxRetries` are never closures. `tag` is always a closure, and `storeDir` almost always. `container` settings come mostly from the unloaded `conf/containers_*.config` files.
In a process definition, a closure makes a directive dynamic, so it is evaluated for each task (`docs/process.mdx:1183-1204`). The docs list `executor`, `label`, `maxForks` and `secret` as directives that cannot be dynamic (`docs/process.mdx:1206-1211`). The corpus has 3 `label` closures.

Value kinds over all process settings: 9,097 closures, 7,641 constants, 3,750 lists or maps that contain closures, and 895 other expressions (evaluated once at load time, usually from `params`).

### `publishDir` and `resourceLimits` shapes

| Shape or key | Pipelines | Occurrences | Pipelines (loaded) | Occurrences (loaded) |
|---|---:|---:|---:|---:|
| `publishDir` key `path` | 125 | 4,192 | 123 | 4,053 |
| `publishDir` key `mode` | 125 | 4,052 | 123 | 3,947 |
| `publishDir` as a single map | 125 | 3,950 | 122 | 3,824 |
| `publishDir` key `saveAs` | 122 | 2,396 | 120 | 2,303 |
| `resourceLimits` as a map with `cpus`, `memory`, `time` | 108 | 499 | 107 | 457 |
| `publishDir` key `pattern` | 82 | 1,762 | 80 | 1,695 |
| `publishDir` key `enabled` | 81 | 1,590 | 79 | 1,536 |
| `publishDir` as a list of maps | 47 | 413 | 46 | 400 |
| `publishDir` that is not a list or map literal | 23 | 83 | 2 | 24 |
| `publishDir` key `overwrite` | 6 | 51 | 6 | 51 |

Lists of maps have 1 to 10 entries, most often 2 or 3.
The standard form (`abotyper/conf/modules.config:13-17`) is:

```groovy
publishDir = [
    path: { "${params.outdir}/${task.process.tokenize(':')[-1].tokenize('_')[0].toLowerCase()}" },
    mode: params.publish_dir_mode,
    saveAs: { filename -> filename.equals('versions.yml') ? null : filename }
]
```

## Closures in process settings

A closure in a process setting can use the process DSL, and it may use names that are not declared, because they can be process inputs (`VariableScopeVisitor.java:118-128`, `144-162` and `331-341`).

### References

| Reference | Pipelines | Occurrences | Pipelines (loaded) | Occurrences (loaded) |
|---|---:|---:|---:|---:|
| `task` (any property) | 144 | 4,185 | 144 | 4,085 |
| `task.attempt` | 144 | 3,510 | 144 | 3,487 |
| `task.exitStatus` | 142 | 217 | 142 | 215 |
| `params` | 133 | 9,070 | 130 | 8,626 |
| `meta` (any property) | 110 | 6,366 | 102 | 5,223 |
| `meta.id` | 106 | 3,886 | 97 | 2,924 |
| `task.process` | 104 | 203 | 101 | 137 |
| `workflow` (any property) | 90 | 331 | 90 | 320 |
| `workflow.profile` | 88 | 185 | 88 | 184 |
| `workflow.containerEngine` | 13 | 139 | 10 | 129 |
| `meta.single_end` | 12 | 42 | 11 | 39 |
| `task.cpus` | 11 | 21 | 10 | 18 |
| `task.ext` (any property) | 7 | 206 | 6 | 202 |
| `task.memory` | 7 | 17 | 6 | 15 |
| `task.tag` | 2 | 8 | 2 | 8 |

Other `meta` properties appear in 2 to 6 pipelines each (`meta.sample`, `meta.read_group`, `meta.strandedness`, `meta.chunk`, `meta.patient` and many more).
Besides `meta`, closures reference 73 other root names 374 times. These are mostly process inputs such as `vcf` (54 times), `input`, `intervals`, `meta2`, `fasta`, `infile` and `reads`.
References to closure parameters and local variables, such as `filename` in `saveAs`, are counted separately (5,430 references in 126 pipelines).

### Function, static and method calls

| Call | Pipelines | Occurrences | Pipelines (loaded) | Occurrences (loaded) |
|---|---:|---:|---:|---:|
| `check_max(...)` (defined in the same config, rejected by strict syntax) | 34 | 669 | 34 | 669 |
| `Math.max`, `Math.pow`, `Math.ceil`, `Math.log`, `Math.round` | 4 | 50 | 4 | 50 |
| `new File(...)` | 3 | 3 | 3 | 3 |
| `parse_int(...)`, `mem_calc(...)`, `archive_size_calc(...)` (functions defined in the config, rejected by strict syntax) | 2 | 16 | 2 | 16 |
| `System.getProperty` | 1 | 13 | 1 | 13 |
| `sleep(...)` | 1 | 4 | 1 | 4 |
| `error(...)` | 1 | 1 | 1 | 1 |

Method calls used by at least 8 pipelines inside process closures:

| Method | Pipelines | Occurrences | Pipelines (loaded) | Occurrences (loaded) |
|---|---:|---:|---:|---:|
| `equals` | 119 | 1,899 | 117 | 1,841 |
| `tokenize` | 106 | 450 | 104 | 317 |
| `toLowerCase` | 105 | 215 | 102 | 149 |
| `contains` | 92 | 311 | 92 | 311 |
| `join` | 64 | 468 | 60 | 410 |
| `trim` | 49 | 353 | 44 | 305 |
| `endsWith` | 24 | 168 | 23 | 165 |
| `split` | 14 | 147 | 14 | 147 |
| `replace` | 9 | 30 | 9 | 25 |
| `unique` | 8 | 43 | 8 | 42 |
| `replaceFirst` | 8 | 40 | 7 | 31 |
| `toString` | 8 | 24 | 7 | 16 |
| `last` | 8 | 16 | 8 | 16 |

Less common methods include `replaceAll`, `flatten`, `startsWith`, `collect`, `first`, `size`, `findAll`, `toGiga`, `get`, `toUpperCase`, `substring`, `indexOf`, `collectEntries`, `lastIndexOf`, `minus` and `min`.

