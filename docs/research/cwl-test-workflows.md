<!-- CWL test workflows, evaluation of Rust CWL loaders on them, and an inventory of the CWL features real workflows use. -->
<!-- Copyright (c) 2026 The iunctus Contributors -->
<!-- SPDX-License-Identifier: MIT or Apache-2.0 -->

# CWL test workflows: Rust loader evaluation and feature inventory

Research date: 2026-09-27. This note builds on [cwl.md](cwl.md). That note covers the spec and the existing engines. This note measures real CWL files.

Method in short:

1. I built a text-only set of CWL test workflows (section 1).
2. I loaded every CWL document with each Rust CWL loader I could find. A small Rust program did this (section 2).
3. I counted CWL features over the real-world part of the test workflows with Python scripts (section 3). Python and ruamel.yaml were only research tools here. They are not a proposal for iunctus.

All numbers are my own counts from the test workflows unless a source is given. The scripts are in `~/.cache/iunctus/test-workflows/cwl/scripts/`. The raw results (JSON and JSON Lines) are in `~/.cache/iunctus/test-workflows/cwl/results/`.

## Summary

- **Test workflows.** 1,122 conformance test files (v1.0, v1.1, v1.2) and 8,288 real-world text files. The real-world files come from 121 WorkflowHub CWL entries, their 42 source repositories and 9 other well-known collections. After removing byte-identical copies, the real-world part has 3,216 distinct CWL documents.
- **Rust loaders.** Only three Rust CWL loaders exist. Only one is usable: `cwl_core` 0.15.1 from FAIRagro's commonwl (MIT OR Apache-2.0). It loads all 633 valid conformance tool files and 3,190 of 3,216 distinct real-world documents (99.2%). cwl-zen loads 91.1%. cwl-rs loads 51.4%, because it rejects every v1.2 document.
- **cwl_core is a typed serde model, not a Schema Salad implementation.** It handles local `$import`, map-to-list forms and the two type DSLs. It does not flatten imported arrays. It does not support `$import` of fragments or URLs. It resolves `$include` in three fields only. It keeps `run:` as a path string, does no IRI resolution and does no version upgrade.
- **cwl_core silently drops a whole `requirements` list when one entry does not fit its model.** This happens in 74 of 2,585 real-world documents that have requirements (2.9%). In 54 of them the lost list contained InlineJavascriptRequirement.
- **WorkflowHub.** Of 121 CWL entries, 79 main workflows load together with all their `run:` references in cwl_core. 23 entries are abstract workflows built from `Operation` steps. They cannot run. That leaves 95 concrete workflows.
- **CWL v1.0 is still the most common version.** It covers 61% of distinct real-world documents and 44 of 117 WorkflowHub main workflows. Loading v1.0 with the upgrade rules is required.
- **JavaScript is common but short.** 56 of 95 concrete WorkflowHub entries use at least one JavaScript expression. The median JavaScript expression has 44 characters, and 90% have fewer than 193. 54% of all expressions are plain parameter references that need no JavaScript engine. 14 WorkflowHub entries use ES2015+ syntax (arrow functions, `let`, `includes`), although the spec says ECMAScript 5.1.
- **Feature use in the 95 concrete WorkflowHub entries:** DockerRequirement 74 (as a hint in 65), JavaScript 56, Directory type 53, InitialWorkDirRequirement 48, step `valueFrom` 42, subworkflows 41, ExpressionTool 33, scatter 33, `when` 26, `pickValue` 24, secondaryFiles 24, ShellCommandRequirement 22, SoftwareRequirement 22. Rare: `loadContents` 12, `dockerFile` 9, record types 8. Never used anywhere in the test workflows: ToolTimeLimit, InplaceUpdateRequirement, `nested_crossproduct`.

## 1. Test workflows

### Takeaway

The test workflows live in `~/.cache/iunctus/test-workflows/cwl/`. It holds about 9,410 text files (31.5 MB), plus 66 MB on disk including the blobless git metadata. Every source and commit is listed in `~/.cache/iunctus/test-workflows/cwl/revisions.txt`. The real-world part is skewed toward a few large repositories and a few organisations, so section 3 gives counts per document, per repository and per WorkflowHub entry.

### Findings

Contents:

