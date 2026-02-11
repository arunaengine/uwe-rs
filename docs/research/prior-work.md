<!-- Research notes on earlier cross-language workflow tools and platforms. -->
<!-- Copyright (c) 2026 The iunctus Contributors -->
<!-- SPDX-License-Identifier: MIT or Apache-2.0 -->

# Prior work: cross-language tools, multi-language engines and unified representations for bioinformatics workflows

Research date: 2026-09-27. Scope: converters between workflow languages, platforms that run several languages, academic work on interoperability and intermediate representations (IRs), description and provenance standards, and re-implementations of Nextflow or Snakemake. "Cited Findings" are facts checked against the linked source (GitHub API, PyPI, crates.io, papers, docs). "Inferences" are my own reasoning. Items only seen in a search-result excerpt (page not opened) are marked as such.

## 1. Converters and translators between workflow languages

### Takeaway
Many converters exist, but none handles real Nextflow or Snakemake pipelines completely. Every tool either runs the original engine to get the concrete job graph, or parses statically and stops at embedded code (JavaScript, Groovy, Python). The best measured automatic coverage is 73-81% (CNT, CWL to Nextflow, two pipelines). Most small converters are abandoned. The active work in 2025-2026 is LLM-based conversion with validators (Seqera AI, Galaxy Workflow Foundry).

### Cited Findings

