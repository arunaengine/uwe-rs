<!-- Research notes on CWL and what a native Rust runner needs. -->
<!-- Copyright (c) 2026 The iunctus Contributors -->
<!-- SPDX-License-Identifier: MIT or Apache-2.0 -->

# Common Workflow Language (CWL): spec, preprocessing, expressions, conformance, implementations, TES mapping, and what a native Rust runner needs

Research date: 2026-09-27. All "current" statements are as of this date unless marked otherwise.
Method: I read primary sources. I cloned the spec, test, tool and Rust repositories into /tmp and inspected them directly (cwl-v1.0 to cwl-v1.3, cwltool, cwltest, schema_salad, cwl-tes, fairagro/commonwl, fairagro/sciwin, cwl-website). I also used the GitHub and crates.io APIs, and CI logs.
Test counts below are my own counts from the `conformance_tests.yaml` files (following `$import` entries). They are verified facts about the files, not numbers quoted from a web page.

## 1. What are the current CWL versions, document types and key features?

### Takeaway
CWL v1.2.1 (tagged 2024-01-03) is the current official standard. v1.3.0-dev1 is an unreleased draft whose main new feature is loops. A runner must support four process classes and about 15 requirement types. Most of the difficulty sits in CommandLineTool and in the File/Directory data model, not in the workflow graph.