| Folder | Source | Text files | CWL documents |
|---|---|---|---|
| `conformance/v1.0` | [common-workflow-language](https://github.com/common-workflow-language/common-workflow-language) `v1.0/` at `98e95c4` | 318 | 161 tool files named in 197 tests |
| `conformance/v1.1` | [cwl-v1.1](https://github.com/common-workflow-language/cwl-v1.1) `tests/` and `conformance_tests.yaml` at `3867f2f` | 323 | 210 tool files, 253 tests |
| `conformance/v1.2` | [cwl-v1.2](https://github.com/common-workflow-language/cwl-v1.2) `tests/` and `conformance_tests.yaml` at `9790833` | 481 | 316 tool files, 378 tests |
| `realworld/workflowhub/<id>` | WorkflowHub copy of each of the 121 entries (latest version) | 2,345 | 1,621 |
| `realworld/sources/` | the 42 source repositories named by the WorkflowHub entries | 3,657 | 2,274 |
| `realworld/collections/` | 9 other collections (table below) | 2,286 | 1,561 |

The source and collection folders include 5 git submodules at their pinned commits, because workflows reference tools inside them. They hold 294 CWL documents. The feature inventory leaves them out, so each tool is counted once, in its own repository.

Other collections:

| Collection | What it is | Commit | Distinct documents |
|---|---|---|---|
| [common-workflow-library/bio-cwl-tools](https://github.com/common-workflow-library/bio-cwl-tools) | community tool descriptions | `8104953` (branch `release`) | 145 |
| [genome/analysis-workflows](https://github.com/genome/analysis-workflows) | McDonnell Genome Institute pipelines | `5fda2d9` | 254 |
| [ncbi/pgap](https://github.com/ncbi/pgap) | NCBI Prokaryotic Genome Annotation Pipeline | `2a81fcd` | 271 |
| [NCI-GDC/gdc-dnaseq-cwl](https://github.com/NCI-GDC/gdc-dnaseq-cwl) | NCI Genomic Data Commons DNA-Seq | `86f9497` | 73 |
| [kids-first/kf-alignment-workflow](https://github.com/kids-first/kf-alignment-workflow) | Kids First alignment | `43d7b79` | 259 |
| [kids-first/kf-somatic-workflow](https://github.com/kids-first/kf-somatic-workflow) | Kids First somatic calling | `41ac019` | 103 |
| [Barski-lab/workflows](https://github.com/Barski-lab/workflows) | BioWardrobe and SciDAP workflows | `4723ce5` | 229 |
| [hubmapconsortium/salmon-rnaseq](https://github.com/hubmapconsortium/salmon-rnaseq) | HuBMAP RNA-seq | `81e4484` | 19 |
| [mskcc/ACCESS-Pipeline](https://github.com/mskcc/ACCESS-Pipeline) | MSKCC ACCESS pipeline | `b0f226a` | 93 |

WorkflowHub entries:

- The API lists 121 entries with workflow type `cwl` ([query](https://workflowhub.eu/workflows?filter%5Bworkflow_type%5D=cwl)).
- 57 entries come from 4 groups. DT-GEO has 23 entries (earthquake and volcano workflows, almost all abstract). BioExcel biobb has 18, UNLOCK (Wageningen) has 16 and EBI Metagenomics has 5. The other 59 entries come from single projects. Two entries are test entries (183 "testEntryTitleNew" and 188 "entryTitleTest").
- The source repository comes from the RO-Crate `isBasedOn` field (`/workflows/<id>/ro_crate_metadata`). 92 entries name a repository: 36 on GitHub, 2 on gitlab.com, 2 on git.wur.nl, 1 on forge.inrae.fr and 1 on gitlab.bsc.es. 28 entries have no source URL (uploaded files). One points to a Renku notebook URL, which is not a git repository.
- Entry 1081 returns HTTP 403 for its files. Entry 885 contains no CWL file.
- WorkflowHub can hold files "externally". For those, the raw endpoint redirects to an HTML page, and the blob endpoint shows a `remote_source` annotation. Two files were like this. I removed them. For entry 1818 I used the main workflow from its remote source on git.wur.nl. One registered file (`98/1_md_list.cwl`) really is an HTML page.

How it was built:

- Git sources: `git clone --depth 1 --filter=blob:none --no-checkout`, then `git sparse-checkout set --no-cone` with `*.cwl *.yml *.yaml *.json *.js`. For GitHub repositories I first read the tree API and excluded files over 1 MiB (one file). One oversized file on GitLab was excluded after checkout.
- A second pass added files named in `$include`, `$import` or `run:` that the patterns missed. These were 31 R and Python scripts in 6 repositories. WorkflowHub copies got 42 such files.
- WorkflowHub files came one by one from `/workflows/<id>/git/<version>/tree` and `/raw/<path>`.
- At most 4 parallel downloads, all under `nice -n 19`. No data files, containers or test datasets were fetched.

### Caveats

- Source repositories are at their default branch on 2026-09-27, not at the commit registered in WorkflowHub. The WorkflowHub copies hold the registered version but often lack the referenced tool files. For 64 entries the registered main file is byte-identical to the file in the source clone. For 24 more the same path exists with changes.
- Many repositories contain stale files with broken references, for example `workflows/to_check/` in [m-unlock/cwl](https://gitlab.com/m-unlock/cwl). I kept them because they are part of real repositories. They raise the "missing file" counts in section 2.
- Five repositories hold 1,224 of the 3,216 distinct documents: pgap (271), kf-alignment-workflow (259), analysis-workflows (254), Barski-lab (229) and m-unlock (219).
- Duplicates are removed only when files are byte-identical (SHA-1). Near-copies, such as m-unlock on gitlab.com and on git.wur.nl, still count twice.

## 2. Rust CWL loaders

### Takeaway

`cwl_core` (FAIRagro commonwl) is the only Rust CWL loader worth considering. Its model covers almost all real syntax: 99.2% of distinct real-world documents and 100% of valid conformance tool files load. Its weak points are Schema Salad preprocessing, version handling, and error handling that sometimes drops data silently. The other two loaders are narrow or dead.

### 2.1 Options found

| Project | Crate | Version evaluated | License | Status | Scope |
|---|---|---|---|---|---|
| [fairagro/commonwl](https://github.com/fairagro/commonwl) | `cwl_core` plus `cwl_salad` (also re-exported by `commonwl`) | 0.15.1 from crates.io (2026-09-09). Its source is identical to git `044345d` (2026-09-17). | MIT OR Apache-2.0 | active. 32 releases of `cwl_core` from 2026-03-04 to 2026-09-09. One main developer (643 commits). | v1.2 model: CommandLineTool, Workflow, ExpressionTool, Operation, packed `$graph` |
| [inutano/cwl-zen](https://github.com/inutano/cwl-zen) | `cwl-zen` (not on crates.io) | git `319436c` (2026-04-05) | `Cargo.toml` says MIT, but there is no LICENSE file, and GitHub reports no license | no commits since 2026-04, 0 stars | "JS-free subset of CWL v1.2": CommandLineTool, Workflow, ExpressionTool |
| [onnovalkering/cwl-rs](https://github.com/onnovalkering/cwl-rs) | `cwl` (never published) | git `4db80bd` (2021-08-19) | MIT | archived | v1.0 and v1.1 only: CommandLineTool and Workflow |

I searched crates.io for "cwl", "commonwl", "common workflow language", "schema-salad" and "salad", and GitHub for Rust repositories about CWL. I found nothing else that parses CWL. The draft Rust code generator for Schema Salad ([PR #917](https://github.com/common-workflow-language/schema_salad/pull/917)) has no deserialization yet (see [cwl.md](cwl.md)). I did not test it.

### 2.2 Method

- The program is in `scripts/loadeval/`. It has one binary per loader and a shared harness. The harness reads file paths, calls the loader, catches panics and prints one JSON line per file.
- cwl_core: `cwl_core::load_cwl_file(path, true)`, which includes `$import` preprocessing. For Workflow documents I also call `cwl_core::packed::pack_cwl(&doc, path, None)`. It loads every `run:` file recursively. I call this the "run tree" check.
- cwl-zen: `cwl_zen::parse::parse_cwl(path)`. For `$graph` files it retries with `path#main`.
- cwl-rs: `cwl::v11::CwlDocument::from_path(path)`.
- "Loads" means no error and no panic. It does not mean the model is right. For cwl_core I also compared the number of requirements in the model with the number in the raw YAML (`scripts/dropped_reqs.py`).
- Conformance input: each tool file named in a test, with its `#fragment` when the test uses one (687 files).
- Real-world input: every file that is a CWL document, plus `.cwl` files that are not valid YAML. A CWL document is a mapping with a process `class`, a `cwlVersion`, or a `$graph` of processes.
- Build: Rust 1.98.1, release mode. `cwl_core` pulls 119 crates for parsing alone. Loading all 5,477 real-world files, run trees included, took about 10 seconds. The slowest single file took 140 ms.

### 2.3 Results on the conformance tests

Files used only by valid tests, and files used only by `should_fail` tests (16 files used by both are left out):

| Loader | v1.0 (158 valid files) | v1.1 (196) | v1.2 (279) | `should_fail`-only files rejected at load (of 38) |
|---|---|---|---|---|
| cwl_core | 158 (100%) | 196 (100%) | 279 (100%) | 0 |
| cwl-zen | 157 | 194 | 268 | 2 |
| cwl-rs | 98 (62%) | 121 (62%) | 4 (1%) | 25 |

- cwl_core also passed the run-tree check for all 280 conformance workflows.
- Most `should_fail` tests fail at run time, so loading them is correct. But five v1.2 tests are pure syntax checks: v1.2 features in documents marked v1.0 or v1.1 (for example `invalid_syntax_v10_uses_v12_tool`). cwl_core accepts all five, because it treats every document as v1.2.
- cwl-rs "rejects" 25 because it rejects v1.2 as a whole, not because it validates.

### 2.4 Results on real-world CWL

Distinct documents (byte-identical copies removed, submodule copies excluded):

| Documents | Count | cwl_core | cwl-zen | cwl-rs |
|---|---|---|---|---|
| All | 3,216 | 3,190 (99.2%) | 2,930 (91.1%) | 1,653 (51.4%) |
| CommandLineTool | 2,185 | 99.5% | 96.6% | 68.5% |
| Workflow | 851 | 99.2% | 81.3% | 18.3% |
| ExpressionTool | 94 | 95.7% | 98.9% | 0% |
| Operation | 44 | 100% | 0% | 0% |
| packed (`$graph`) | 42 | 88.1% | 83.3% | 0% |
| cwlVersion v1.0 (with `cwl:v1.0`) | 1,959 | 99.1% | 95.4% | 81.1% |
| cwlVersion v1.1 | 83 | 98.8% | 97.6% | 77.1% |
| cwlVersion v1.2 (with dev versions) | 1,174 | 99.3% | 83.5% | 0% |

- 16 more `.cwl` files (10 distinct) are not valid YAML. They have duplicate keys or broken mappings, and ruamel.yaml rejects them too. cwl_core rejects all 16. cwl-zen accepts 4 and cwl-rs accepts 10, so those two do not catch duplicate keys reliably.
- cwl_core run-tree check over the 851 distinct Workflow documents:
  - 592 (69.6%) load with every referenced file.
  - 119 fail because an `Operation` appears in the tree. `pack_cwl` hits `unimplemented!()` and panics ([packed.rs line 234](https://github.com/fairagro/commonwl/blob/044345dcdf36f016d8709e5aa5e116164d0b48d5/crates/core/packed.rs#L234)).
  - 106 fail because a referenced file is missing from the repository.
  - 27 fail because a referenced document fails to load.
  - 7 fail because the workflow itself fails to load.

### 2.5 Why cwl_core rejects documents (26 distinct documents)

| Cause | Documents | Example | Whose problem |
|---|---|---|---|
| `cwlVersion: cwl:v1.0` | 8 | `vetscience/Assemblosis/Run/assembly.cwl` | cwl_core. `cwlVersion` is a vocabulary field, so the prefixed form should expand to v1.0 under Salad rules. I did not confirm that cwltool accepts it. |
| YAML that ruamel.yaml accepts but serde-saphyr 1.3.0 rejects | 7 | `genome/analysis-workflows/definitions/subworkflows/gatk_haplotypecaller_iterator.cwl` | YAML parser. In 6 documents a multi-line quoted scalar has continuation lines that are not indented deeper than its key. In 1 document a plain `-` stands inside a flow sequence (`[odgi, build, -o, -, ...]`). |
| `$import` of a `#fragment` in the same document | 5 | `m-unlock/cwl/packed/workflow_indexbuilder_packed.cwl` (`$import: "#main/STAR"`) | cwl_core. The [Salad import rules](https://github.com/common-workflow-language/schema_salad/blob/main/src/schema_salad/metaschema/import_include.md) allow fragments, http and https. |
| Not valid CWL (`outputs:` empty, or a list of strings) | 3 | `longmanplus/EOSC-Life_demos/.../locate_wh.cwl` | the document |
| The same requirement class listed twice | 2 | `Barski-lab/workflows/tools/trimmomatic.cwl` | Unclear. cwl_core says the spec forbids this ([validate.rs](https://github.com/fairagro/commonwl/blob/044345dcdf36f016d8709e5aa5e116164d0b48d5/crates/core/validate.rs)). I found no such rule in the v1.2 `Process.yml`, `CommandLineTool.yml` or `Workflow.yml`. |
| `$import` target missing from the repository | 1 | `Barski-lab/workflows/devel/samtools/samtools-view.cwl` | the document |

I confirmed the YAML cases with minimal inputs (`scripts/yamltest/`). serde-saphyr 1.3.0 rejects `[a, -, b]` and duplicate keys. It accepts `[ File?, string? ]` and anchors with aliases. ruamel.yaml 0.19.1 accepts all three valid inputs. Salad forbids anchors and aliases ([cwl.md](cwl.md) section 2), so a strict loader should reject them.

cwl-zen fails mostly on legal forms its narrow model does not cover:

- `outputSource` or `source` as a list (98 files).
- `secondaryFiles` as a single string or an expression (about 80).
- The `Operation` class (86).
- `doc` as a list, and single-string `baseCommand` variants.

cwl-rs fails on v1.2 (1,154 files), on step shapes it does not model (227), on ExpressionTool (63) and on dev versions such as `v1.2.0-dev2` (25).

### 2.6 WorkflowHub entries: does the whole workflow load?

For each entry I took the RO-Crate `mainEntity`. If it was missing or not CWL, I took a Workflow file in the entry (16 entries, listed in `results/wfh_entries.json`). I preferred the copy in the source repository, because the WorkflowHub copy often lacks the referenced tools.

| Outcome with cwl_core | Entries |
|---|---|
| Main workflow and all `run:` references load | 79 |
| Single CommandLineTool entry, loads | 3 |
| Packed entry, loads | 2 |
| Run tree contains `Operation` (abstract workflow), `pack_cwl` panics | 23 |
| A referenced file is missing from the registration or the repository | 6 |
| Main file is invalid YAML (duplicate key) | 2 |
| No CWL in the entry (1081: HTTP 403, 885: README only) | 2 |
| `run:` uses a namespace prefix that expands to a raw GitHub URL (entry 455) | 1 |
| `run:` uses absolute paths from the author's machine (`/home/gbayarri_local/...`, entry 257) | 1 |
| A referenced tool fails the serde-saphyr YAML check (entry 63) | 1 |
| Main file uses `cwlVersion: cwl:v1.0` (entry 334) | 1 |

So 84 of 121 entries load fully. At least 5 more fail only because of loader limits: entries 63, 334 and 455, plus loading Operation trees without packing.

### 2.7 API shape of cwl_core 0.15.1

Sources: [load.rs](https://github.com/fairagro/commonwl/blob/044345dcdf36f016d8709e5aa5e116164d0b48d5/crates/core/load.rs), [documents.rs](https://github.com/fairagro/commonwl/blob/044345dcdf36f016d8709e5aa5e116164d0b48d5/crates/core/documents.rs), [requirements.rs](https://github.com/fairagro/commonwl/blob/044345dcdf36f016d8709e5aa5e116164d0b48d5/crates/core/requirements.rs), [packed.rs](https://github.com/fairagro/commonwl/blob/044345dcdf36f016d8709e5aa5e116164d0b48d5/crates/core/packed.rs), [cwl_salad deserialize.rs](https://github.com/fairagro/commonwl/blob/044345dcdf36f016d8709e5aa5e116164d0b48d5/crates/salad/deserialize.rs).

- **Entry points.** `load_cwl_file(path, preprocess)`, `from_str`, `packed_from_str` and `preprocess_cwl_file`. Also `packed::pack_cwl` and `PackedCWL::unpack(Option<&str>)`.
- **Types.** `CWLDocument` is an enum tagged by `class`, with variants `CommandLineTool`, `ExpressionTool`, `Operation` and `Workflow`.
  - Other types: `WorkflowStep`, input and output parameter types, `CommandLineBinding`, `CommandOutputBinding`, `CWLType`, `SecondaryFileSchema`, `File`, `Directory` and `Dirent`.
  - `ToolRequirements` has 13 variants and `WorkflowRequirements` has 17. `ToolHints` and `WorkflowHints` hold a known requirement or any JSON value.
  - Structs have builders (`bon`) and validation (`validator`).
- **Expressions stay as text.** Examples: `NumberOrExpression::Expression(String)`, `when: Option<String>`, `valueFrom` as a string. `cwl_core` evaluates nothing. The engine crate uses Boa.
- **`$import`** is handled only in `preprocess_cwl_file`. It replaces `{$import: file}` with the parsed local file and detects cycles. It then serializes the whole tree back to YAML and parses it again ([load.rs line 112](https://github.com/fairagro/commonwl/blob/044345dcdf36f016d8709e5aa5e116164d0b48d5/crates/core/load.rs#L112)). It does not flatten arrays, does not handle fragments and does not load http or https.
- **`$include`** exists only as a `StringOrInclude` type in `expressionLib`, `Dirent.entry` and `dockerFile`. The file is read later by `pack_cwl` or the engine.
- **`run:`** is a `StringOrDocument`: a path string or an inline document. It is not resolved on load. `pack_cwl` and the engine join it with the workflow directory ([backend.rs line 653](https://github.com/fairagro/commonwl/blob/044345dcdf36f016d8709e5aa5e116164d0b48d5/crates/engine/backend.rs#L653)). URLs and namespace prefixes are not supported.
- **`$graph`** is detected when the file text contains the substring `$graph` ([load.rs line 40](https://github.com/fairagro/commonwl/blob/044345dcdf36f016d8709e5aa5e116164d0b48d5/crates/core/load.rs#L40)). No file of the test workflows triggers this check by mistake. `unpack` picks the entry named by the fragment, or `main`, or the only entry. It strips `#main/` prefixes from ids.
- **IRIs.** Ids are kept as written. There is no general identifier or link resolution. Named types stay plain strings (`CommandInputType::String`). `SchemaDefRequirement.types` is an untyped JSON value.
- **Salad forms.** Map-to-list works for `inputs`, `outputs`, `steps`, step `in`, `requirements`, `hints`, `envDef` and `packages`. The type DSL (`File[]?`) and the secondaryFiles DSL work too.
- **`$namespaces` and `$schemas`** are kept as extension fields. Only the engine's format checking uses them.
- **Unknown fields.** Unknown top-level fields go into `extension_fields`, so typos pass silently. Unknown step fields are ignored.
- **Silent loss of requirements.** `requirements` and `hints` use `deserialize_map_list_option_class`. It returns `None` when any entry fails ([deserialize.rs line 76](https://github.com/fairagro/commonwl/blob/044345dcdf36f016d8709e5aa5e116164d0b48d5/crates/salad/deserialize.rs#L76)). In the test workflows this hit 74 documents:
  - 68 CommandLineTools list a workflow-only requirement, such as StepInputExpressionRequirement. The v1.2 schema types `requirements` as `ProcessRequirement[]`, so these files are valid ([Process.yml](https://github.com/common-workflow-language/cwl-v1.2/blob/main/Process.yml)).
  - 2 use `LoadListingRequirement` without `loadListing`. That field is optional in v1.2 (`LoadListingEnum?`), but cwl_core requires it.
  - 2 `$import` a list into the requirements list. Salad flattens such lists, but cwl_core does not.
  - 2 use Arvados `arv:` requirements.
  - The lost lists contained InlineJavascriptRequirement (54 documents), ResourceRequirement (38), DockerRequirement (24), ShellCommandRequirement (14) and InitialWorkDirRequirement (12).
- **Versions.** `cwlVersion` must match a `vX.Y[.Z[-pre]]` pattern. There is no v1.0 or v1.1 upgrade and no check that a feature fits the declared version.
- **YAML.** serde-saphyr 1.3.0 with strict booleans. It rejects duplicate keys. It accepts anchors and aliases.
- **Errors.** Parse errors carry line and column. Validation errors do not. The model keeps no source spans.
- **Numbers.** Values such as `default` are `serde_json::Value`, so numbers are `f64`. Resource fields use `i32`, `i64` or `f32`.

### 2.8 API shape of cwl-zen and cwl-rs

- **cwl-zen** ([parse.rs](https://github.com/inutano/cwl-zen/blob/319436cb7bbb261608f237420d3b4c830142d068/src/parse.rs), [model.rs](https://github.com/inutano/cwl-zen/blob/319436cb7bbb261608f237420d3b4c830142d068/src/model.rs)):
  - `parse_cwl(path)` accepts `file#id` for `$graph` files and inlines `run: "#id"`.
  - It reads `$namespaces` only to expand `format`. It has no `$import` and no `$include`.
  - Requirements and hints are untyped `Vec<serde_yaml::Value>`. Inputs and outputs are a `HashMap`, so their order is lost.
  - It uses `serde_yaml` 0.9, which is deprecated. It has no JavaScript by design.
- **cwl-rs** ([lib and v11 modules](https://github.com/onnovalkering/cwl-rs/tree/master/src)):
  - `CwlDocument::from_path` supports cwlVersion v1.0 and v1.1, and only CommandLineTool and Workflow.
  - It uses `serde_yaml` 0.8 and has no Salad preprocessing.
- Neither resolves `$import` or `run:` references.

### Inferences

- cwl_core is the only realistic Rust starting point. Its breadth of syntax is not the problem. Its gaps are in preprocessing, strictness and versions.
- Gaps that real workflows hit, in order of impact:
  1. **Silent requirement loss (2.9% of documents).** iunctus must turn this into an error or a warning. Otherwise a tool can run without its container or without JavaScript enabled.
  2. **v1.0 upgrade semantics**, because v1.0 covers 61% of documents. Examples are network access on and deep directory listing ([cwl.md](cwl.md) section 1).
  3. **Loading and validating `Operation` trees** (23 WorkflowHub entries). iunctus must then refuse to run them with a clear error.
  4. **Salad features seen in the test workflows:** `$import` fragments (5 documents), array flattening (2), `$include` in other fields (1, in `dockerPull`), namespace-prefixed or URL `run:` (5 documents, 1 entry) and `cwl:v1.0` (8 documents, 1 entry).
  5. **YAML compatibility with ruamel.yaml**, which cwltool uses: 7 documents in analysis-workflows, ACCESS-Pipeline and plant2human. Either the YAML layer must accept these, or the fix must go into saphyr.
  6. **Source spans** for good error messages.
- Three ways to use cwl_core (inference, not decided):
  1. Depend on it and send fixes upstream.
  2. Put an iunctus Salad preprocessing pass in front of its types. This pass would work on a YAML tree with spans and would do imports, includes, namespaces, IRIs and the version upgrade.
  3. Write an iunctus model, using cwl_core as reference and oracle.

  Option 2 keeps most of the value of its roughly 6,000 lines of types. It also moves the risky part (Salad and IRIs) under iunctus control.
- These test workflows and the harness can serve as the loader regression suite: 687 conformance files, 3,216 real documents and 121 WorkflowHub entries.

### Gaps

- I did not run cwltool on the test workflows. So "valid" means "ruamel.yaml parses it and it is not obviously broken". It does not mean cwltool accepts it. This affects the `cwl:v1.0`, duplicate-requirement and `$import`-fragment cases.
- I measured whether loaders load, not whether their models are right. Apart from the requirement counts, I did not diff the cwl_core model against cwltool's normalized output.
- I did not evaluate the schema_salad Rust code generator (PR #917).

## 3. Feature inventory (real-world test workflows only)

### Takeaway

Real CWL uses most of the spec, but in simple ways. The long tail is in files, directories and JavaScript glue, not in workflow control. A concrete WorkflowHub entry uses a median of 5 of the 20 advanced features listed below. 26 of 95 use none of them: they need only CommandLineTool, Workflow, basic types, parameter references and a container image.

### 3.1 Scope and method

- Input: the WorkflowHub copies, source repositories and collections. Submodule copies are excluded. 5,162 document files reduce to 3,216 distinct documents.
- These come from 87 origins: 38 repositories with CWL files, 9 collections, and 40 WorkflowHub entry folders that add documents found nowhere else.
- Two views:
  1. All distinct documents. The tables give documents and origins.
  2. Per WorkflowHub entry: all documents reachable from the entry's main workflow through `run:` (1,031 distinct documents, 118 entries). Of these entries, 95 are concrete and 23 contain `Operation`. This view matches decision 4 in [decisions.md](../decisions.md).
- YAML was read with ruamel.yaml (YAML 1.2, as in cwltool). PyYAML (YAML 1.1) fails on 37 of the distinct documents, for example on `type: [ File?, string? ]`.
- Expressions: I scanned strings only in fields where the spec allows expressions, such as `valueFrom`, `glob`, `when`, `outputEval`, `entry`, `arguments` and resource fields. The scanner handles nested brackets and quotes.
  - A `$(...)` counts as a parameter reference when it matches the CWL grammar: `inputs`, `self` or `runtime`, followed by `.name`, `['name']` or `[n]`.
  - Everything else counts as JavaScript. `$(` inside `doc`, `label` or `default` was not counted (74 strings in 67 documents).
- Scripts: `scripts/inventory.py`, `scripts/wfh_reachable.py`, `scripts/wfh_per_entry.py`. Output: `results/inventory.json`, `results/inventory_wfh.json`, `results/wfh_per_entry.json`.

### 3.2 Document classes and versions

| Top-level class | Distinct documents |
|---|---|
| CommandLineTool | 2,185 (68%) |
| Workflow | 851 (26%) |
| ExpressionTool | 94 |
| Operation | 44 |
| packed `$graph` | 42 |

Counting inline and packed processes too, the test workflows have 2,607 CommandLineTools, 986 Workflows, 188 ExpressionTools and 324 Operations. Workflows are small: the median has 4 steps, 90% have at most 15, and the largest has 58.

| cwlVersion | Distinct documents | WorkflowHub main workflows (117) |
|---|---|---|
| v1.0 | 1,951, plus 8 written as `cwl:v1.0` (61% together) | 43, plus 1 `cwl:v1.0` |
| v1.1 | 83 (3%) | 6 |
| v1.2 | 1,149, plus 25 dev versions (`v1.2.0-dev2` 18, `v1.2.0-dev4` 7) (37% together) | 63, plus 4 `v1.2.0-dev2` |

The dev versions come from EBI Metagenomics and MetaGOflow. cwltool loads them through its dev-version update chain ([update.py](https://github.com/common-workflow-language/cwltool/blob/main/cwltool/update.py)).

### 3.3 Requirements and hints

"Documents" counts distinct documents that use the class as a requirement or a hint, at any level. "Origins" is out of 87. "WorkflowHub" counts concrete entries (out of 95) whose reachable documents use it.

| Class | Documents | Origins | WorkflowHub | Notes |
|---|---|---|---|---|
| InlineJavascriptRequirement | 1,849 | 59 | 59 | 704 documents declare it but use no JavaScript |
| DockerRequirement | 1,838 | 49 | 74 | hint in 1,090 documents, requirement in 754 |
| ResourceRequirement | 925 | 24 | 21 | `ramMin` 885, `coresMin` 566, `tmpdirMin` 103, `coresMax` 115; expressions in 83 documents |
| InitialWorkDirRequirement | 540 | 42 | 48 | see 3.7 |
| ShellCommandRequirement | 501 | 25 | 22 | always a requirement (one hint) |
| SubworkflowFeatureRequirement | 485 | 48 | 41 | |
| StepInputExpressionRequirement | 459 | 46 | 45 | |
| MultipleInputFeatureRequirement | 442 | 51 | 44 | |
| ScatterFeatureRequirement | 431 | 45 | 39 | |
| SoftwareRequirement | 343 | 13 | 22 | a hint in 342; 335 give `specs` (bio.tools or other URLs) |
| SchemaDefRequirement | 120 | 7 | 3 | types mostly come from `$import`ed files |
| NetworkAccess | 53 | 13 | 13 | `true` in 51; an expression in 4 |
| EnvVarRequirement | 51 | 14 | 7 | |
| `sbg:maxNumberOfParallelInstances`, `sbg:AWSInstanceType` | 49, 43 | 2 | 0 | Seven Bridges hints (Kids First) |
| WorkReuse | 34 | 3 | 2 | |
| LoadListingRequirement | 21 | 7 | 5 | `shallow_listing` 11, `deep_listing` 8, `no_listing` 4 |
| `cwltool:Secrets` | 5 | 3 | 1 | |
| `arv:RuntimeConstraints`, `arv:APIRequirement` | 3 | 1 | 0 | Arvados, used as requirements |
| ToolTimeLimit, InplaceUpdateRequirement | 0 | 0 | 0 | |

Step-level requirements and hints are rare: 4 documents have step requirements and 13 have step hints (all `sbg:AWSInstanceType`).

### 3.4 JavaScript and parameter references

Use:

- 2,358 of 3,216 documents (73%) contain an expression or a parameter reference.
- 1,214 documents (38%) contain JavaScript: 743 with a JavaScript `$(...)` and 685 with a `${...}` body.
- 69 documents use JavaScript without declaring InlineJavascriptRequirement themselves. They may inherit it from a parent workflow.
- Per WorkflowHub entry: 56 of 95 concrete entries use JavaScript, and 76 use parameter references.

Counts over all distinct documents:

| Kind | Count | Share |
|---|---|---|
| Parameter references `$(inputs.x.y)` | 6,290 | 54% |
| JavaScript `$(...)` | 3,484 | 30% |
| JavaScript `${...}` function bodies | 1,794 | 16% |

Length of JavaScript expressions (characters of code, trimmed; 5,278 expressions):

| | Median | p75 | p90 | p99 | Max |
|---|---|---|---|---|---|
| All JavaScript | 44 | 89 | 193 | 982 | 5,377 |
| `$(...)` | 31 | | 76 | | |
| `${...}` | 116 | | 372 | | |

In lines, the median is 1, p90 is 6, p99 is 28 and the maximum is 141. 210 expressions have more than 10 lines. The WorkflowHub subset looks the same (median 41, p90 181, p99 726).

Of the 3,484 JavaScript `$(...)` expressions:

- 721 are literals only: strings, `null`, `true`, or object literals such as `{class: 'Directory', listing: []}`.
- 448 only join references and string literals with `+`, for example `inputs.x.nameroot + ".bam"`.
- 763 call a function defined in `expressionLib`, for example `generateGATK4BooleanValue()`.

Where expressions sit (JavaScript only): `valueFrom` 2,683, `when` 696, `glob` 606, `outputEval` 311, `entry` 243, `expression` 188, `arguments` 111, `stdout` 102, `listing` 81, `secondaryFiles` 73, `ramMin` 54, `stderr` 41, `envValue` 20, and a few in other resource fields and `format`.

Most common constructs, counted per JavaScript expression:

- `.nameroot`, `.nameext` or `.basename`: 1,021
- `var`: 609
- `.length`: 452
- `.split(`: 410
- regular expression literals: 314
- `?:`: 298
- `self[0]`: 251
- `for` loops: 220
- `.replace(`: 211
- `.concat(`: 183
- `.contents`: 125
- `.join(`: 120
- `parseInt` or `parseFloat`: 74
- `typeof`: 63
- `Math.`: 46
- `JSON.`: 12
- `.sort(`: 10

ES2015+ syntax appears in about 100 expressions, in 14 of 95 WorkflowHub entries: arrow functions 34, `.find(` 28, `let` or `const` 19, `startsWith` or `endsWith` 19, `includes` 11, template literals 6, spread 3. No expression uses `require(`. One uses `console.`.

`expressionLib`:

- Inline in 145 documents from 8 origins, with 87 distinct libraries. The median library has 270 characters, the largest 2,195.
- `$include` inside `expressionLib`: 0 documents. `$import` inside it: 4.
- Only 5 WorkflowHub entries use `expressionLib`.

### 3.5 Files and directories

| Feature | Documents | Origins | WorkflowHub (of 95) |
|---|---|---|---|
| `Directory` type | 869 | 60 | 53 |
| secondaryFiles on inputs | 599 | 28 | 24 |
| secondaryFiles on outputs | 287 | 21 | 22 |
| `format` on inputs / outputs | 395 / 431 | 29 / 26 | 43 / 41 |
| `default` with a File or Directory literal | 98 | 19 | 14 |
| `loadContents` | 72 | 17 | 12 |
| `loadListing` field | 34 | 4 | 6 |
| `streamable` | 29 | 11 | 10 |

secondaryFiles forms, counted in documents:

- plain suffix pattern: 609
- `^` (strip an extension): 367
- expression: 41
- record form `{pattern, required}`: 45
- trailing `?`: 10

`format` values are mostly EDAM CURIEs such as `edam:format_1930`. 11 `format` values are expressions.

### 3.6 Workflow features

| Feature | Documents | Origins | Steps or uses | WorkflowHub (of 95) |
|---|---|---|---|---|
| `run:` to a file | 745 | 74 | 5,527 steps | 86 |
| `run:` inline | 152 | 22 | 407 steps | 15 |
| `run: "#id"` in packed files | 42 | 9 | 705 steps | 2 |
| `run:` with a namespace prefix (a URL) | 5 | 1 | 14 steps | 1 |
| step input `valueFrom` | 330 | 44 | 2,003 inputs | 42 |
| step input `default` | 308 | 33 | 1,775 inputs | |
| multiple `source` | 240 | 41 | 873 inputs | 36 |
| multiple `outputSource` | 44 | 14 | 63 outputs | |
| `scatter` | 273 | 41 | 794 steps | 33 |
| `when` | 148 | 28 | 857 steps | 26 |
| ExpressionTool (any level) | 141 | 25 | 188 objects | 33 |
| `Operation` (any level) | 138 | 7 | 324 objects | 23 entries are abstract |

- `scatterMethod` (documents): none given with a single scattered input in 196, `dotproduct` 113, `flat_crossproduct` 3, `nested_crossproduct` 0.
- `pickValue` (documents): `all_non_null` 87, `first_non_null` 79, `the_only_non_null` 2.
- `linkMerge` (documents): `merge_flattened` 161, `merge_nested` 18.
- `when` expressions are short. The most common are `$(inputs.x)` (a parameter reference) and `$(inputs.x !== null)`.

### 3.7 InitialWorkDirRequirement

Used in 540 documents (42 origins) and in 48 of 95 WorkflowHub entries. Forms, counted in documents:

| Form | Documents |
|---|---|
| Dirent (`entryname` / `entry`) | 355 |
| list item that is an expression or a reference such as `$(inputs.x)` | 158 |
| `writable: true` | 100 |
| whole `listing` as one expression | 50 |
| `entry: {$include: script}` | 47 |
| absolute `entryname` | 2 |
| File or Directory literal | 2 |

The `$include` form stages scripts (Python, R) into the working directory. That is why the test workflows include the referenced script files.

### 3.8 Types

Counted in documents:

- Primitive types: `File` 3,079, `string` 2,288, `int` 1,309, `boolean` 844, `Directory` 869, `float` 296, `long` 68, `double` 42.
- `enum` 281 (22 origins), `record` 50 (12 origins), `Any` 4.
- `?` shorthand 1,679, `[]` shorthand 897, explicit `type: array` 333, `null` in unions 435.
- `stdout` 331 and `stderr` 165 as output types.
- Named types from imported files, for example `../types/sequence_data.yml#sequence_data` in analysis-workflows. A loader must resolve these across files.
- Per WorkflowHub entry: `enum` in 18 and `record` in 8.

### 3.9 Command line details

- ShellCommandRequirement: 501 documents. `shellQuote: false`: 433 documents (21 origins, 23 WorkflowHub entries).
- `stdout:` 492 documents, `stderr:` 208 and `stdin:` 2.
- `successCodes` 21, `temporaryFailCodes` 12, `permanentFailCodes` 11.

### 3.10 Containers and software

- DockerRequirement keys: `dockerPull` in 1,818 documents, `dockerFile` in 24 (9 WorkflowHub entries), `dockerImageId` in 20 and `dockerOutputDirectory` in 1.
- 659 distinct images. By registry:
  - Docker Hub, without a registry host: 344, plus 6 written as `docker.io/...`
  - `quay.io` (mostly BioContainers): 200
  - `docker-registry.wur.nl`: 60
  - Seven Bridges registries: 45
  - `registry.gsc.wustl.edu`: 3
  - `ghcr.io`: 1
- About 13 concrete WorkflowHub entries have tools but no DockerRequirement anywhere in their reachable documents. They rely on tools installed on the host. One of them uses SoftwareRequirement.
- SoftwareRequirement is almost always a hint. The top packages are samtools 28, openjdk 24, sapp 23, bedtools 18 and pigz 18.

### 3.11 Schema Salad features in use

| Feature | Documents | Origins | WorkflowHub (of 95) | Where |
|---|---|---|---|---|
| map form of `inputs` / `outputs` | 2,870 / 2,828 | | | list form only in 325 / 348 |
| map form of `steps` / step `in` | 819 / 818 | | | list form in 74 / 59 |
| map form of `requirements` / `hints` | 1,035 / 728 | | | list form in 1,619 / 468 |
| `$namespaces` | 1,337 | 50 | 66 | mostly `s:` (schema.org) and `edam:` |
| `$schemas` | 822 | 28 | 38 | schema.org RDF and EDAM OWL URLs, some from GitHub `main` |
| `$import` | 196 | 10 | 2 | `types` 116, `s:mainEntity` 66, `requirements` 4, `outputs` 4, `expressionLib` 4 |
| `$include` | 61 | 10 | 7 | Dirent `entry` 47, `dockerFile` 15, `dockerPull` 1 |
| `$graph` | 42 | 9 | 2 | |
| `$base`, `$mixin` | 0 | 0 | 0 | |

### 3.12 Extensions and metadata

- `s:` (schema.org) keys appear in 965 documents: authors, licences, citations. `sbg:` keys appear in 28, `cwltool:` in 5 and `arv:` in 2.
- Namespaces: `s` is declared as both `http://schema.org/` (608) and `https://schema.org/` (461). `edam` is declared as both `http://` (448) and `https://` (161). So format IRIs from different files may not compare equal without normalization. This is an inference, not tested.

### Inferences: priorities for iunctus

- **Loader first.** Every entry needs:
  - map forms and both type DSLs
  - `$namespaces` (in 66 of 95 entries, mostly for metadata)
  - `$import` for types
  - `$include` for InitialWorkDir scripts
  - the v1.0 to v1.2 upgrade
  - `Operation` parsing
  - a warning, not an error, for unknown `sbg:` and `cwltool:` hints
  - no fetching of `$schemas` URLs at load time. They point at large ontologies. Format checking is optional in the spec.
- **Treat DockerRequirement hints as the container to use.** 65 of 74 WorkflowHub entries with Docker give it only as a hint. `dockerFile` (9 entries) does not fit TES and needs an image build step or a clear error.
- **Parameter references in pure Rust.** They cover 54% of expressions. 704 documents declare InlineJavascriptRequirement but need no engine.
- **JavaScript.** It is needed for 56 of 95 entries. Real code is short and ES5-style, but 14 entries use ES2015+ syntax. The embedded engine must accept ES2015+. Boa and QuickJS-NG both do ([rust-ecosystem.md](rust-ecosystem.md) section 2). No expression uses Node modules. One uses `console`.
- **Files and directories are central.** Directory appears in 53 entries, InitialWorkDir in 48 (with `writable: true` in 24) and secondaryFiles in 24. This matches the ranking in [cwl.md](cwl.md) section 8.
- **Workflow control.** Subworkflows (41 entries), step `valueFrom` (42), scatter (33, almost only dotproduct or a single input), ExpressionTool (33), and `when` with `pickValue` (26). All are needed for broad coverage. `nested_crossproduct` can wait. The test workflows never use it.
- **Can wait:** ToolTimeLimit and InplaceUpdateRequirement (unused), WorkReuse (2 entries), LoadListingRequirement (5), record types (8), `loadContents` (12).
- **Features come in bundles.** Adding features one at a time unlocks few extra workflows. 26 entries need none of the 20 advanced features. Most others need 5 or more at once. A CWL milestone should target the whole common set together.

### Gaps

- JavaScript classification uses my own scanner and a regular expression for the parameter reference grammar. Edge cases may be misclassified, for example escaped `\$(` or `$(` inside comments.
- I did not check which WorkflowHub entries actually run (no data, no cwltool runs). "Uses a feature" means the text contains it.
- Per-entry counts over-weight groups with many similar entries: DT-GEO 23, BioExcel 18 and UNLOCK 16 of 121.
- Reachability follows only `run:` file paths. It misses remote `run:` URLs (entry 455) and files that are missing from the repositories (7 entries).
- I did not add Dockstore CWL entries or `common-workflow-library/legacy` (archived). Both would add to the test workflows.

## 4. Reproduction

- Test workflows: `~/.cache/iunctus/test-workflows/cwl/` with `revisions.txt` (every source, branch or version, and commit).
- Build scripts: `~/.cache/iunctus/test-workflows/cwl/scripts/`
  - `fetch_repo.py` does the sparse, blobless clones. It is driven by `clone_one.sh`, `repos_wfh.txt` and `repos_extra.txt`.
  - `fetch_pinned.sh` fetches the submodules.
  - `fetch_meta.sh` and `fetch_snapshots.py` handle WorkflowHub. `entries.json` holds the entry to repository mapping.
  - `fix_refs.py` and `fix_refs_wfh.py` fetch referenced scripts.
- Analysis: `classify.py` (index), `conformance_list.py`, `loadeval/` (the Rust harness), `final_stats.py`, `dropped_reqs.py`, `wfh_entries.py`, `inventory.py`, `wfh_reachable.py` and `wfh_per_entry.py`. The Python scripts expect ruamel.yaml. They were run from a working copy in `/tmp/iunctus-cwl/`, so the paths inside them point there.
- Results: `~/.cache/iunctus/test-workflows/cwl/results/`, including per-file loader outputs (`res_all_*.jsonl`, `res_conf_*.jsonl`) and all extracted expressions (`expressions.jsonl`).