#### wf2wf (the closest match to the iunctus idea, as a converter)
- PyPI `wf2wf` 1.1.0, uploaded 2025-07-15. Summary: "Universal workflow-format converter built around a loss-preserving intermediate representation". - [PyPI wf2wf](https://pypi.org/project/wf2wf/)
- GitHub `csmcal/wf2wf`: MIT license, 1 star, last push 2025-07-15 (no activity in the 14 months since). - [GitHub csmcal/wf2wf](https://github.com/csmcal/wf2wf)
- Formats (import and export): Snakemake, Nextflow, CWL, HTCondor DAGMan, WDL, Galaxy, BioCompute Objects. All conversions go source -> IR (JSON/YAML) -> target. - [wf2wf README](https://github.com/csmcal/wf2wf)
- Loss handling: side-car `.loss.json` files; entries marked "lost", "lost again" or "reapplied" (restored on round trip); `--fail-on-loss` aborts on any loss. - [wf2wf README](https://github.com/csmcal/wf2wf)
- IR design (from `core.py`): a `Workflow` holds a dict of `Task`s, a list of `Edge`s (only `parent` task id -> `child` task id), workflow inputs/outputs, requirements/hints, provenance and documentation. Types are modeled on CWL (`TypeSpec` with File, Directory, array, record, enum; `FileSpec` with `secondary_files`, checksum, format IRI). Almost every `Task` field (command, script, cpu, mem_mb, container, conda, retry, `when`, `scatter`) is wrapped in `EnvironmentSpecificValue`, so one value can differ per execution environment ("shared_filesystem", "distributed_computing", "cloud_native"). - [wf2wf core.py](https://github.com/csmcal/wf2wf/blob/main/wf2wf/core.py)
- Snakemake import: runs `snakemake --dag --forceall` and `snakemake --dry-run` as subprocesses to get the graph and job details. It has a regex-based "parse-only" fallback. - [wf2wf importers/snakemake.py](https://github.com/csmcal/wf2wf/blob/main/wf2wf/importers/snakemake.py)
- Nextflow import: regex-based (for example `process\s+(\w+)\s*\{`, `include { X } from '...'`, and `withName:` blocks in config). - [wf2wf importers/nextflow.py](https://github.com/csmcal/wf2wf/blob/main/wf2wf/importers/nextflow.py)
- Self-reported Nextflow coverage: `process {}` parsing "basic"; channels "static analysis"; DSL2 modules "partial"; dynamic operators such as `collectFile` "limited"; params handling "roadmap". - [wf2wf Nextflow cheat-sheet](https://github.com/csmcal/wf2wf/blob/main/docs/user_guide/engines/nextflow.md)
- Self-reported Snakemake coverage: `shell:`/`script:` yes; `run:` blocks "auto-script"; wildcards yes; checkpoints "partial (static graph only)"; dynamic rules unsupported; `pipe()`, `dynamic()`, `shadow`, modules unsupported. - [wf2wf Snakemake cheat-sheet](https://github.com/csmcal/wf2wf/blob/main/docs/user_guide/engines/snakemake.md)

#### Janis
- Janis translate reads CWL, WDL and Galaxy, and writes CWL, WDL and Nextflow. It ingests source files into an internal API that models tools and workflows, then writes the target. - [Janis docs](https://janis.readthedocs.io/en/translate-docs/index.html)
- Stated limits: "in some situations it may be necessary to make manual adjustments before the output is runnable", because of "freeform Javascript (CWL), Java / Groovy (Nextflow), and Python / Cheetah (Galaxy)". - [Janis docs](https://janis.readthedocs.io/en/translate-docs/index.html)
- Built by the Portable Pipelines Project (Melbourne Bioinformatics and WEHI) (search-result excerpt; page failed with a TLS error). - [Melbourne Bioinformatics tutorial](https://www.melbournebioinformatics.org.au/tutorials/tutorials/janis_translate/janis_translate/); workshop materials 2023 - [Zenodo 8052348](https://zenodo.org/records/8052348)
- Maintenance: PyPI `janis-pipelines` 0.13.1 is the last release (2023-08-30). Main repo is labeled "[Alpha]", 44 stars, last push 2023-09-17. `janis-core` last push 2026-01-13. - [PyPI janis-pipelines](https://pypi.org/project/janis-pipelines/), [GitHub janis](https://github.com/PMCC-BioinformaticsCore/janis), [GitHub janis-core](https://github.com/PMCC-BioinformaticsCore/janis-core)

#### cwl2nxf (older, abandoned)
- Archived repo, last push 2018-08-09, described as "experimental". README: "This project is no longer under active development". Supported CWL v1.0, basic scatter, limited JS for `runtime` and `inputs`, secondaryFiles. Unsupported: subworkflows, general JS expressions, record types, most of `outputEval`/`loadContents`, imports. - [GitHub nextflow-io/cwl2nxf](https://github.com/nextflow-io/cwl2nxf)
- Seqera blog (2017-07-20, older) named the hard parts as "scatter, CWL expressions, and conditional command line inclusion". - [Seqera blog: Nextflow and CWL](https://seqera.io/blog/nextflow-and-cwl/)

#### CNT (CWL to Nextflow, academic)
- Putra, Kim, Gunawi, Grossman, IEEE BIBE 2023. Three stages: tool-level translation (CWL `CommandLineTool` -> Nextflow process), graph dependency analysis, correctness check. - [CNT paper PDF](https://ucare.cs.uchicago.edu/pdf/bibe23-cnt.pdf), [IEEE Xplore](https://ieeexplore.ieee.org/document/10431902/)
- Evaluated on the GDC DNA-Seq and RNA-Seq pipelines. Automatic translation coverage 73% and 81%. JavaScript code blocks are replaced with a placeholder "designed to trigger a syntax error", so a developer must fix them. CNT detects three common JS patterns to help. Manual translation of GDC DNA-Seq was estimated at about 160 person-hours. Nextflow ran 21-39% (DNA-Seq) and 33-72% (RNA-Seq) faster than cwltool. - [CNT paper PDF](https://ucare.cs.uchicago.edu/pdf/bibe23-cnt.pdf)

#### Snakemake `--export-cwl` and `handover`
- "every Snakemake job is encoded into a single step in the CWL workflow. Moreover, every step of that workflow calls Snakemake again to execute the job." Default container is the Snakemake image. Fails if the workflow has checkpoints. No absolute output paths. All target files land in the workdir. (Docs for Snakemake 9.27.0.) - [Snakemake interoperability docs](https://snakemake.readthedocs.io/en/stable/executing/interoperability.html)
- Snakemake can call Nextflow through a `handover` rule. The rule runs locally and the foreign system does its own submission; "After the step is done, Snakemake continues execution with the output files produced by the foreign workflow." - [Snakemake foreign WMS docs](https://snakemake.readthedocs.io/en/stable/snakefiles/foreign_wms.html)

#### WDL <-> CWL
- `wdl2cwl`: archived, "[Experimental]", PyPI 0.2.2 from 2017-02-21 (older). README says it is replaced by `wdl-cwl-translator`. - [GitHub wdl2cwl](https://github.com/common-workflow-lab/wdl2cwl), [PyPI wdl2cwl](https://pypi.org/project/wdl2cwl/)
- `wdl-cwl-translator`: WDL -> CWL v1.2. Uses miniwdl to parse WDL and `cwl_utils.parser.cwl_v1_2` objects to emit CWL. Targets OpenWDL 1.1. Stated goal: translate the Broad WARP whole-genome germline single-sample workflow and show equivalent results. Last push 2026-06-24, 28 stars. - [GitHub wdl-cwl-translator](https://github.com/common-workflow-lab/wdl-cwl-translator)
- `cwl2wdl` (adamstruck): "Proof of concept", last update 2022-10-06. - [GitHub adamstruck/cwl2wdl](https://github.com/adamstruck/cwl2wdl)
- dxCompiler (DNAnexus) keeps a CWL v1.2 -> WDL v1 mapping document. It lists the gaps: CWL JavaScript expressions, `secondaryFiles` and the rule that files from one directory must be staged together, no `Directory` type in WDL 1.0, CWL records have no global namespace (renaming needed), `nested_crossproduct`/`flat_crossproduct` scatter, `valueFrom`. - [dxCompiler CWL_v1.2.0_to_WDL_v1.md](https://github.com/dnanexus/dxCompiler/blob/main/doc/CWL_v1.2.0_to_WDL_v1.md)

#### Galaxy
- `gxformat2` 0.27.0 on PyPI (2026-05-27). CLI includes `gxwf-lint`, `gxwf-abstract-export` (abstract CWL), `gxwf-to-native`, `gxwf-to-format2`. - [PyPI gxformat2](https://pypi.org/project/gxformat2/), [GitHub gxformat2](https://github.com/galaxyproject/gxformat2)
- `workflowhub-eu/galaxy2cwl` produces CWL descriptions ("initially an abstract cwl interface") of Galaxy workflows and executions, updated 2026-09-16. `common-workflow-lab/galaxy2cwl` converts Galaxy tool XML to a CWL CommandLineTool (updated 2025-02-13). - [GitHub workflowhub-eu/galaxy2cwl](https://github.com/workflowhub-eu/galaxy2cwl), [GitHub common-workflow-lab/galaxy2cwl](https://github.com/common-workflow-lab/galaxy2cwl)
- Galaxy Workflow Foundry (created 2026-04-29): LLM-agent conversion "pipelines" including `nextflow-to-galaxy`, `cwl-to-galaxy`, `paper-to-galaxy`, `paper-to-cwl`, `nextflow-to-cwl`. It says LLMs "fail the same boring, detectable ways every time: hallucinated tool IDs, dropped `+galaxyN` revisions, fabricated parameter names, `gxformat2` the parser rejects on line one". Each step is checked with the `gxwf` static validator. - [GitHub galaxyproject/foundry](https://github.com/galaxyproject/foundry), [PR #600 CWL to Galaxy, merged 2026-09-22](https://github.com/galaxyproject/foundry/pull/600)

#### LLM-based and other converters
- Seqera AI (blog 2025-01-08): converts "workflow scripts (e.g. WDL, CWL, bash)" to Nextflow DSL2. Offers sandbox testing and nf-test generation. No accuracy numbers published. - [Seqera blog](https://seqera.io/blog/legacy-scripts-to-nextflow-seqera-ai/)
- BioUML WDL -> Nextflow translator (Kiselev, Ryabova, Kolpakov, F1000Research 2026, 15:138, "2 approved with reservations"). IR is an object-oriented model based on the BioUML meta-model. Authors: "it is possible that there are cases where incorrect Nextflow will be generated". They describe Seqera AI as based on Claude and write "Sometimes LLM hallucinates and produces scripts with errors". - [F1000Research 15-138](https://f1000research.com/articles/15-138), [GitHub genespace-ru/workflow-engine](https://github.com/genespace-ru/workflow-engine)
- Stephen Turner, Zenodo "brief idea" (2024-10-17): Snakemake -> Nextflow with nf-core modules using a fine-tuned LLM. Only metadata was readable. - [Zenodo 13942979](https://zenodo.org/records/13942979)
- `nf-core-pipeline-to-arc` (nfdi4plants): reads `nextflow_schema.json` and builds one CWL tool wrapper for the whole pipeline (needs `nextflow` and `cwltool` at run time). "WORK IN PROGRESS", last push 2022-05-09. - [GitHub nf-core-pipeline-to-arc](https://github.com/nfdi4plants/nf-core-pipeline-to-arc)
- OxyMake `ox-translate` (Rust): a Snakemake parser and a WDL parser both produce one IR; generators emit Oxymakefile TOML, Snakemake or WDL; "Structured escalation tracking for constructs requiring manual review". - [ox-translate lib.rs](https://github.com/noogram/oxymake/blob/main/crates/ox-translate/src/lib.rs)
- OxyMake Snakemake parser: these constructs "cannot be losslessly translated" and "must fail loudly unless the operator explicitly opts in via `--lossy`": `checkpoint`, `module`, `pepfile`, `localrules`, `ruleorder`, `onsuccess`, `onerror`, `onstart`. `expand()` is handled with regex tiers (literal lists, `config["key"]`). - [ox-translate snakemake/parser.rs](https://github.com/noogram/oxymake/blob/main/crates/ox-translate/src/snakemake/parser.rs)
- WfCommons `wfbench` translators generate runnable benchmark workflows from WfFormat for Airflow, Bash, CWL, Dask, Makeflow, Nextflow, Parsl, Pegasus, PyCOMPSs, Snakemake, StreamFlow, Swift/T and TaskVine. WfCommons 1.5 (PyPI 2026-07-27). - [WfCommons translators](https://github.com/wfcommons/WfCommons/tree/main/wfcommons/wfbench/translator), [PyPI wfcommons](https://pypi.org/project/wfcommons/)

### Inferences
- Two families exist. (a) "Run the engine, then export the concrete jobs": wf2wf's Snakemake importer, `snakemake --export-cwl`, Latch's Snakemake JIT (section 2). (b) "Parse the source statically": Janis, CNT, cwl2nxf, OxyMake, wf2wf's Nextflow importer. Family (a) cannot be used by iunctus because iunctus must not shell out. Family (b) always stops at embedded code and dynamic constructs.
- Outputs that still call the original engine inside each step (`--export-cwl`, `nf-core-pipeline-to-arc`) are wrappers, not translations.
- Regex parsing of Nextflow (wf2wf) or of Snakemake parts (OxyMake `expand()`) is a sign of how hard these languages are to parse without their host language. A real grammar is needed.
- wf2wf's IR is a static task DAG with task-level edges only. It has no ports, channels or runtime graph growth. That design cannot represent Nextflow dataflow or Snakemake checkpoints, which matches its own "partial" and "unsupported" rows.
- Converters that survive are backed by a funded platform (dxCompiler) or a standards group (wdl-cwl-translator). One-person converters go stale within one to two years (cwl2nxf, wdl2cwl, cwl2wdl, nf-core-pipeline-to-arc, wf2wf).
- LLM conversion is now the most active area. It always needs a deterministic validator (Galaxy Foundry uses `gxwf`, Seqera AI uses a sandbox and nf-test).

### Gaps
- No public, reproducible coverage benchmark for wf2wf, Janis or Seqera AI on nf-core or the Snakemake workflow catalog.
- No non-LLM converter from real Nextflow DSL2 pipelines to CWL or WDL was found. Only whole-pipeline wrapping exists.
- The Janis peer-reviewed paper was not located. The Melbourne tutorial page could not be opened (TLS error).
- Details of the Turner fine-tuned-LLM approach were not readable.

## 2. Multi-language engines and platforms: shared model or wrappers?

### Takeaway
Only a few systems ever had a real shared IR across languages: Cromwell's WOM (WDL and CWL, CWL dropped in 2022), DNAnexus dxCompiler's IR (WDL and CWL, shaped like the target platform), and StreamFlow's internal model (CWL only in practice). No platform has a shared IR that covers Nextflow or Snakemake. Every platform that supports Nextflow runs the Nextflow JVM engine. Every platform that supports Snakemake runs Snakemake, at least to compute the job graph. The dominant pattern is wrapping native engines behind a common API (GA4GH WES, a `run.sh`, or a platform job API).

### Cited Findings
- **Cromwell (WOM).** WOM is "a directed acyclic graph that captures workflow inputs, outputs, calls, and the dependencies between them". - [Cromwell: What is WOM?](https://cromwell.readthedocs.io/en/latest/developers/bitesize/workflowParsing/whatIsWom/)
- Ahmed et al. 2021: "The ability to translate a workflow language into intermediate representation, e.g., the WOM representation of CWL and WDL code in Cromwell, enables stitching meta-workflows written in more than one language." - [Ahmed et al. 2021, PMC](https://pmc.ncbi.nlm.nih.gov/articles/PMC8569008/)
- Cromwell 79 (2022-05-17) was "the last release with CWL"; support removed in Cromwell 80 (2022-06-10); Cromwell 85 (2023-02-16) removed the `cwl` artifact. "CWL will be re-introduced at a later date in the Terra platform, using a solution other than Cromwell." Cromwell 92 (latest GitHub release, 2026-01-30) "now fully supports WDL 1.1". The CHANGELOG already has a section for 93. - [Cromwell CHANGELOG](https://github.com/broadinstitute/cromwell/blob/develop/CHANGELOG.md), [Cromwell releases](https://github.com/broadinstitute/cromwell/releases)
- **Terra** (blog 2022-05-03, older): "CWL support in Cromwell will enter a deprecation phase starting May 2022". For Nextflow and CWL, Terra planned an "app model" that runs existing community tools instead of adding languages to Cromwell. - [Terra blog](https://terra.bio/terras-roadmap-to-supporting-more-workflow-languages/)
- **Toil** 9.5.0 (2026-06-02). "toil-wdl-runner works by using MiniWDL to parse and interpret the WDL workflow"; WDL 1.0 or later; draft-2 last supported in 5.12.0. - [Toil WDL docs](https://toil.readthedocs.io/en/master/wdl/running.html), [Toil releases](https://github.com/DataBiosphere/toil)
- Toil's CWL runner imports cwltool internals (`cwltool.builder`, `command_line_tool`, `job`, `load_tool`, `main`), and the WDL runner imports MiniWDL (`WDL.Lint`, `WDL.runtime.config`, container backends). - [cwltoil.py](https://github.com/DataBiosphere/toil/blob/master/src/toil/cwl/cwltoil.py), [wdltoil.py](https://github.com/DataBiosphere/toil/blob/master/src/toil/wdl/wdltoil.py)
- Ahmed 2021 (older state): Toil then used the Hermes parser generator and translated WDL tasks to Python functions, while Cromwell translated them into Bash. - [Ahmed et al. 2021, PMC](https://pmc.ncbi.nlm.nih.gov/articles/PMC8569008/)
- **REANA** supports CWL, Serial, Yadage and Snakemake; no Nextflow or WDL listed. - [REANA supported systems](https://docs.reana.io/running-workflows/supported-systems/)
- REANA's Snakemake engine subclasses Snakemake's executor plugin `RemoteExecutor`, so real Snakemake builds the DAG and REANA only submits jobs. REANA's CWL engine imports cwltool classes. `reana-client` 0.9.6 (2026-03-25); `reana` 0.95.0-alpha.5 (2026-06-07). - [reana-workflow-engine-snakemake](https://github.com/reanahub/reana-workflow-engine-snakemake), [reana-workflow-engine-cwl](https://github.com/reanahub/reana-workflow-engine-cwl), [PyPI reana-client](https://pypi.org/project/reana-client/), [GitHub reana](https://github.com/reanahub/reana)
- **Sapporo** 2.2.6 (2026-08-11): builds on GA4GH WES 1.1.0. Engines: cwltool, Nextflow, Toil, Cromwell, Snakemake, ep3, StreamFlow. "Each workflow engine runs inside its own Docker container". Architecture uses a `run.sh` abstraction. Generates RO-Crate metadata. - [GitHub sapporo-service](https://github.com/sapporo-wes/sapporo-service), [PyPI sapporo](https://pypi.org/project/sapporo/)
- **WfExS-backend** 1.0.10 (2026-09-07): fetches workflows from WorkflowHub (TRS) or RO-Crates, sets up Nextflow or cwltool, and describes results as RO-Crate. Snakemake onboarding is "progress towards" integration (poster). - [GitHub WfExS-backend](https://github.com/inab/WfExS-backend)
- **Arvados**: "CWL is the primary way to develop and run workflows", through `arvados-cwl-runner`, CWL v1.0 to v1.2 (search-result excerpt of the doc page). - [Arvados cwl-runner docs](https://doc.arvados.org/user/cwl/cwl-runner.html)
- **StreamFlow**: "a container-native Workflow Management System written in Python 3 and based on the Common Workflow Language (CWL) standard". The code has its own generic model (`streamflow/workflow/`: port, step, token, combinator, transformer, executor) and a CWL translator into it (`streamflow/cwl/translator.py`). PyPI shows 0.1.6 (2023-04-21, older); GitHub tags reach 0.2.0rc3; repo pushed 2026-09-25. - [StreamFlow docs](https://streamflow.di.unito.it/documentation/latest/), [GitHub streamflow](https://github.com/alpha-unito/streamflow), [PyPI streamflow](https://pypi.org/project/streamflow/); paper - [Colonnelli et al., IEEE TETC 2021](https://doi.org/10.1109/TETC.2020.3019202)
- **dxCompiler** 2.18.0 (2026-08-05): compiles WDL (draft-2, 1.0, 1.1) and CWL 1.2 to DNAnexus. "The language-specific AST is translated into dxCompiler 'Intermediate Representation' (IR), which mirrors the structure of DNAnexus applets/workflows". "An IR application has the language-specific source code attached to it because this is used to execute the task/tool at runtime." Workflow blocks that need runtime evaluation (scatter, conditionals, expressions, CWL `valueFrom`) become "fragment" helper applets. - [dxCompiler Internals.md](https://github.com/dnanexus/dxCompiler/blob/main/doc/Internals.md), [GitHub dxCompiler](https://github.com/dnanexus/dxCompiler)
- **Latch, Nextflow**: Latch runs the real Nextflow engine "as a subprocess", with "a patched version of the Nextflow Kubernetes plugin" and a shared filesystem ("Nextflow EFS"). - [Latch Nextflow overview](https://wiki.latch.bio/workflows/sdk/nextflow/overview)
- **Latch, Snakemake**: "JIT (Just-In-Time) registration. This means that once the single-task workflow above is complete, it will produce a second workflow, which runs the actual Snakemake jobs." - [Latch Snakemake tutorial](https://wiki.latch.bio/workflows/sdk/snakemake/tutorial). The JIT step replaces input, output and params function calls with pre-computed static strings (search-result excerpt). - [Latch Snakemake cloud compatibility](https://wiki.latch.bio/workflows/sdk/snakemake/cloud)
- **Dockstore** 1.20.0 (2026-07-28) is a registry, not an engine. Descriptor types: CWL, WDL, NFL (Nextflow), GALAXY, SMK (Snakemake) (search-result excerpt of the `.dockstore.yml` template docs). - [Dockstore .dockstore.yml docs](https://docs.dockstore.org/en/stable/assets/templates/workflows/workflows.html), [GitHub dockstore](https://github.com/dockstore/dockstore)
- **Galaxy** has its own native model. CWLcon 2022 (Marius van den Beek): "Galaxy's workflow format predates CWL and has never been formalized." CWL support code exists in Galaxy (`lib/galaxy/tool_util/cwl/`), developed for years in a fork. - [CWLcon 2022 talk](https://cwl.discourse.group/t/cwl-support-in-galaxy-challenges-design-choices-and-progress/553), [Galaxy tool_util/cwl](https://github.com/galaxyproject/galaxy/tree/dev/lib/galaxy/tool_util/cwl), [fork PR #47](https://github.com/common-workflow-lab/galaxy/pull/47)
- **Hi-WAY** (older, last push 2017-10-31): ran Cuneiform, Galaxy and Pegasus DAX workflows on Hadoop YARN. - [GitHub Hi-WAY](https://github.com/marcbux/Hi-WAY)

### Inferences
- Wrapping wins in practice because it gives full language fidelity at low cost. The price is one engine runtime per language, no shared scheduling or caching across languages, and no single provenance model (RO-Crate is bolted on afterwards).
- Cromwell is the key cautionary case: a real shared IR for two languages existed, and the team still dropped one language to focus resources. Maintaining two front ends with full conformance was the burden, not the IR itself.
- dxCompiler shows a working hybrid: the IR holds structure, types and resources, but expression evaluation stays in the original language at run time. iunctus can copy this idea without shelling out, if it embeds its own evaluators.
- Latch's Snakemake JIT and Snakemake `--export-cwl` show the Snakemake job graph is only reliably known after Snakemake's Python runs. Any tool that wants Snakemake without Snakemake must evaluate that Python itself.
- Seqera Platform is, as far as I could tell, Nextflow-only (not verified against Seqera docs; see Gaps).

### Gaps
- Terra's actual 2026 language support (did CWL or Nextflow apps ship?) was not verified.
- Arvados WDL support was not verified.
- Galaxy mainline CWL maturity in 2026 (usable in production or not) was not verified.
- Seqera Platform language support was not checked in primary docs.
- WESkit (WES for Snakemake and Nextflow) is hosted outside GitHub and was not checked.

## 3. Academic literature on interoperability, unified representations and semantic comparisons

### Takeaway
The literature compares engines by features and ergonomics, and it repeatedly asks for interoperability, but it contains no unified IR that spans Nextflow, Snakemake and CWL. The only formal workflow IR (SWIRL, 2024) is low-level, distribution-focused, and today has only a Pegasus DAX front end. Community summits (2021 to 2025) say plainly that systems with different abstractions do not translate well. LLM papers from 2023 to 2026 show LLMs help with generation but struggle with workflow semantics and need validators.

### Cited Findings
- Wratten, Wilm, Göke, "Reproducible, scalable, and shareable analysis pipelines with bioinformatics workflow managers", Nature Methods 2021. Perspective comparing workflow managers and community pipeline collections. - [DOI 10.1038/s41592-021-01254-9](https://doi.org/10.1038/s41592-021-01254-9)
- Ahmed et al., "Design considerations for workflow management systems use in production genomics research and the clinic", Scientific Reports 2021. Compared Nextflow, CWL, WDL (plus executors) and Swift/T on a variant-calling pipeline and a scalability test, locally, on HPC and in the cloud. Snakemake and Galaxy were excluded ("GA4GH TES support was only added to Snakemake in their November 2020 release"). - [DOI 10.1038/s41598-021-99288-8](https://doi.org/10.1038/s41598-021-99288-8), [PMC full text](https://pmc.ncbi.nlm.nih.gov/articles/PMC8569008/)
- Ahmed 2021 quotes: "It is quite possible that in the future some kind of a hybrid workflow coding paradigm might emerge". "Therefore, conformance tests and backwards compatibility between a workflow language and an execution engine are critical." Early interoperability efforts named: IWIR and SHIWA. - [PMC full text](https://pmc.ncbi.nlm.nih.gov/articles/PMC8569008/)
- Leipzig, "A review of bioinformatic pipeline frameworks", Briefings in Bioinformatics 2017 (older): frameworks differ on "implicit or explicit syntax", "configuration, convention or class-based design", and command line vs workbench. - [DOI 10.1093/bib/bbw020](https://doi.org/10.1093/bib/bbw020)
- Jackson, Kavoussanakis, Wallace, "Using prototyping to choose a bioinformatics workflow management system", PLOS Comp Biol 2021: re-implemented the RiboViz workflow as prototypes (Ahmed 2021 says they evaluated Snakemake, CWL+cwltool, CWL+Toil and Nextflow). - [DOI 10.1371/journal.pcbi.1008622](https://doi.org/10.1371/journal.pcbi.1008622)
- Djaffardjy et al., "Developing and reusing bioinformatics data analysis pipelines using scientific workflow systems", CSBJ 2023: requirements review plus an empirical study of workflow reuse on a large workflow collection. - [DOI 10.1016/j.csbj.2023.03.003](https://doi.org/10.1016/j.csbj.2023.03.003)
- Mölder et al., "Sustainable data analysis with Snakemake", F1000Research (version 3, 2025). - [DOI 10.12688/f1000research.29032.3](https://doi.org/10.12688/f1000research.29032.3)
- Goble et al., "FAIR Computational Workflows", Data Intelligence 2020. - [DOI 10.1162/dint_a_00033](https://doi.org/10.1162/dint_a_00033)
- Wilkinson et al., "Applying the FAIR Principles to computational workflows", Scientific Data 2025: "the description should be enough to be translated for another workflow system"; "Standard language formats like CWL encourage interoperability and offer the potential for workflow comparisons and exchange between different languages through documentation of components, inputs, and outputs." - [DOI 10.1038/s41597-025-04451-9](https://doi.org/10.1038/s41597-025-04451-9)
- Soiland-Reyes et al., "Making Canonical Workflow Building Blocks Interoperable across Workflow Languages", Data Intelligence 2022: interoperability at the tool-wrapper level (BioExcel Building Blocks), one workflow run with 5 different workflow systems. - [DOI 10.1162/dint_a_00135](https://doi.org/10.1162/dint_a_00135)
- Workflows Community Summit, Jan 2021: "there are now hundreds of independent WMSs, many of which are moribund". The report asks at which layer to interoperate (workflow description, task description, task submission, data passing) and says interoperability "may occur via shim layers or intermediate representations, in a similar manner to compiling to a high level language". - [arXiv 2103.09181](https://arxiv.org/abs/2103.09181)
- Workflows Community Summit, technical, 2021: "Systems that use the same underlying abstractions can likely be translated between, which is not the case for systems that use different abstractions." It recommends "common APIs ... so as interoperability could be achieved at the component level (in contrast to interoperability at the workflow specification level as targeted, for instance, by CWL)". - [arXiv 2106.05177](https://arxiv.org/abs/2106.05177)
- Later summit reports: facilities integration (2022) - [arXiv 2201.07435](https://arxiv.org/abs/2201.07435); "A Roadmap Revolution" (2022 summit, posted 2023-03-31) - [arXiv 2304.00019](https://arxiv.org/abs/2304.00019); 2024 summit, 111 participants from 18 countries, six themes including FAIR computational workflows - [arXiv 2410.14943](https://arxiv.org/abs/2410.14943); 2025 Amsterdam summit (posted 2026-02-04): barriers include "tensions between system generality and domain-specific utility" and "gaps in standardization"; proposes "formalizing workflow patterns and community-driven benchmarks". - [arXiv 2602.05131](https://arxiv.org/abs/2602.05131)
- Suter et al., "A Terminology for Scientific Workflow Systems" (2025): five axes (workflow characteristics, composition, orchestration, data management, metadata capture), classifies 23 WMSs. - [arXiv 2506.07838](https://arxiv.org/abs/2506.07838)
- Coleman et al., "WfCommons: A framework for enabling scientific workflow research and development", FGCS (online 2021). - [DOI 10.1016/j.future.2021.09.043](https://doi.org/10.1016/j.future.2021.09.043)
- SWIRL: Colonnelli, Medić, Mulone, Bono, Padovani, Aldinucci, "Introducing SWIRL: An Intermediate Representation Language for Scientific Workflows", FM 2024, LNCS 14933, pp. 226-244. A low-level compilation target using send/receive primitives on a location-aware graph, with formal semantics and rewriting rules with equivalence proofs. - [arXiv 2407.01838](https://arxiv.org/abs/2407.01838)
- SWIRL compiler `swirlc` (Python, ANTLR): `translate` and `compile` commands; the documented front end is Pegasus DAX (`swirlc translate --language dax`). PyPI 0.0.1 (2024-06-24); repo pushed 2026-09-21. - [GitHub swirlc](https://github.com/alpha-unito/swirlc)
- Fernando, Creager, Simpson, "Towards Build-Time Interoperability of Workflow Definition Languages", SYNASC 2007 (older). - [DOI 10.1109/SYNASC.2007.18](https://doi.org/10.1109/SYNASC.2007.18)
- OxyMake paper (Sérié, arXiv, June 2026, revised Sept 2026): "Snakemake rules embed Python, so the full job graph is known only after the workflow file is executed." Result identity is "a pure function of what its rule declares: command text, inputs, parameters, environment." "The guarantees reach exactly as far as the workflow declares, and no further." - [arXiv 2606.20989](https://arxiv.org/abs/2606.20989)

#### LLM-based workflow work (2023 to 2026)
- Alam and Roy, "From Prompt to Pipeline" (2025): GPT-4o, Gemini 2.5 Flash, DeepSeek-V3 generating Galaxy and Nextflow workflows, compared with Galaxy Training Network and nf-core baselines. Gemini best for Galaxy, DeepSeek-V3 best for Nextflow; prompting strategy mattered. - [arXiv 2507.20122](https://arxiv.org/abs/2507.20122)
- Yildiz and Peterka, "Do Large Language Models Speak Scientific Workflows?" (2024, revised 2025-10-09): "LLMs often struggle with workflow related tasks due to their lack of knowledge of scientific workflows". Their translation test was between HPC I/O systems (Henson and ADIOS), not bio languages: best score 5/5 for Henson to ADIOS, at most 3/5 the other way. - [arXiv 2412.10606](https://arxiv.org/abs/2412.10606)
- Masera, Leone, Köster, Molineris, "Snakemaker" (2025): generative AI turns terminal history and notebooks into Snakemake workflows. - [arXiv 2505.02841](https://arxiv.org/abs/2505.02841)
- Duque et al. (2023): LLM function calling to build and run workflows (Phyloflow). - [arXiv 2312.07711](https://arxiv.org/abs/2312.07711)
- Alam et al. (2024, revised 2025): Stack Overflow and GitHub study of workflow system development; workflow execution is the hardest topic on Stack Overflow, data structures and operations on GitHub. - [arXiv 2411.10890](https://arxiv.org/abs/2411.10890)

### Inferences
- The summit statement about "same underlying abstractions" is the key theory for iunctus. Nextflow (push dataflow over channels), Snakemake (pull, backward chaining from target files) and CWL (static step graph with scatter and `when`) use different abstractions. A unified IR must therefore hold both a dataflow runtime and a demand-driven planner, or lower both into a shared runtime task layer. It cannot be a simple static DAG.
- I would classify Snakemake as Leipzig's "implicit" (make-like) style and Nextflow and CWL as "explicit". That is my reading, not a quote.
- SWIRL is the most formal IR, but it sits below the level iunctus needs (it models location and communication, not language semantics). It could inspire a later lowering stage.
- No paper benchmarks translation fidelity across Nextflow, Snakemake and CWL on the same pipelines.

### Gaps
- I found no formal (operational semantics) comparison of Nextflow, Snakemake and CWL.
- I found no paper on a single IR that covers all three of Nextflow, Snakemake and CWL.
- Full text of the Wratten 2021 paper was not read (paywall); only the abstract was used.

## 4. Workflow description and provenance standards

### Takeaway
There is no executable cross-language standard. CWL can act as an abstract interface description (signatures only), RO-Crate packages workflows and their runs, WfFormat records concrete task graphs and traces, and Bioschemas gives web metadata. These are good output formats for iunctus (provenance, instantiated graph), not input IRs.

### Cited Findings
- **WfFormat** v1.6 (released 2026-08-05): JSON Schema draft 2020-12. Core is `workflow.specification` (tasks with `id`, `parents`, `children`, and files); `workflow.execution` is optional. "Unknown properties are permitted". `schemaVersion` is a closed enum (only "1.6"). 1.6 adds optional `metrics`. - [GitHub WfFormat](https://github.com/wfcommons/WfFormat)
- WfCommons parses execution logs of Makeflow, Nextflow, Pegasus, RO-Crate, Snakemake, StreamFlow and TaskVine into WfFormat instances. - [WfCommons log parsers](https://github.com/wfcommons/WfCommons/tree/main/wfcommons/wfinstances/logs)
- **Workflow RO-Crate** profile 1.1.0: the crate "MUST refer to the Main Workflow via `mainEntity`"; its types "MUST include `File`, `SoftwareSourceCode` and `ComputationalWorkflow`"; it "MUST refer to its type via `programmingLanguage`". An optional "Main Workflow CWL Description" (types File, SoftwareSourceCode, HowTo) is linked with `subjectOf`. WorkflowHub extracts metadata from CWL, Galaxy, KNIME, Nextflow and Snakemake. - [Workflow RO-Crate profile](https://about.workflowhub.eu/Workflow-RO-Crate/)
- **RO-Crate** specification 1.3.0 released 2026-06-22. - [GitHub ro-crate](https://github.com/ResearchObject/ro-crate)
- **Workflow Run RO-Crate**: 0.6.0 released 2026-09-07. It captures workflow run provenance "at different levels of granularity" and aligns with W3C PROV. Paper: Leo et al., PLOS ONE 19(9), 2024. - [workflow-run-crate releases](https://github.com/ResearchObject/workflow-run-crate), [arXiv 2312.07852](https://arxiv.org/abs/2312.07852)
- Implementations listed: Galaxy (v23.1.1+), runcrate (0.5.0+), COMPSs (3.4+), StreamFlow (0.2.0.dev10), Nextflow via nf-prov (1.4.0+), WfExS (0.10.1+), Sapporo (1.5.1+), Autosubmit (4.0.100+). - [Workflow Run RO-Crate site](https://www.researchobject.org/workflow-run-crate/)
- Wilkinson 2025 shows CWLProv output converted to an RO-Crate "that follows the Provenance Run Crate profile". - [DOI 10.1038/s41597-025-04451-9](https://doi.org/10.1038/s41597-025-04451-9)
- **Bioschemas ComputationalWorkflow**: the profile page shows version 1.1-DRAFT. - [Bioschemas ComputationalWorkflow](https://bioschemas.org/profiles/ComputationalWorkflow)
- **CWL** v1.2.1 is the current released spec (2024-01-03). CWL v1.2 defines `Operation`: "an abstract operation. It is a potential step of a workflow that has not yet been bound to a concrete implementation. It specifies an input and output signature, but does not provide enough information to be executed." - [CWL v1.2 Workflow spec](https://www.commonwl.org/v1.2/Workflow.html), [GitHub cwl-v1.2](https://github.com/common-workflow-language/cwl-v1.2)
- Abstract CWL producers: `gxwf-abstract-export` in gxformat2, and WorkflowHub's `galaxy2cwl`. - [GitHub gxformat2](https://github.com/galaxyproject/gxformat2), [GitHub galaxy2cwl](https://github.com/workflowhub-eu/galaxy2cwl)
- Current reference versions: cwltool 3.3.20260925135507 (2026-09-25), cwl-utils 0.45 (2026-09-26), WDL spec v1.3.0 (2026-01-12). - [PyPI cwltool](https://pypi.org/project/cwltool/), [PyPI cwl-utils](https://pypi.org/project/cwl-utils/), [GitHub openwdl/wdl](https://github.com/openwdl/wdl)

### Inferences
- "Abstract CWL" is a signature-level description. It is useful for registries and for comparing workflows, but it cannot carry Nextflow or Snakemake execution semantics.
- WfFormat is close to the shape of iunctus's instantiated task graph (tasks, parents, children, files, runtimes). iunctus could export it for benchmarking and research use.
- Workflow Run RO-Crate is the obvious provenance output. nf-prov and Sapporo already emit it, so users of those engines would expect it.

### Gaps
- Release status of Bioschemas ComputationalWorkflow 1.0 vs the 1.1 draft was not checked.
- The exact list of the three Workflow Run Crate profiles (Process Run Crate, Workflow Run Crate, Provenance Run Crate) was not confirmed on the page I opened; only "Provenance Run Crate" is confirmed by Wilkinson 2025.
- GA4GH TES and WES versions are out of scope here and were not researched.

## 5. Why unification and translation attempts stayed partial or failed

### Takeaway
The same reasons appear in every project: embedded host-language code, dynamic graphs that exist only at run time, different execution models (push dataflow vs pull from targets), mismatched type and file semantics, rich configuration systems, module and import systems, moving language targets, and small teams that stop maintaining the tool.

### Cited Findings
- Embedded code: Janis names "freeform Javascript (CWL), Java / Groovy (Nextflow), and Python / Cheetah (Galaxy)" as the reason for manual fixes. - [Janis docs](https://janis.readthedocs.io/en/translate-docs/index.html). CNT replaces JS blocks with placeholders that break the build on purpose. - [CNT PDF](https://ucare.cs.uchicago.edu/pdf/bibe23-cnt.pdf). cwl2nxf did not implement JS expressions. - [cwl2nxf](https://github.com/nextflow-io/cwl2nxf)
- Dynamic graph: Snakemake's graph "is known only after the workflow file is executed". - [OxyMake paper](https://arxiv.org/abs/2606.20989). `--export-cwl` fails with checkpoints. - [Snakemake docs](https://snakemake.readthedocs.io/en/stable/executing/interoperability.html). wf2wf: checkpoints "static graph only", dynamic rules unsupported, Nextflow dynamic operators "limited". - [wf2wf Snakemake](https://github.com/csmcal/wf2wf/blob/main/docs/user_guide/engines/snakemake.md), [wf2wf Nextflow](https://github.com/csmcal/wf2wf/blob/main/docs/user_guide/engines/nextflow.md)
- Dataflow modelling: CNT must build "a proper dataflow model" with Nextflow operators and track data lineage to pick the right channel factory (for example `Channel.path()` vs `Channel.of()`). - [CNT PDF](https://ucare.cs.uchicago.edu/pdf/bibe23-cnt.pdf)
- Type and file semantics: `secondaryFiles` co-location, no `Directory` in WDL 1.0, record naming, scatter cross products, `valueFrom`. - [dxCompiler mapping doc](https://github.com/dnanexus/dxCompiler/blob/main/doc/CWL_v1.2.0_to_WDL_v1.md). CNT notes needed information is "scattered across all CWL files". - [CNT PDF](https://ucare.cs.uchicago.edu/pdf/bibe23-cnt.pdf)
- Config and params: wf2wf Nextflow params handling is still "roadmap", and its config parser is regex over `withName:` blocks. - [wf2wf Nextflow](https://github.com/csmcal/wf2wf/blob/main/docs/user_guide/engines/nextflow.md), [importer](https://github.com/csmcal/wf2wf/blob/main/wf2wf/importers/nextflow.py). The CERIT-SC `nextflow-go` launcher exists because `kuberun` could not handle `nextflow.config` files with functions or multi-line strings. - [GitHub nextflow-go](https://github.com/CERIT-SC/nextflow-go)
- Modules, imports, hooks: OxyMake refuses `module`, `ruleorder`, `localrules`, `onstart`/`onsuccess`/`onerror` without `--lossy`. wf2wf does not support Snakemake modules and only partly supports DSL2 modules. cwl2nxf did not support subworkflows or imports. - [OxyMake parser](https://github.com/noogram/oxymake/blob/main/crates/ox-translate/src/snakemake/parser.rs), [wf2wf](https://github.com/csmcal/wf2wf/blob/main/docs/user_guide/engines/snakemake.md), [cwl2nxf](https://github.com/nextflow-io/cwl2nxf)
- Moving targets: Nextflow strict syntax arrived in 25.04 and is the default from 26.04 (turn off with `NXF_SYNTAX_PARSER=v1`); "Starting in version 25.10, new language features require the strict parser". - [Nextflow strict syntax docs](https://docs.seqera.io/nextflow/strict-syntax)
- Maintenance and focus: Cromwell dropped CWL in 2022 to focus on WDL. - [Cromwell CHANGELOG](https://github.com/broadinstitute/cromwell/blob/develop/CHANGELOG.md), [Terra blog](https://terra.bio/terras-roadmap-to-supporting-more-workflow-languages/). Many converters are archived or stale (cwl2nxf 2018, wdl2cwl 2021, cwl2wdl 2022, nf-core-pipeline-to-arc 2022, wf2wf 2025). - GitHub repo pages linked in section 1.
- Different abstractions do not translate. - [arXiv 2106.05177](https://arxiv.org/abs/2106.05177)
- LLM failure modes: hallucinated tool IDs and parameter names, invalid output. - [Galaxy Foundry README](https://github.com/galaxyproject/foundry); "Sometimes LLM hallucinates". - [F1000Research 15-138](https://f1000research.com/articles/15-138)

### Inferences
- The failures were mostly in translating to another language's source code. Translation forces every source construct to have an equivalent target construct. Execution does not have that constraint: an engine only needs to run the construct, not express it in another language. This is the main reason iunctus's "execute, don't translate" approach avoids part of the history of failure.
- The part that remains hard for iunctus is the same part that broke translators: evaluating embedded Groovy, Python and JavaScript with the original semantics.
- Nextflow's strict syntax lowers the Groovy barrier a lot (no imports, no classes, no loops, no mixed top-level statements). Snakemake has no comparable restriction, so the Python barrier remains.

### Gaps
- No study measures how often real pipelines use dynamic features (Snakemake checkpoints and input functions, Nextflow custom Groovy or exotic operators). This would size the risk for iunctus directly.

## 6. Re-implementations of Nextflow or Snakemake in other languages

### Takeaway
I found no complete re-implementation of Nextflow outside the JVM, in any language. For Snakemake, there is one partial Rust engine with its own format and a subset translator (OxyMake), and one Rust parser that compiles Snakefiles to Python (snakemake-lang). Rust-native engines already exist for CWL (commonwl) and WDL (Sprocket), both with GA4GH TES backends, and both built on St. Jude's `crankshaft` execution framework. These are the closest relatives of iunctus.

### Cited Findings

#### Nextflow
- Official grammar: the Nextflow repo contains the `nf-lang` module (ANTLR grammar), used by the language server and `nextflow lint`. - [nf-lang module](https://github.com/nextflow-io/nextflow/tree/master/modules/nf-lang), [Nextflow strict syntax docs](https://docs.seqera.io/nextflow/strict-syntax)
- `nextflow-io/tree-sitter-nextflow` "Mirrors the official Nextflow ANTLR grammar". It has Rust, Python and C bindings (built from source) and an ast-grep setup. Last push 2026-09-24. - [GitHub tree-sitter-nextflow](https://github.com/nextflow-io/tree-sitter-nextflow)
- Nextflow language server (Java), last push 2026-09-24. - [GitHub nextflow-io/language-server](https://github.com/nextflow-io/language-server)
- RefTrace (Go, ANTLR Groovy grammar, Python API) lints Nextflow. Limits: "only processes are handled"; "The parser is not perfect. It doesn't seek to handle all of Groovy". Last push 2025-01-23. - [GitHub RefTrace](https://github.com/RefTrace/RefTrace)
- `CERIT-SC/nextflow-go` is only a Go Kubernetes launcher for the real Nextflow, not a re-implementation. - [GitHub nextflow-go](https://github.com/CERIT-SC/nextflow-go)
- nf-core strict-syntax health (2026-09-27, Nextflow 26.09.1-edge): 144 pipelines, 0 parse errors, 1228 errors, 5946 warnings; 96 pipelines (66.7%) have zero errors. "Fixing all errors from `nextflow lint` will be a requirement by early spring 2026." - [GitHub nf-core/strict-syntax-health](https://github.com/nf-core/strict-syntax-health)
- Current Nextflow stable release v26.04.6 (2026-07-09). - [GitHub nextflow releases](https://github.com/nextflow-io/nextflow/releases)

#### Snakemake
- OxyMake (Rust, Apache-2.0, created 2026-06-15, v0.5.0 on 2026-09-17, 5 stars, "Best-effort single-maintainer project"). Own TOML format (`Oxymakefile.toml`). `ox translate Snakefile` reads existing Snakefiles; `ox export snakemake` writes them back. Executors: local, SLURM, Ray; Kubernetes "designed but not yet implemented". The README says cold end-to-end runs are slower than Snakemake. - [GitHub oxymake](https://github.com/noogram/oxymake), [arXiv 2606.20989](https://arxiv.org/abs/2606.20989)
- `cademirch/snakemake-lang` (Rust, created 2026-05-19, 0 stars): "Parser, AST, and compiler for the Snakemake workflow language", with optional CLI, JSON output and PyO3 bindings. `compile` turns `.smk` into "virtual Python" (decorator form like `@workflow.rule(...)`), `parse` emits the AST as JSON. - [GitHub snakemake-lang](https://github.com/cademirch/snakemake-lang)
- Current Snakemake 9.27.0 (PyPI 2026-09-11). - [PyPI snakemake](https://pypi.org/project/snakemake/)

#### Rust-native engines for other bio workflow languages
- `commonwl` (FAIRagro, crates.io 0.15.1, 2026-09-09): Rust crate to parse and execute CWL (`CommandLineTool`, `Workflow`, `ExpressionTool`, `Operation`). Built on `crankshaft`. Backends Local, Docker and TES all "Fully operational" with about 99% of all conformance tests and 97% of required tests; Slurm planned. Two failures come from `serde_json` storing numbers as f64. Used by SciWIn. - [GitHub fairagro/commonwl](https://github.com/fairagro/commonwl), [crates.io commonwl](https://crates.io/crates/commonwl), [GitHub fairagro/sciwin](https://github.com/fairagro/sciwin)
- `cwl-rs`: an older CWL object model for Rust (last update 2025-11-09). - [GitHub cwl-rs](https://github.com/onnovalkering/cwl-rs)
- Sprocket (St. Jude Rust Labs): WDL engine in Rust, v0.31.0 (2026-09-16), 216 stars. Crates include `wdl-grammar`, `wdl-ast`, `wdl-analysis`, `wdl-engine`, `wdl-lsp`, `wdl-lint`, `wdl-format`. `wdl-engine` depends on `crankshaft` and has backends: local, docker, apptainer, LSF+Apptainer, Slurm+Apptainer, TES. - [GitHub sprocket](https://github.com/stjude-rust-labs/sprocket), [wdl-engine backends](https://github.com/stjude-rust-labs/sprocket/tree/main/crates/wdl-engine/src/backend)
- `crankshaft`: "A Rust-based, headless workflow execution framework supporting local, cloud, and HPC", 0.11.0 (2026-08-26), 59 stars. - [GitHub crankshaft](https://github.com/stjude-rust-labs/crankshaft)

### Inferences
- If iunctus executes Nextflow natively, it will likely be the first non-JVM Nextflow runtime. The strongest starting points are the official ANTLR grammar and its tree-sitter mirror (which has Rust bindings), plus the strict syntax rules.
- For Snakemake, a pure-Rust approach can only cover a subset (OxyMake shows which constructs fall out). Full fidelity needs a Python evaluator. snakemake-lang's "compile to virtual Python" plus PyO3 suggests one route: parse in Rust, evaluate the generated Python in an embedded interpreter.
- For CWL, iunctus does not need to start from zero: `commonwl` already reaches about 99% conformance in Rust with a TES backend. Reuse or close study is an option.
- `crankshaft` is already the shared task layer for two Rust bio engines, with a TES backend. It is a natural candidate for iunctus's executor layer.

### Gaps
- GitHub search is limited. Private or unpublished Nextflow re-implementations cannot be ruled out.
- I did not measure how much of the Snakemake language snakemake-lang parses, or how complete tree-sitter-nextflow is on nf-core code.

## 7. Lessons learned for iunctus

### Takeaway
Past work says: do not try to translate between languages; execute each language with its own semantics over a shared runtime layer. The shared layer should be the concrete task (command, inputs, outputs, resources, container), not the source language constructs. Embedded code must be evaluated by real interpreters. Conformance suites decide success. Loss and unsupported features must fail loudly.

### Cited Findings
- A shared IR for two languages worked technically (WOM) but was dropped for maintenance reasons. - [Cromwell CHANGELOG](https://github.com/broadinstitute/cromwell/blob/develop/CHANGELOG.md)
- dxCompiler keeps language source attached to IR nodes and evaluates it at run time. - [dxCompiler Internals.md](https://github.com/dnanexus/dxCompiler/blob/main/doc/Internals.md)
- Static translators stop at embedded code (73-81% coverage in CNT). - [CNT PDF](https://ucare.cs.uchicago.edu/pdf/bibe23-cnt.pdf), [Janis docs](https://janis.readthedocs.io/en/translate-docs/index.html)
- Snakemake's graph only exists after its Python runs. - [OxyMake paper](https://arxiv.org/abs/2606.20989)
- Ahmed 2021: "conformance tests and backwards compatibility ... are critical". - [PMC](https://pmc.ncbi.nlm.nih.gov/articles/PMC8569008/)
- Rust CWL and WDL engines with TES backends already exist and share `crankshaft`. - [commonwl](https://github.com/fairagro/commonwl), [Sprocket](https://github.com/stjude-rust-labs/sprocket), [crankshaft](https://github.com/stjude-rust-labs/crankshaft)
- Explicit loss handling exists in wf2wf (`.loss.json`, `--fail-on-loss`) and OxyMake (fail unless `--lossy`). - [wf2wf](https://github.com/csmcal/wf2wf), [OxyMake parser](https://github.com/noogram/oxymake/blob/main/crates/ox-translate/src/snakemake/parser.rs)
- Nextflow strict syntax is default since 26.04; 66.7% of nf-core pipelines have zero lint errors today. - [strict syntax docs](https://docs.seqera.io/nextflow/strict-syntax), [nf-core health](https://github.com/nf-core/strict-syntax-health)

### Inferences
1. **Use two IR levels, not one static DAG.** Level 1 is a per-language "program" model (Nextflow processes, channels and operators; Snakemake rules with patterns and input functions; CWL steps with scatter and `when`). Level 2 is a shared, runtime-grown graph of concrete task instances. wf2wf's single static DAG is the design to avoid.
2. **Unify at the task level, like Latch JIT and `--export-cwl` do, but natively.** All three languages end in "run this command with these files, this container and these resources". That is also exactly what GA4GH TES needs. Most cross-language value (scheduling, caching, provenance, executors) lives there.
3. **Keep embedded code as code and evaluate it with a real interpreter.** Plan for a JavaScript engine (CWL expressions), a Groovy-subset interpreter scoped to Nextflow strict syntax, and a Python evaluator for Snakemake (for example embedded CPython). Translating this code is where every earlier project stopped.
4. **Scope Nextflow to strict syntax.** It is the default since 26.04 and removes imports, classes and loops. Say clearly that legacy (v1 parser only) code is out of scope. Use nf-core lint health as a moving measure of real-world coverage.
5. **Scope Snakemake honestly.** Pure-Rust parsing covers static rules. Checkpoints, input functions, `run:` blocks, modules and hooks need Python. Decide early whether to embed Python or to declare these out of scope. OxyMake's list is a good starting inventory of the hard parts.
6. **Build on conformance suites.** CWL and WDL have them (commonwl reaches about 99% for CWL). Nextflow and Snakemake have none, so iunctus must build its own from nf-core pipelines, nf-core module tests and the Snakemake test suite, and compare outputs against the reference engines.
7. **Fail loudly and report loss.** Copy the explicit patterns (`--fail-on-loss`, `--lossy`, loss reports). Silent partial support is what made past converters untrustworthy.
8. **Reuse the Rust ecosystem.** Candidates: `crankshaft` (executor layer with TES), `commonwl` (CWL parse and engine), `tree-sitter-nextflow` (Nextflow parse, Rust bindings), `snakemake-lang` (Snakemake parse to AST or Python). Check each crate's maturity first; several are young and have few users.
9. **Treat configuration as a first-class language.** Nextflow config (profiles, `withName`/`withLabel` selectors, closures) and Snakemake profiles and config broke regex-based tools and even Nextflow's own `kuberun`. iunctus needs a proper config evaluator per language.
10. **Budget for moving targets.** Nextflow ships edge releases often and changed its parser in 2025-2026. Snakemake is at 9.x with a plugin architecture. Cromwell shows a funded team dropping a language. Per-language front ends need owners and CI against upstream releases.
11. **Emit standard provenance.** Workflow Run RO-Crate (already produced by nf-prov, Sapporo, WfExS, Galaxy) is the expected output. WfFormat export of the instantiated graph is cheap and useful for research.
12. **Use LLMs only as helpers.** They can suggest fixes for unsupported constructs, but every published system pairs them with deterministic validation. They are not a replacement for execution semantics.

### Gaps
- No data exists on how many nf-core or Snakemake catalog pipelines use each hard feature (checkpoints, input functions, custom Groovy, exotic operators). Measuring this would be the most useful next step for the feasibility study.
- No evidence was found on performance of a non-JVM Nextflow runtime, because none exists.