### Cited Findings
- Release history (GitHub release dates):
  - draft-2: 2015-07-07. draft-3: 2016-02-16. [Source](https://github.com/common-workflow-language/common-workflow-language/releases)
  - v1.0: 2016-07-18. v1.0.1: 2017-07-23. v1.0.2: 2019-07-23. [Source](https://github.com/common-workflow-language/common-workflow-language/releases)
  - v1.1.0: 2019-06-07. [Source](https://github.com/common-workflow-language/cwl-v1.1/releases)
  - v1.2.0: 2020-08-07. v1.2.1: 2024-01-03 ("Official CWL v1.2.1"). [Source](https://github.com/common-workflow-language/cwl-v1.2/releases)
- The cwl-v1.3 repository "holds the `v1.3.0-dev1` in-development version". It has no tags or releases. Its last spec or test commits were in 2025-12 to 2026-04 (for example "Add test on overlapping input, output and step names", 2026-04-29). The last commit was a typo fix on 2026-09-20. [Source](https://github.com/common-workflow-language/cwl-v1.3)
- The v1.3.0-dev1 Workflow changelog adds a Loop feature: `LoopWorkflowStep`, `LoopInput` and `LoopOutputMethod` (`last_iteration` default, or `all_iterations`). The CommandLineTool changelog says undeclared outputs in `cwl.output.json` must be discarded, and that fewer conformance tests now use `ShellCommandRequirement`. [Source](https://github.com/common-workflow-language/cwl-v1.3/blob/main/Workflow.yml), [Source](https://github.com/common-workflow-language/cwl-v1.3/blob/main/CommandLineTool.yml)
- cwltool ships schemas for `v1.3.0-dev1` and an `update.py` function `v1_2to1_3dev1`. Loading development versions needs the `--enable-dev` flag. [Source](https://github.com/common-workflow-language/cwltool/tree/main/cwltool/schemas), [Source](https://github.com/common-workflow-language/cwltool/blob/main/cwltool/argparser.py)
- The CWL website's specification page source still calls v1.2.0 "the current CWL Standards". The rendered spec at /v1.2/ is titled v1.2.1. So the website text is slightly stale. [Source](https://github.com/common-workflow-language/cwl-website/blob/main/content/specification.md), [Source](https://www.commonwl.org/v1.2/Workflow.html)
- Process classes in v1.2:
  - `CommandLineTool`: wraps one command-line program.
  - `ExpressionTool`: runs a JavaScript expression and produces outputs without a container.
  - `Workflow`: a graph of steps.
  - `Operation` (new in v1.2): an abstract placeholder step that cannot run. [Source](https://www.commonwl.org/v1.2/Workflow.html)
- v1.2 Workflow changelog: it added `when` (conditional steps), `pickValue` (on step inputs and workflow outputs), the abstract `Operation` class, and `intent`. `loadContents` must now fail on files over 64 KiB instead of silently truncating. [Source](https://www.commonwl.org/v1.2/Workflow.html#Changelog)
- Workflow feature enums in v1.2:
  - `scatterMethod`: `dotproduct`, `nested_crossproduct`, `flat_crossproduct`.
  - `linkMerge`: `merge_nested`, `merge_flattened`.
  - `pickValue`: `first_non_null`, `the_only_non_null`, `all_non_null`.
  [Source](https://github.com/common-workflow-language/cwl-v1.2/blob/main/Workflow.yml)
- v1.1 added `ToolTimeLimit`, `WorkReuse`, `NetworkAccess`, `InplaceUpdateRequirement`, `LoadListingRequirement`/`loadListing`, and the `stdin` type shortcut. It also added `runtime.exitCode` in `outputEval`, explicit `required` on secondaryFiles, and expression-computed `position`. [Source](https://www.commonwl.org/v1.1/CommandLineTool.html#Changelog)
- v1.2 CommandLineTool changelog:
  - Fractional `coresMin`/`coresMax` and floating-point RAM/tmpdir/outdir sizes in `ResourceRequirement`.
  - Escaping rules for string interpolation.
  - Absolute `entryname` paths in `InitialWorkDirRequirement` when a container is mandatory (used together with `DockerRequirement.dockerOutputDirectory`).
  - Non-File values returned into `Dirent.entry` are JSON-serialized.
  [Source](https://www.commonwl.org/v1.2/CommandLineTool.html#Changelog)
- v1.2.1 changes are clarifications plus new tests:
  - IANA media type `application/cwl` (plus `+json` and `+yaml`).
  - An unofficial JSON Schema for CWL.
  - Clarified `null` and `.length` in parameter references.
  - `ExpressionTool` outputs "are never type-checked due to a long-standing bug in the CWL reference implementation", with a plan to fix this in v1.3.
  [Source](https://www.commonwl.org/v1.2/Workflow.html#Changelog)
- Requirements vs hints: if an implementation cannot satisfy a requirement, or does not recognize one, "it is a fatal error and the implementation must not attempt to run the process". Unsatisfied hints are not errors. Workflow requirements are inherited by steps. The most specific level wins, and requirements override hints. [Source](https://www.commonwl.org/v1.2/CommandLineTool.html#Requirements_and_hints)
- Only these requirements are valid to inherit into a CommandLineTool: `InlineJavascriptRequirement`, `SchemaDefRequirement`, `DockerRequirement`, `SoftwareRequirement`, `InitialWorkDirRequirement`, `EnvVarRequirement`, `ShellCommandRequirement`, `ResourceRequirement`, `LoadListingRequirement`, `WorkReuse`, `NetworkAccess`, `InplaceUpdateRequirement`, `ToolTimeLimit`. [Source](https://www.commonwl.org/v1.2/CommandLineTool.html#Requirements_and_hints)
- `loadContents` reads at most 64 KiB of a UTF-8 file into `contents`. Larger files must cause an error. [Source](https://github.com/common-workflow-language/cwl-v1.2/blob/main/Process.yml)
- `streamable: true` on a File "indicates that the file is read or written sequentially without seeking". An implementation may use named pipes. [Source](https://github.com/common-workflow-language/cwl-v1.2/blob/main/Process.yml)
- `InplaceUpdateRequirement` lets tools modify `writable: true` InitialWorkDir files in place. The implementation "must ensure that only one workflow step may access a writable file at a time". [Source](https://github.com/common-workflow-language/cwl-v1.2/blob/main/CommandLineTool.yml)
- cwltool upgrades v1.0 documents on load. `v1_0to1_1` injects `NetworkAccess: true` and `LoadListingRequirement: deep_listing`, because v1.0 semantics allowed network use and loaded deep listings. [Source](https://github.com/common-workflow-language/cwltool/blob/main/cwltool/update.py)
- Reference paper: Crusoe et al., "Methods Included: Standardizing Computational Reuse and Portability with the Common Workflow Language", Communications of the ACM 65(6):54-63, June 2022. [Source](https://doi.org/10.1145/3486897), [Source](https://arxiv.org/abs/2105.07028)

### Inferences
- iunctus should target v1.2.1 as its internal CWL dialect. It should load v1.0 and v1.1 by upgrading documents on load, as cwltool does. v1.0 documents need the upgrade semantics (network on, deep listing), not just a parse.
- v1.3 loops can wait. v1.3 is not released, and 18 of its 415 tests are loop tests. The iunctus intermediate representation should still allow a step that repeats until a condition holds, because v1.3 will need it.
- `Operation` needs only parsing and validation. A runner must refuse to execute it.

### Gaps
- I found no announced release date or roadmap for v1.3.0. I did not search the CWL Discourse forum in depth.

## 2. What Schema Salad preprocessing is required, and how hard is it in Rust?

### Takeaway
A CWL document is not plain YAML. It must go through Schema Salad preprocessing before its meaning is clear. The steps are imports and includes, a base IRI, namespaces, `$graph`, identifier and link resolution, vocabulary mapping, map-to-list conversion, and two small type DSLs. The parts CWL uses are well specified and can be done in Rust in a few thousand lines. No Rust code generator for Schema Salad has been merged. The only public attempt is a stalled draft pull request.

### Cited Findings
- Salad documents use the JSON-compatible subset of YAML 1.2. Tags (`!`, `!!`), anchors and aliases (`&`, `*`), and `%YAML`/`%TAG` directives "must not be used". [Source](https://www.commonwl.org/v1.2/SchemaSalad.html#Syntax)
- Document context directives:
  - `$base` overrides the base IRI.
  - `$namespaces` maps prefixes to IRIs.
  - `$schemas` lists RDF documents (RDF-XML) "which will be queried for RDF schema data".
  - Other `$` directives must be ignored.
  - `$graph` holds the primary content. A root-level array is an implicit graph.
  [Source](https://www.commonwl.org/v1.2/SchemaSalad.html#Document_context)
- `$import` replaces the node with the loaded document. If the import is inside an array and returns an array, the result is flattened into the parent array (clarified in Salad v1.2.1). `$include` inserts a file as a string. [Source](https://www.commonwl.org/v1.2/SchemaSalad.html#Import)
- Record field annotations drive preprocessing. `jsonldPredicate: "@id"` marks an identifier field. `_type: "@id"` marks a link field (with `identity: true`, identifier resolution is used, otherwise link resolution). `_type: "@vocab"` means vocabulary resolution. [Source](https://www.commonwl.org/v1.2/SchemaSalad.html#Record_field_annotations)
- Identifier maps: fields with `mapSubject` (and optional `mapPredicate`) accept a JSON object. It is turned into an array, with each key assigned to the `mapSubject` field. This is why `inputs: {x: File}` and `inputs: [{id: x, type: File}]` mean the same. [Source](https://www.commonwl.org/v1.2/SchemaSalad.html#Identifier_maps)
- Type DSL (`typeDSL: true`): a trailing `?` makes a union with `null`. A trailing `[]` makes an array. `[]?` means an optional array. Identifier resolution happens after the DSL expansion. [Source](https://www.commonwl.org/v1.2/SchemaSalad.html#Domain_Specific_Language_for_types)
- Secondary-files DSL (`secondaryFilesDSL: true`): a string becomes `{pattern, required}`. A trailing `?` sets `required: false`. [Source](https://www.commonwl.org/v1.2/SchemaSalad.html#Domain_Specific_Language_for_secondary_files)
- Salad version history: v1.1 added field defaults, subscoped fields, `inVocab` and the secondaryFiles DSL, and removed `$mixin`. v1.2 lets inherited records narrow field types. v1.2.1 only clarifies. [Source](https://www.commonwl.org/v1.2/SchemaSalad.html)
- Packed documents: a `$graph` document's processes each need an `id`. `run` refers to them by id. Without a fragment in the reference, the runner must pick the top-level process, or `#main` in a `$graph` document, else error. `cwlVersion` below the top level must be ignored. [Source](https://www.commonwl.org/v1.2/CommandLineTool.html#Packed_documents)
- The reference `schema_salad` Python package is actively released. The latest release is 8.10.20260825112551 (2026-08-26). [Source](https://github.com/common-workflow-language/schema_salad/releases)
- Its code generator targets are `python`, `java`, `typescript`, `dotnet`, `cpp` and `dlang` (from the `--codegen` help text). Rust is not among them. [Source](https://github.com/common-workflow-language/schema_salad/blob/main/src/schema_salad/main.py)
- The Rust backend is PR #917 "[WIP] Rust language backend support" by giuseppe998e:
  - Opened 2025-03-05. Still an open draft (+3601 lines). Last updated 2026-06-28.
  - Its checklist: code generation done. Deserialization, serialization and tests not done.
  - The maintainer asked "is there anything I can help with?" on 2026-01-06.
  - The author's fork was last pushed 2025-08-16.
  [Source](https://github.com/common-workflow-language/schema_salad/pull/917), [Source](https://github.com/giuseppe998e/schema_salad)
- Other language bindings exist: a D validator (tom-tan/schema-salad-d, pushed 2024-10) and TypeScript auto bindings (2022). [Source](https://github.com/tom-tan/schema-salad-d), [Source](https://github.com/common-workflow-lab/schema-salad-ts-auto)
- cwl-utils (Python) holds parsers generated by schema-salad for v1.0, v1.1 and v1.2 (`parser/cwl_v1_2.py` and others). Latest release v0.45 on 2026-09-26. [Source](https://github.com/common-workflow-language/cwl-utils)
- Size data point (my count from the cloned repo): schema_salad's core modules are `ref_resolver.py` 1212 lines, `schema.py` 824, `validate.py` 492, `jsonld_context.py` 264 and `sourceline.py` 295. That is about 3,100 lines of Python. [Source](https://github.com/common-workflow-language/schema_salad/tree/main/src/schema_salad)
- An unofficial JSON Schema for CWL v1.2 exists in the cwl-v1.2 repo (`json-schema/`). It "captures much, but not all" of CWL. [Source](https://www.commonwl.org/v1.2/Workflow.html#Changelog), [Source](https://github.com/common-workflow-language/cwl-v1.2/tree/main/json-schema)
- fairagro/commonwl does not use a generic Salad engine. Its loader checks `if contents.contains("$graph")` to choose packed or plain parsing. Its `cwl_salad` crate (358 lines) provides serde helpers for map-to-list, the type DSL, the secondaryFiles DSL and shorthand forms, plus a derive macro. [Source](https://github.com/fairagro/commonwl/blob/main/crates/core/load.rs), [Source](https://github.com/fairagro/commonwl/tree/main/crates/salad)

### Inferences
- Two routes are realistic in Rust:
  1. Hand-written serde types for CWL v1.2, plus a small preprocessor for the Schema Salad subset that CWL uses. It would cover `$import`, `$include`, `$namespaces`, `$base`, `$graph`, map-to-list, the type DSL, the secondaryFiles DSL, and IRI resolution of `id`, `source`, `outputSource`, `run` and `scatter`. This is what commonwl does, and it passes 99% of the tests.
  2. A generic Schema Salad engine that reads the official CWL schema YAML. This is more work, but it would make v1.3 and extensions cheaper.
  Route 1 is the lower risk for iunctus. It is roughly 2,000 to 4,000 lines of Rust.
- The hard part of Salad is not YAML. It is correct IRI resolution: relative ids, `#` fragments, step-scoped ids like `#main/step/out`, and ids in embedded or imported documents. Wrong ids break `source` links in packed workflows.
- A YAML parser that rejects anchors, tags and aliases (or at least warns) is needed for strict conformance. The `serde-saphyr` crate used by commonwl is one option (inference, not tested here).
- `$schemas` (RDF-XML ontologies such as EDAM) is only needed for format checking. It is optional (4 tests tagged `format_checking` in v1.2). A Rust RDF parser crate would be needed if it is implemented. I did not evaluate which one.

### Gaps
- I did not measure how complete PR #917's generated Rust types are. The PR states that deserialization is missing, so it is not usable today.
- I did not check whether the Salad rules for `refScope` and `subscope` are fully exercised by the CWL tests.

## 3. How do expressions work, and which Rust JavaScript engines fit?

### Takeaway
CWL has two expression levels. Parameter references `$(inputs.x.path)` are required, and need no JavaScript engine. Full JavaScript (`$(...)` expressions and `${...}` function bodies, ECMAScript 5.1) is optional. It is enabled by `InlineJavascriptRequirement` and can include `expressionLib` code. cwltool runs JavaScript in a separate Node.js process. In Rust, Boa (pure Rust) or rquickjs (QuickJS-NG, C) are good embedded choices. V8 through deno_core is the most compatible but by far the heaviest.

### Cited Findings
- Parameter references use a small grammar: a symbol, then `.symbol`, `['str']`, `["str"]` or `[index]` segments. They are "designed to not require a Javascript engine". `null` and `.length` on arrays have special rules. The context holds `inputs`, `self` and `runtime`. [Source](https://www.commonwl.org/v1.2/CommandLineTool.html#Parameter_references)
- String interpolation: if a reference has leading or trailing text, the value is turned into a string. Strings are inserted as literal text. Other values are inserted as JSON, and "object entries are sorted by key". Escapes: `\$(`, `\${` and `\\`. [Source](https://www.commonwl.org/v1.2/CommandLineTool.html#String_interpolation)
- JavaScript expressions are "Optional" in the spec. They need `InlineJavascriptRequirement`.
  - `$(...)` is evaluated as an ECMAScript 5.1 expression.
  - `${...}` is evaluated as `(function() { ... })()`.
  - The result must be JSON data, else `permanentFailure`.
  - `expressionLib` code runs first.
  - `inputs`, `self` and `runtime` are global variables.
  - Scanners must handle nested brackets and brackets inside strings.
  [Source](https://www.commonwl.org/v1.2/CommandLineTool.html#Expressions_(Optional))
- cwltool's JavaScript sandbox (now in cwl-utils `sandboxjs.py`):
  - It starts a `nodejs` or `node` process. The minimum version is 0.10.26.
  - If Node is missing, it falls back to a `node:alpine` container (docker, podman, udocker or singularity).
  - The helper script evaluates each expression with `require("vm").runInNewContext(fn, {})` and keeps the process alive between calls.
  - Expressions are wrapped as `"use strict"; <jslib> (function(){...})()`.
  [Source](https://github.com/common-workflow-language/cwl-utils/blob/main/src/cwl_utils/sandboxjs.py), [Source](https://github.com/common-workflow-language/cwl-utils/blob/main/src/cwl_utils/cwlNodeEngine.js)
- cwltool's `--eval-timeout` is "Time to wait for a Javascript expression to evaluate before giving an error, default 60s". The library default in `sandboxjs.py` is 20 s. [Source](https://github.com/common-workflow-language/cwltool/blob/main/cwltool/argparser.py), [Source](https://github.com/common-workflow-language/cwl-utils/blob/main/src/cwl_utils/sandboxjs.py)
- Boa (`boa_engine`), a pure Rust engine:
  - v0.22 was released on 2026-08-28. Its "conformance has grown to 95.60% in the official ECMAScript Test Suite (Test262)".
  - The crate has 5.09 million downloads.
  [Source](https://github.com/boa-dev/boa/releases/tag/v0.22), [Source](https://crates.io/crates/boa_engine)
- rquickjs 0.14.0 (2026-09-18) binds QuickJS-NG, a maintained fork of Bellard's QuickJS. The README claims "almost complete ES2020 support" and "nearly 100%" of Test262 for ES2020 features. [Source](https://github.com/DelSkayn/rquickjs), [Source](https://github.com/quickjs-ng/quickjs)
- V8 through Rust:
  - The `v8` crate (rusty_v8) v152.2.0 is from 2026-08-20. deno_core 0.412.0 is from 2026-09-16.
  - V8 is "over 600,000 lines of C++" and "often takes 30 minutes to compile". By default `cargo build` downloads a prebuilt static library.
  - For x86_64 Linux release builds that download is about 37 MiB gzipped.
  [Source](https://github.com/denoland/rusty_v8), [Source](https://github.com/denoland/rusty_v8/releases/tag/v152.2.0), [Source](https://crates.io/crates/deno_core)
- fairagro/commonwl uses `boa_engine` 0.21.1 for CWL expressions and passes all 134 JavaScript-tagged v1.2 tests on its local backend. Its only local failures are 2 number-precision tests. [Source](https://github.com/fairagro/commonwl/blob/main/Cargo.toml), [Source](https://github.com/fairagro/commonwl/actions/runs/35203037834)
- In the v1.2.1 suite (my count), 134 of 378 tests are tagged `inline_javascript`. None of the 84 `required` tests use JavaScript. [Source](https://github.com/common-workflow-language/cwl-v1.2/blob/main/conformance_tests.yaml)

### Inferences
- ES5.1 is a small target. Boa and QuickJS-NG both implement far newer editions, so ES5.1 syntax is covered. The real compatibility risk is behavior that Node has and the engine does not, used by real-world `expressionLib` code (for example `JSON`, `RegExp` details, `Array.prototype` methods, sorting stability, number formatting). commonwl's result suggests Boa is good enough for the conformance suite.
- Suggested design: put a parameter-reference evaluator in pure Rust (no engine). Call a JavaScript engine only when `InlineJavascriptRequirement` is present. Hide the engine behind a trait, so Boa can be the default and QuickJS or V8 can be added later.
- Sandboxing: use a fresh context per evaluation, or a cached context per `expressionLib` that is reset. Add an instruction or time budget (a 60 s default like cwltool). Give no file, network or process APIs. Boa and QuickJS have no built-in I/O, which helps.
- Numbers: CWL values are JSON. Expected outputs include very large integers (for example 4.2e42). A Rust implementation should keep numbers exact through the pipeline, for example with `serde_json` `arbitrary_precision` or its own number type. commonwl loses 2 tests because "serde_json stores all numbers as f64" (their words).

### Gaps
- I found no published head-to-head test of Boa vs QuickJS-NG on CWL `expressionLib` code in the wild. Measuring this on test workflows (for example WorkflowHub or Dockstore CWL files) is a good follow-up.
- I did not find current Test262 numbers for QuickJS-NG in a primary source. The rquickjs README text is about QuickJS in general.

## 4. How does conformance testing work, and who passes how much?

### Takeaway
Conformance is measured with `cwltest`, which runs any runner that follows the `cwl-runner` command-line convention against YAML test lists. v1.2.1 has 378 tests, 84 of them tagged `required`. cwltool, Toil, StreamFlow, Arvados and Calrissian report 100% of required tests. The Rust library fairagro/commonwl passes 375 or 376 of the 378 v1.2 tests on its local, Docker and TES backends.

### Cited Findings
- Test counts (my counts from the files):

  | Suite | Total tests | `required` | `inline_javascript` | `should_fail` |
  |---|---|---|---|---|
  | v1.0 (common-workflow-language main) | 197 | 49 | n/a | n/a |
  | v1.1 | 253 | 59 | 91 | 18 |
  | v1.2.0 tag | 336 | 65 | 117 | n/a |
  | v1.2.1 tag | 378 | 83 | 134 | n/a |
  | v1.2 main (2026-09-20) | 378 | 84 | 134 | 41 |
  | v1.3.0-dev1 main | 415 | 102 | 155 | 47 |

  Sources: [v1.0](https://github.com/common-workflow-language/common-workflow-language/blob/main/v1.0/conformance_test_v1.0.yaml), [v1.1](https://github.com/common-workflow-language/cwl-v1.1/blob/main/conformance_tests.yaml), [v1.2](https://github.com/common-workflow-language/cwl-v1.2/blob/main/conformance_tests.yaml), [v1.3](https://github.com/common-workflow-language/cwl-v1.3/blob/main/conformance_tests.yaml)
- v1.2 main tag counts:
  - Process classes: `command_line_tool` 195, `workflow` 173, `expression_tool` 36.
  - Most common feature tags: `inline_javascript` 134, `conditional` 46, `scatter` 42, `initial_work_dir` 38, `multiple_input` 35, `inputs_should_parse` 33, `shell_command` 23, `step_input` 20, `subworkflow` 17.
  - Smaller feature tags: `docker` 11, `resource` 11, `timelimit` 9, `schema_def` 7, `work_reuse` 5, `env_var` 4, `format_checking` 4, `load_listing` 3, `networkaccess` 2, `inplace_update` 2, `secondary_files` 2.
  The v1.3 draft adds `loop` (18). [Source](https://github.com/common-workflow-language/cwl-v1.2/blob/main/conformance_tests.yaml)
- Expected outputs in the v1.2 test files contain about 211 `sha1$` checksums. `location: Any` is used as a wildcard. So a runner must report correct `size` and `checksum` for output Files. [Source](https://github.com/common-workflow-language/cwl-v1.2/blob/main/conformance_tests.yaml)
- `cwltest` usage: `cwltest --test conformance_tests.yaml --tool <runner>`.
  - Options include `-n` (test numbers), `--tags`, `--exclude-tags`, `--only-tools`, `--junit-xml` and `--badgedir`.
  - `--badgedir` writes badgen.net JSON per tag, for example `{"subject": "command_line_tool", "status": "100%"}`.
  - Latest release: 2.7.20260814150058 (2026-08-14).
  [Source](https://github.com/common-workflow-language/cwltest), [Source](https://github.com/common-workflow-language/cwltest/blob/main/src/cwltest/argparser.py), [Source](https://github.com/common-workflow-language/cwltest/releases)
- Exit code 33 means "unsupported feature" (`UNSUPPORTED_FEATURE = 33` in cwltest). Such tests are counted apart from failures. [Source](https://github.com/common-workflow-language/cwltest/blob/main/src/cwltest/__init__.py)
- The spec repos document how to run the suite with any runner in `CONFORMANCE_TESTS.md`. [Source](https://github.com/common-workflow-language/cwl-v1.2/blob/main/CONFORMANCE_TESTS.md)
- The CWL website states that the community "does not yet test or certify" implementations. Badges are self-reported. [Source](https://github.com/common-workflow-language/cwl-website/blob/main/content/implementations.md)
- Badge values fetched on 2026-09-27:
  - Toil v1.2: required 100%, all 100%. [Source](https://ucsc-ci.com/api/v4/projects/3/jobs/artifacts/master/raw/badges1.2/required.json?job=cwl_badge&search_recent_successful_pipelines=true)
  - StreamFlow v1.2: required 100%. [Source](https://streamflow.di.unito.it/cwl-conformance/v1.2/required.json)
  - Arvados: required 100%. [Source](https://jutro-4zz18-ytx3kamvs9vo8q5.collections.jutro.arvadosapi.com/required.json)
  - Calrissian: required 100%. The badge path says "1.0.2", so the CWL version it covers is unclear. [Source](https://raw.githubusercontent.com/Duke-GCB/calrissian/master/conformance/badges/1.0.2/required.json)
  - REANA v1.0: required 94%. [Source](https://raw.githubusercontent.com/reanahub/reana-workflow-engine-cwl/master/badges/v1.0/required.json)
  - Xenon-flow: required 80%. [Source](https://raw.githubusercontent.com/xenon-middleware/xenon-flow/gh-pages/badges/required.json)
- Toil's docs say its badges "describe the current development version of Toil", and that `--bypass-file-store` is needed to fully support `inplace_update`. [Source](https://toil.readthedocs.io/en/latest/cwl/conformance.html)
- fairagro/commonwl's CI run of 2026-09-17 (cwltest against cwl-v1.2, `-j 3`):
  - local: "376 tests passed, 2 failures, 0 unsupported features".
  - docker: "375 tests passed, 3 failures".
  - tes (Funnel plus S3-compatible storage): "375 tests passed, 3 failures".
  - The failures are `paramref_arguments_roundtrip` and `paramref_arguments_inputs` (large numbers). Docker and TES also fail `networkaccess2`.
  [Source](https://github.com/fairagro/commonwl/actions/runs/35203037834)

### Inferences
- A runner that skips JavaScript can still pass all `required` tests. It could pass about 244 of 378 tests (the ones without the JavaScript tag). Real-world CWL uses JavaScript often, though, so JavaScript is needed for practical use.
- iunctus should wire `cwltest` into CI from the first week. It should publish per-tag badges for each backend (local, TES), as commonwl does.
- `cwltest` needs Python. That is fine for CI. It does not break the "no shelling out to engines" rule, because cwltest is a test harness, not an engine.

### Gaps
- I could not read Arvados, StreamFlow or Calrissian "all tests" percentages. Only their `required` badges were reachable.
- I did not find an official cwltool badge for v1.2. The old `common-workflow-language/conformance` repo was last pushed in 2020.

## 5. What do the existing (non-Rust) implementations look like?

### Takeaway
cwltool (Python) is the reference implementation, and most Python engines (Toil, Calrissian, REANA, cwl-tes, Galaxy's work) reuse its loader and expression code. Only a few engines are independent: Arvados (Go orchestration, but it also uses cwltool code), StreamFlow (own Python engine using cwl-utils), and the Seven Bridges Rabix executor (Java, v1.0, now legacy).

### Cited Findings
- cwltool is the reference runner for v1.0 to v1.2. It runs on Linux, macOS and Windows, locally only. Latest release 3.3.20260925135507 (2026-09-25). [Source](https://www.commonwl.org/implementations/), [Source](https://github.com/common-workflow-language/cwltool/releases)
- cwltool size (my count): about 17,600 lines of Python in `cwltool/`. `command_line_tool.py` has 1579 lines, `process.py` 1428, `job.py` 1096, `workflow_job.py` 1062 and `builder.py` 695. It depends on schema_salad and cwl-utils on top of that. [Source](https://github.com/common-workflow-language/cwltool)
- Production implementations listed on commonwl.org:
  - cwltool.
  - Arvados (AWS, Azure, Slurm, LSF).
  - Toil (v1.0 to v1.2 "and all optional features"; AWS, GCP, Grid Engine, HTCondor, LSF, Slurm, PBS/Torque).
  - StreamFlow (v1.0 to v1.2, "nearly all optional features"; Kubernetes, HPC, SSH).
  - Calrissian (Kubernetes).
  - CWL-Airflow (v1.0 to v1.1).
  - shaft (CommandLineTool only, v1.0).
  Partial implementations: ep3, REANA (v1.0), Xenon, Galaxy, cwl-tes, Pegasus (import under development) and Weaver.
  [Source](https://www.commonwl.org/implementations/)
- Activity on 2026-09-27:
  - Toil: last push 2026-09-26, release 9.5.0 on 2026-06-02.
  - StreamFlow: 0.2.0rc3 on 2026-09-19.
  - Calrissian: 0.18.1 on 2024-12-13, last push 2026-02-04.
  - CWL-Airflow: last release and push 2023-10-25.
  - REANA CWL engine: 0.9.4 on 2024-11-29, last push 2026-09-22.
  - rabix/bunny (Seven Bridges Java executor): last release 2018-03-22, pushed 2020.
  [Source](https://github.com/DataBiosphere/toil), [Source](https://github.com/alpha-unito/streamflow), [Source](https://github.com/Duke-GCB/calrissian), [Source](https://github.com/Barski-lab/cwl-airflow), [Source](https://github.com/reanahub/reana-workflow-engine-cwl), [Source](https://github.com/rabix/bunny)
- `toil-cwl-runner` "provides CWL parsing functionality using cwltool". [Source](https://toil.readthedocs.io/en/master/cwl/running.html)
- The Seven Bridges Platform (Velsera) docs state: "The Seven Bridges Platform supports the following versions of CWL: sbg:draft-2, v1.0, v1.1, v1.2". They also say "Not all CWL v1.0 features are currently supported". [Source](https://docs.sevenbridges.com/docs/about-the-common-workflow-language)
- Galaxy: CWL support is listed as partial. Recent Galaxy work converts CWL into Galaxy's own gxformat2 workflows, rather than running CWL natively (from search results, not checked in depth). [Source](https://www.commonwl.org/implementations/), [Source](https://github.com/galaxyproject/foundry/pull/600)
- The website's tools page lists `cwl-rs` ("CWL object model for Rust") as a tool. It does not list any Rust runner. [Source](https://github.com/common-workflow-language/cwl-website/blob/main/content/tools.md)

### Inferences
- Because most engines reuse cwltool, "passes 100% required" mostly measures the cwltool core. An independent reimplementation (like iunctus) will hit edge cases that the other engines never re-derived. The conformance suite is the main safety net.
- cwltool is the behavioral oracle. When the spec is unclear, iunctus should match cwltool's output.

### Gaps
- I did not verify Arvados' or StreamFlow's internals in code for this note.

## 6. Which Rust CWL projects exist, and can iunctus reuse them?

### Takeaway
There is one serious Rust CWL implementation: FAIRagro's `commonwl` workspace (crates `commonwl`, `cwl_core`, `cwl_engine`, `cwl_engine_storage`, `cwl_salad`). It parses and executes CWL v1.2 with local, Docker and GA4GH TES backends at 99% conformance. It is dual-licensed MIT OR Apache-2.0. Its main risks are a single main developer and an unstable 0.x API. Everything else in Rust is archived, tiny, or not about CWL.

### Cited Findings
- crates.io search for "cwl" (2026-09-27): the relevant crates are all from FAIRagro:
  - `commonwl`, `cwl_core`, `cwl_engine`, `cwl_engine_storage`, `cwl_salad`, `cwl_salad_derive` and `cwl-lsp`: all 0.15.1, updated 2026-09-09. `cwl_core` has about 2,350 downloads.
  - `sciwin` 2.0.1.
  - `reana` and `reana-auth` 0.7.2.
  No crate is named `cwl`. [Source](https://crates.io/search?q=cwl), [Source](https://crates.io/crates/commonwl)
- fairagro/commonwl repo facts:
  - Created 2025-12-17. Last push 2026-09-17. 2 stars.
  - Contributors: JensKrumsieck with 643 commits, plus dependabot.
  - Rust edition 2024, MSRV 1.95.
  [Source](https://github.com/fairagro/commonwl)
- Its README: "A Rust-framework for the Common Workflow Language (CWL) that supports parsing and execution". It covers `CommandLineTool`, `Workflow`, `ExpressionTool` and `Operation`, and is "based on the headless task execution framework crankshaft". Features: none by default for parsing only, `engine` for execution, `tes` for the TES backend. Slurm is "planned". [Source](https://github.com/fairagro/commonwl)
- Storage backends: local filesystem, S3-compatible, and HTTP(S) read-only. Staging "works the same across all task backends". [Source](https://github.com/fairagro/commonwl)
- Size (my count of `.rs` lines): `core` 6,252, `engine` 11,789, `storage` 1,088, `lsp` 853, `salad` 358, `salad_derive` 72, `conformance` 116. About 20,500 lines in total. [Source](https://github.com/fairagro/commonwl/tree/main/crates)
- Engine layout: `backend/` (local, docker, tes), `environment/`, `io/` (file, directory and JSON staging), `schema/`, `command.rs`, `expression.rs` (Boa), `scatter.rs` and `workflow.rs`. [Source](https://github.com/fairagro/commonwl/blob/main/CLAUDE.md)
- The test suite is a git submodule of cwl-v1.2. The engine defaults to `cwlVersion` "v1.2". I found no v1.3 loop support and no v1.0/v1.1 upgrade code in the source. [Source](https://github.com/fairagro/commonwl/blob/main/.gitmodules), [Source](https://github.com/fairagro/commonwl/blob/main/crates/engine/lib.rs)
- Changelog 0.9.0: "TES backend conformance raised from 24% to 99%". Fixes included storage-aware secondaryFiles, File metadata computed after values become remote, and not wiring TES `stdin`, "which some TES servers (e.g. Funnel) truncate to zero bytes". Later releases added step events (0.15.0) and configurable S3 (0.15.0). [Source](https://github.com/fairagro/commonwl/blob/main/CHANGELOG.md)
- SciWIn is FAIRagro's user tool (binary `s4n`, "SciWIn-Client"). Its 2.x line (v2.0.0 on 2026-09-08, v2.0.1 on 2026-09-09) uses `commonwl` for "local, Docker and TES backends used by `s4n execute`". It is funded by DFG project 501899475. [Source](https://github.com/fairagro/sciwin), [Source](https://github.com/fairagro/commonwl)
- onnovalkering/cwl-rs, "The Common Workflow Language (CWL) object model for Rust": MIT, created 2020-02-27, last push 2021-08-19, archived. [Source](https://github.com/onnovalkering/cwl-rs)
- inutano/cwl-zen, "A minimal, JS-free subset of CWL v1.2", is in Rust. Created 2026-03-28, last push 2026-04-04, 0 stars, no license file. Its README claims "A minimal CWL runner without JS support could be ~2,000 lines of code" (the author's opinion, not verified). [Source](https://github.com/inutano/cwl-zen)
- crankshaft (St. Jude Rust Labs, Apache-2.0) is the task-execution library used by commonwl for Docker and TES. Release crankshaft-v0.11.0 on 2026-08-26. [Source](https://github.com/stjude-rust-labs/crankshaft)

### Inferences
- Reuse options for iunctus, from least to most coupling:
  1. Use `commonwl` as a reference and an oracle only. Read its code and compare outputs.
  2. Depend on `cwl_core` (parsing and validation only, no engine features). Translate its document model into the iunctus intermediate representation. The licence allows this.
  3. Depend on `cwl_engine` for CommandLineTool preparation and output collection. Let iunctus schedule workflows and call TES. This is harder, because commonwl's engine owns scheduling and staging through crankshaft and its own storage layer.
- Option 2, plus selective reuse or porting of logic (secondaryFiles, glob, InitialWorkDir, expression wiring), looks like the best trade-off. Full dependence on `cwl_engine` would tie iunctus to one maintainer's design and its 0.x churn: 15 minor releases in under a year, and a new 0.15 API on 2026-09-09.
- Bus factor is the main reuse risk. Almost all commits come from one person, and the project is grant funded.
- Before deciding, ask FAIRagro directly. Upstream collaboration (for example on v1.0/v1.1 upgrade and exact numbers) may be cheaper than a fork.

### Gaps
- I did not build `commonwl` or run its tests locally. The conformance numbers come from its CI logs.
- I did not check `commonwl`'s public API stability, docs coverage, or how easy it is to separate the parser from its engine types.

## 7. How do CWL runners map to GA4GH TES?

### Takeaway
TES runs one container task with declared input files, output files and resources. It knows nothing about CWL types. The CWL engine must do everything else outside TES: build the command line, compute secondaryFiles, lay out InitialWorkDir, and after the task, glob outputs, read `cwl.output.json`, compute checksums and apply `outputEval`. Both known CWL-on-TES engines (cwl-tes, and commonwl's TES backend) upload the whole working directory to object storage and collect outputs from there.

### Cited Findings
- TES spec releases: v1.0 on 2021-02-12, v1.1 on 2022-11-15. The OpenAPI `info.version` is 1.1.0. [Source](https://github.com/ga4gh/task-execution-schemas/releases)
- TES 1.1 inputs:
  - Each input has `url` or literal `content`, an absolute container `path`, and `type` FILE or DIRECTORY.
  - Implementations "should support a minimum of 128 KiB" of `content`.
  - A `streamable` flag hints that a streaming mount (for example FUSE on S3) may be used.
  Outputs:
  - `path` "May contain pattern matching wildcards", but only POSIX shell wildcards.
  - With wildcards, `path_prefix` is required and `url` must be a directory.
  [Source](https://github.com/ga4gh/task-execution-schemas/blob/develop/openapi/task_execution_service.openapi.yaml)
- TES 1.1 resources: `cpu_cores`, `preemptible`, `ram_gb`, `disk_gb`, `zones` and `backend_parameters`. I found no field for network isolation and no concept of a writable input in the OpenAPI file. [Source](https://github.com/ga4gh/task-execution-schemas/blob/develop/openapi/task_execution_service.openapi.yaml)
- cwl-tes (OHSU):
  - "CWL engine backended by the GA4GH Task Execution API", built on cwltool.
  - Archived. Last commit 2022-11-10. Last release 0.3.0 on 2019-11-25.
  - The TES paper calls it "an experimental workflow engine based on the CWL reference engine".
  [Source](https://github.com/ohsu-comp-bio/cwl-tes), [Source](https://arxiv.org/abs/2405.00013)
- cwl-tes task mapping (from `cwl_tes/tes.py`):
  - Every primary input File and each of its `secondaryFiles` becomes a TES input.
  - InitialWorkDir listing entries become inputs. Writable entries raise "The TES spec does not allow for writable inputs".
  - Outputs are `stdout`, `stderr` and the whole output dir as one `DIRECTORY` output named `workdir` (plus `dockerOutputDirectory` if set).
  - After the task, outputs are collected from remote storage, including a remote `cwl.output.json`.
  - Resources map to `cpu_cores`, `ram_gb` and `disk_gb`. The CWL document id is sent as a tag.
  [Source](https://github.com/ohsu-comp-bio/cwl-tes/blob/master/cwl_tes/tes.py)
- Toil and TES:
  - The TES batch system "was removed from Toil after version 5.12". It now lives in a separate plugin (`toil-batch-system-tes`, last push 2026-08-26).
  - The plugin notes you "might also need to use the AWS job store, or another job store accessible over the network".
  - So Toil runs its own worker inside TES tasks and moves files through its job store. It does not map CWL files to TES inputs one by one.
  [Source](https://github.com/adamnovak/toil_batch_system_tes)
- Funnel is a TES server, not a CWL engine. The project now lives at `calypr/funnel` (Go, last push 2026-09-02). [Source](https://github.com/calypr/funnel)
- commonwl TES backend (`crates/engine/backend/tes.rs`, 350 lines, built on crankshaft):
  - It uses fixed container paths `/mnt/task/workdir` and `/mnt/task/tmp`.
  - Workdir and tmpdir are uploaded as `Directory` outputs. stdout and stderr are `File` outputs.
  - It does not use TES `stdin`.
  - It adds a second executor that runs `find . -type d -empty -exec touch {}/<marker>`, so empty directories survive S3.
  - Its CI tests TES against Funnel plus rustfs (S3-compatible).
  [Source](https://github.com/fairagro/commonwl/blob/main/crates/engine/backend/tes.rs), [Source](https://github.com/fairagro/commonwl)
- commonwl notes: "Funnel has an upstream crash under concurrent load". Their dev script has a watchdog that restarts it. [Source](https://github.com/fairagro/commonwl)

### Inferences
- Recommended mapping for iunctus (inference, based on the two engines above):
  - Inputs: one TES input per File (primary and secondary). Use a DIRECTORY input per Directory. Use `content` for small literal files and InitialWorkDir text entries. Upload large literals to storage first, because CWL puts no limit on literal size while TES only guarantees 128 KiB.
  - Paths: choose fixed container paths for `runtime.outdir` and `runtime.tmpdir` before building the command line. CWL expressions can read these values.
  - Outputs: upload the whole outdir as one DIRECTORY output. Then run glob, `cwl.output.json`, secondaryFiles discovery, `loadContents` and `outputEval` in the engine against object storage. TES 1.1 wildcards are too weak for CWL globs, which can come from expressions and must match directories too.
  - Metadata: compute `size` and `sha1` checksums after upload. Either download the files, or add a small extra executor in the same task that writes a manifest (size, checksum, file type, symlinks, empty dirs). The manifest route saves a round trip. commonwl already uses an extra executor for empty directories.
  - stdin: materialize stdin as an input file and redirect inside the command (for example `sh -c` or an argument), because of the Funnel truncation issue.
  - Writable InitialWorkDir and `InplaceUpdateRequirement`: TES has no writable inputs. Stage copies as normal inputs. The container can write to them because they are copies. In-place update across steps is then really "copy in, upload out". That meets the semantics but not the optimization.
  - `NetworkAccess: false` cannot be enforced through standard TES. Use `backend_parameters`, or report it as a limit. commonwl fails `networkaccess2` on TES and Docker.
  - `ToolTimeLimit`: enforce in the engine by cancelling the TES task.
  - `DockerRequirement.dockerFile` (image build) and `dockerLoad` do not fit TES. commonwl rejects `dockerFile` on TES with a clear error.

### Gaps
- I did not test how various TES servers (Funnel, TESK, Microsoft TES on Azure, Pulsar) handle DIRECTORY inputs and outputs, symlinks, or empty directories. Behavior differs by server. This needs hands-on tests.
- I did not check TESK- or Azure-specific limits on `content` size.

## 8. What are the hardest parts to implement natively?

### Takeaway
The workflow graph (scatter, when, pickValue, subworkflows) is well understood and well tested. The long tail is in the File and Directory model and the CommandLineTool life cycle: secondaryFiles, InitialWorkDir, glob and output collection, Directory listings, `cwl.output.json`, type checking of unions and records, and exact JSON round-tripping. JavaScript is less hard than it looks if an embedded engine is used.

### Cited Findings
- secondaryFiles patterns:
  - A trailing `?` makes the file optional.
  - Each leading `^` removes one file extension from the primary path.
  - The rest of the string is appended.
  - Expressions may return strings, File or Directory objects, or null.
  - The v1.2 changelog added tests for the order of operations on input secondaryFiles.
  [Source](https://github.com/common-workflow-language/cwl-v1.2/blob/main/Process.yml), [Source](https://www.commonwl.org/v1.2/CommandLineTool.html#Changelog)
- `glob` uses POSIX glob(3) relative to the output directory. It may be a string, an array or an expression. It must only match existing files or directories. It must not escape the output dir. A symlink match takes the symlink's basename. [Source](https://github.com/common-workflow-language/cwl-v1.2/blob/main/CommandLineTool.yml)
- `InitialWorkDirRequirement` entries can be:
  - Files, Directories, or `Dirent` records (`entryname`, `entry`, `writable`).
  - Expressions that return arrays of these.
  - In v1.2, absolute `entryname` paths when a container is mandatory.
  - JSON-serialized non-File values.
  [Source](https://www.commonwl.org/v1.2/CommandLineTool.html#Changelog)
- Directory listings are controlled by `LoadListingRequirement`/`loadListing` (v1.1+). v1.0 behaved like deep listing, which cwltool restores on upgrade. [Source](https://www.commonwl.org/v1.1/CommandLineTool.html#Changelog), [Source](https://github.com/common-workflow-language/cwltool/blob/main/cwltool/update.py)
- Packed and `$graph` documents need id-based entry points (`#main`) and fragment references. Embedded `run` objects are recursive. [Source](https://www.commonwl.org/v1.2/CommandLineTool.html#Packed_documents)
- Test weight of these areas in v1.2 (my counts): `initial_work_dir` 38, `step_input` 20 (valueFrom and similar), `multiple_input` 35, `scatter` 42, `conditional` 46, `expression_tool` 36. 14 tests expect Directory values in outputs. 11 tests run a packed document through a `#fragment` tool reference. [Source](https://github.com/common-workflow-language/cwl-v1.2/blob/main/conformance_tests.yaml)
- commonwl's changelog shows the kind of bugs such an engine meets:
  - Path traversal through `..` in `entryname`.
  - Dangling outputs across filesystems.
  - Self-referencing workflows and `$import` loops.
  - Symlink loops in Directory inputs.
  - `scatter: []` panics.
  - 0-byte copies.
  - `dockerFile`-only requirements.
  - File metadata after values move to remote storage.
  [Source](https://github.com/fairagro/commonwl/blob/main/CHANGELOG.md)
- Numbers: two v1.2 tests expect exact large numbers (4200000000000000000000000000000000000000000, and 1e42 printed in full). An f64-only JSON model fails them. [Source](https://github.com/fairagro/commonwl/actions/runs/35203037834)
- The ExpressionTool output type check is not done even by cwltool in v1.2 (spec-acknowledged bug). So conformance does not force strict checks there. [Source](https://www.commonwl.org/v1.2/Workflow.html#Changelog)

### Inferences
Hardest parts, ranked for iunctus (my judgement):
1. **Output collection on remote storage**: glob, symlinks, Directory outputs, empty dirs, `cwl.output.json` (with relative `path`/`location`), secondaryFiles on outputs, `loadContents`, checksums. All of this happens after the TES task, against object storage.
2. **InitialWorkDir**: expression-generated listings, writable copies, entryname rules and path-traversal safety, absolute entrynames with `dockerOutputDirectory`, and passing File objects through unchanged (the `iwd-passthrough*` tests).
3. **Type system and binding**: unions (including `Any`, `null`, enums and records), input validation, default values with File/Directory literals, command-line building (`position` ordering, `prefix`, `separate`, `itemSeparator`, `shellQuote` with `ShellCommandRequirement`, nested records and arrays of arrays).
4. **IRI and id handling** in the loader: packed documents, embedded `run`, `$import`, relative references, step-scoped output ids, and `#main`.
5. **Workflow semantics**: `valueFrom` on step inputs (evaluated after `source` and `default`, with `self` set; null handling is tested by `valuefrom_ignored_null`), `linkMerge` with `pickValue`, nested scatter with `when`, and outputs of skipped steps. These are well tested but subtle.
6. **JavaScript**: embedding is easy. Exact Node-like behavior for real-world `expressionLib` code is the risk.
7. **Exact JSON**: big numbers, key order in string interpolation ("sorted by key"), and float formatting on the command line (cwltool 3.3 fixed "float/double inputs below 1e-6 truncating to 0"). [Source](https://github.com/common-workflow-language/cwltool/releases/tag/3.3.20260925135507)

- The iunctus intermediate representation must allow engine-side hooks before and after each task. A CWL CommandLineTool is not "a command string". The command line depends on runtime values (cores, RAM, outdir), and outputs are defined by post-run evaluation.

### Gaps
- I did not classify every one of the 378 tests by root difficulty. The ranking above is judgement plus tag counts plus commonwl's changelog.

## 9. What does a native Rust CWL implementation need? (checklist, effort and risk)

### Takeaway
A native Rust CWL v1.2 runner with TES is clearly feasible. It has been done: fairagro/commonwl, about 20,500 lines of Rust, 375 of 378 tests on TES. For iunctus, a from-scratch implementation is roughly 8 to 13 person-months to reach 95% or more of v1.2 on local plus TES. Reusing or porting parts of `commonwl` could cut that to roughly 3 to 6 months. Overall risk is medium. The main risks are output collection on object storage, TES server differences, and JavaScript fidelity, not the language itself.

### Cited Findings
- Existence proof: `commonwl` passes 376/378 (local), 375/378 (Docker) and 375/378 (TES on Funnel) of the v1.2 tests. [Source](https://github.com/fairagro/commonwl/actions/runs/35203037834)
- Scale reference: `commonwl` is about 20,500 lines of Rust (my count). cwltool is about 17,600 lines of Python, plus schema_salad's core of about 3,100 lines of Python. [Source](https://github.com/fairagro/commonwl), [Source](https://github.com/common-workflow-language/cwltool), [Source](https://github.com/common-workflow-language/schema_salad)
- Timeline reference: the SciWIn repo was created 2024-09-24. `commonwl` was created 2025-12-17. TES conformance went "from 24% to 99%" in release 0.9.0. The work was done mainly by one developer (643 commits). [Source](https://github.com/fairagro/sciwin), [Source](https://github.com/fairagro/commonwl), [Source](https://github.com/fairagro/commonwl/blob/main/CHANGELOG.md)

### Inferences

**Checklist (native Rust, no cwltool, no Node.js):**

Loader and model
- [ ] Strict YAML/JSON reader (JSON-compatible YAML subset; reject or warn on anchors, tags, aliases).
- [ ] Schema Salad preprocessing (the subset CWL uses): `$import` (with array flattening), `$include`, `$base`, `$namespaces`, `$graph`, map-to-list (`mapSubject`/`mapPredicate`), type DSL (`T?`, `T[]`, `T[]?`), secondaryFiles DSL, IRI resolution for identifiers and links, and vocabulary terms.
- [ ] Typed Rust model for v1.2 (four process classes, all requirements, input and output schemas, bindings, Dirent, SchemaDefRequirement type definitions).
- [ ] Version upgrade v1.0 and v1.1 to v1.2 (NetworkAccess, deep listing and similar), with cwltool's `update.py` as the guide.
- [ ] Packed documents: `#main`, fragment entry points, embedded `run`, cycle detection.
- [ ] Validation errors with source line and column. Unknown requirement means fatal. Unknown hint means warning. Extension namespaces are ignored.
- [ ] Exact JSON numbers (arbitrary precision or big-number aware).

Values and types
- [ ] Input object loading (job file), defaults, File/Directory literals (`contents`, `listing`), `location`/`path` resolution relative to the job file.
- [ ] Type checking for unions, `Any`, enums, records, arrays and optional values, with good error messages.
- [ ] File metadata: `basename`, `nameroot`, `nameext`, `dirname`, `size`, `checksum` (sha1), `format`. Format checking against `$schemas` ontologies is optional.
- [ ] `loadContents` (64 KiB limit, error when larger) and `loadListing` (no, shallow, deep).
- [ ] secondaryFiles: pattern rules (`^`, `?`), expressions, `required`, on inputs and outputs, and on Directories.

Expressions
- [ ] Parameter reference parser and evaluator in pure Rust (required).
- [ ] String interpolation with escapes and sorted-key JSON output.
- [ ] Expression scanner (nested brackets, strings) for `$(...)` and `${...}`.
- [ ] Embedded JavaScript engine behind a trait (Boa first; QuickJS-NG or V8 optional), `expressionLib`, `inputs`/`self`/`runtime` globals, timeout, no I/O.

CommandLineTool life cycle
- [ ] Command-line builder: `baseCommand`, `arguments`, `inputBinding` ordering by `position` and then name, `prefix`, `separate`, `itemSeparator`, `valueFrom`, `shellQuote`, `ShellCommandRequirement`, and nested arrays and records.
- [ ] `runtime` object (`outdir`, `tmpdir`, `cores`, `ram`, `outdirSize`, `tmpdirSize`, `exitCode`).
- [ ] `ResourceRequirement` (fractional cores, float sizes, expressions), mapped to TES `cpu_cores`, `ram_gb` and `disk_gb`.
- [ ] `EnvVarRequirement`, `HOME`/`TMPDIR` rules, stdin, stdout and stderr (including the `stdout`/`stderr` type shortcuts and random names), `successCodes`, `temporaryFailCodes`, `permanentFailCodes`.
- [ ] `InitialWorkDirRequirement` (Dirent, expressions, writable copies, absolute entrynames in containers, path-traversal checks).
- [ ] `DockerRequirement` (`dockerPull`, `dockerImageId`, `dockerOutputDirectory`). `dockerFile` and `dockerLoad` are unsupported on TES (exit 33 or a clear error).
- [ ] `SoftwareRequirement` (hint-level: warn or map to images), `ToolTimeLimit` (cancel), `NetworkAccess` (enforce locally, warn on TES), `WorkReuse` (cache key design), `InplaceUpdateRequirement`.
- [ ] Output collection: glob (string, array, expression), symlinks, Directory outputs, `outputEval` with `self`, `cwl.output.json` (drop undeclared keys, as v1.3 requires), and secondaryFiles on outputs.
- [ ] Remote output collection for TES: whole-outdir DIRECTORY output, listing and checksum manifest, empty-dir handling.

Workflow engine (shared with the iunctus intermediate representation)
- [ ] Step input resolution: `source` (single or multiple), `linkMerge`, `pickValue`, `default`, `valueFrom` with `self`, and `loadContents` on step inputs.
- [ ] `scatter` with `dotproduct`, `nested_crossproduct` and `flat_crossproduct`, including empty arrays and nested scatter.
- [ ] `when` conditionals (null outputs for skipped steps), and `pickValue` on workflow outputs.
- [ ] Subworkflows (with `SubworkflowFeatureRequirement`), `StepInputExpressionRequirement`, `MultipleInputFeatureRequirement`, `ScatterFeatureRequirement`, and requirement inheritance (most specific wins, requirements override hints).
- [ ] ExpressionTool execution in the engine (no container).
- [ ] `Operation`: parse and validate, refuse to run.
- [ ] Later (v1.3): `LoopWorkflowStep`.

Testing and tooling
- [ ] A `cwl-runner`-compatible CLI (`--outdir`, `--quiet`, output object JSON on stdout, exit code 33 for unsupported features).
- [ ] cwltest in CI for v1.0, v1.1 and v1.2 (and v1.3-dev optionally), per backend, with per-tag badges.
- [ ] Regression test workflows of real-world CWL (for example from WorkflowHub or Dockstore) to test JavaScript fidelity beyond the suite.

**Effort estimate (inference, one experienced Rust developer, from scratch):**

| Area | Person-weeks |
|---|---|
| Loader, Schema Salad subset, typed model, version upgrade | 4 to 8 |
| Types, validation, input loading, File metadata | 4 to 6 |
| Parameter references, interpolation, JavaScript embedding | 2 to 4 |
| Command-line builder, runtime, requirements | 4 to 6 |
| InitialWorkDir, secondaryFiles, glob and output collection (local) | 6 to 8 |
| Workflow semantics (scatter, when, pickValue, valueFrom, subworkflows) | 4 to 8 (some shared with the intermediate representation) |
| TES backend, object storage staging, remote output collection | 6 to 10 |
| Conformance long tail, v1.0/v1.1 upgrade, fixes for the real-world test workflows | 6 to 10 |
| **Total** | **about 36 to 60 weeks (8 to 13 person-months)** |

- Reaching 100% of `required` (84 tests, no JavaScript) is the first milestone. That is maybe 3 to 4 months. The optional features (JavaScript, InitialWorkDir, and so on) are what make real workflows run.
- With reuse of `cwl_core` (parsing) and porting of commonwl logic, subtract roughly 3 to 6 months. Add 2 to 4 weeks of integration and ongoing upstream tracking.

**Risk assessment (inference):**
- Low: spec clarity and test coverage. The spec is stable, v1.2.1 has 378 tests, and cwltool is the oracle.
- Low to medium: Schema Salad in Rust. The needed subset is small. No official Rust codegen will arrive soon (PR #917 is stalled).
- Medium: JavaScript fidelity. Boa passes the suite (per commonwl), but real-world `expressionLib` code may hit gaps. Mitigate with an engine trait and test-workflows tests.
- Medium to high: TES file semantics. There are no writable inputs, no network flag, and no symlinks or empty dirs on S3. Server bugs exist (Funnel stdin truncation, crash under load). Outputs need a second pass on storage. Mitigate with a whole-outdir upload plus a manifest executor, and test on several TES servers.
- Medium: fitting CWL into a unified intermediate representation shared with Nextflow and Snakemake. CWL needs per-task pre and post hooks in the engine (command building from runtime values, output evaluation). A pure "command plus files" model is not enough.
- Medium: dependency risk if reusing `commonwl`. It has one main maintainer and a fast-moving 0.x API.

### Gaps
- The effort numbers are my estimates. They are anchored on commonwl's size and history and on cwltool's size. They are not measured.
- I did not measure how often real-world CWL (WorkflowHub or Dockstore) uses JavaScript, InitialWorkDir or `$graph` packing. That data would sharpen the priorities.
