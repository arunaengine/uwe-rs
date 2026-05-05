<!-- Research notes on the Snakemake language and runtime. -->
<!-- Copyright (c) 2026 The iunctus Contributors -->
<!-- SPDX-License-Identifier: MIT or Apache-2.0 -->

# Snakemake: language, runtime, and what a native Rust implementation needs

Research date: 2026-09-27. Main evidence: a shallow clone of `snakemake/snakemake` (tag v9.27.0 plus one later commit on `main`, 91763d6, dated 2026-09-11). I also cloned the TES executor plugin, the executor plugin interface, snakemake-wrappers, snakefmt, tree-sitter-snakemake, pydantic/monty and facebook/starlark-rust. I downloaded the workflow catalog data file and scanned 33 real workflows. Line numbers refer to files at that commit. "Own measurement" marks numbers I computed myself from the clones. Everything under "Inferences" is my own reasoning, not a sourced fact.

## Q1. Current Snakemake version and major changes in 8.x and 9.x

### Takeaway
The current release is Snakemake 9.27.0 (2026-09-11). Since 8.0 (2023-12-20) Snakemake has been split into a core plus plugin interfaces: executor, storage, report, logger, scheduler, and (unreleased) software deployment. The `main` branch already holds an unreleased breaking change that adds a `software:` rule directive. So the language is still changing in small but real ways.

### Cited Findings
- Latest tag is v9.27.0, released 2026-09-11. Earlier: 9.26.1 (2026-08-27), 9.26.0 (2026-08-26), 9.25.0 (2026-08-04), 9.24.0 (2026-07-30), 9.23.0 (2026-06-11), 9.22.0 (2026-06-01) ([CHANGELOG.md](https://github.com/snakemake/snakemake/blob/main/CHANGELOG.md); [v9.27.0 tag](https://github.com/snakemake/snakemake/releases/tag/v9.27.0)).
- 9.0.0 was released 2025-03-14. 8.0.0 was released 2023-12-20. So 9.x had 27 minor releases in about 18 months ([CHANGELOG.md](https://github.com/snakemake/snakemake/blob/main/CHANGELOG.md)).
- The docs site title is "Snakemake 9.27.0 documentation" ([snakemake.readthedocs.io](https://snakemake.readthedocs.io/)).
- Snakemake 9.x requires Python 3.11 or newer (`requires-python = ">= 3.11"`) ([pyproject.toml](https://github.com/snakemake/snakemake/blob/main/pyproject.toml)).
- 8.0 changelog: "Snakemake 8 marks the beginning of decomposing Snakemake into a framework of plugins. ... We start with plugins for storage and execution backends." Breaking changes in 8.0: removal of `dynamic`, `version` and `subworkflow`; old remote providers moved to storage plugins; execution backends moved to executor plugins with a new CLI; `--use-conda` and `--use-singularity` deprecated in favor of `--software-deployment-method conda|apptainer`; versioned profiles; a new dataclass-based API ([CHANGELOG 8.0.0](https://github.com/snakemake/snakemake/blob/main/CHANGELOG.md); [migration docs](https://snakemake.readthedocs.io/en/stable/getting_started/migration.html)).
- Report plugins arrived in 8.5.0 (PR #2700) ([CHANGELOG.md](https://github.com/snakemake/snakemake/blob/main/CHANGELOG.md)).
- 9.0.0 breaking change: "Logging refactor & add LoggerPluginInterface" (#3107). 9.0 also added item-level extension of inherited items with `use rule ... with`, dynamic module names, named `multiext`, optional auto-grouping of jobs via temp files for remote execution, and mermaid DAG output ([CHANGELOG 9.0.0](https://github.com/snakemake/snakemake/blob/main/CHANGELOG.md); [Zenodo report on the logging refactor, 2025-06-21](https://zenodo.org/records/15709330)).
- 9.10.0 (2025-08-19): "migrate to scheduler plugin interface and scheduler plugins" (#3676) ([CHANGELOG.md](https://github.com/snakemake/snakemake/blob/main/CHANGELOG.md)).
- 9.12.0 (2025-10-04): "introduce pathvars, allowing to configure parts of input/output/log/benchmark paths for the entire workflow, per module, or per rule" (#3760) ([CHANGELOG.md](https://github.com/snakemake/snakemake/blob/main/CHANGELOG.md)).
- 9.17.0 (2026-03-13): "Allow storing snakemake metadata in files or databases" (#4012), "lambda functions for priority in rules" (#3253), and `profile.yaml` as the default profile file name ([CHANGELOG.md](https://github.com/snakemake/snakemake/blob/main/CHANGELOG.md)).
- 9.23.0 (2026-06-11): "support remote snakefile via gh/gl shorthand". 9.24.0 and 9.25.0 extended the `branch()` and `lookup()` helper functions ([CHANGELOG.md](https://github.com/snakemake/snakemake/blob/main/CHANGELOG.md)).
- Unreleased on `main`: commit 91763d6 (2026-09-11), "feat!: generic software deployment plugin support (#3339)". It adds a `software:` directive with `or` fallbacks and `within=` nesting, for example `software: envmodules("samtools") or container("quay.io/biocontainers/samtools:1.12") or conda(envfile="envs/samtools.yaml")`. The commit message says the old `conda:`, `container:`, `singularity:` and `envmodules:` directives "can internally be delegated to the new specifications" ([PR #3339](https://github.com/snakemake/snakemake/pull/3339); [parser.py on main](https://github.com/snakemake/snakemake/blob/main/src/snakemake/parser.py)).
- Plugin interface pins on `main`: executor >=9.4.0, storage >=4.4.1, report >=2.0.1, software deployment >=0.19.1, logger >=1.1.0, scheduler >=2.0.0. At v9.27.0 the software deployment interface was not yet a dependency ([pyproject.toml](https://github.com/snakemake/snakemake/blob/main/pyproject.toml)).
- Software deployment plugin repositories exist for conda, container, envmodules, eessi, spack and conda2spack (snakemake org), plus third-party pixi, nix, guix and cvmfs plugins. The interface package is at v0.21.2 ([snakemake-interface-software-deployment-plugins](https://github.com/snakemake/snakemake-interface-software-deployment-plugins); [conda plugin](https://github.com/snakemake/snakemake-software-deployment-plugin-conda); [container plugin](https://github.com/snakemake/snakemake-software-deployment-plugin-container)).
- The plugin catalog lists these executor plugins: aws-basic-batch, aws-batch, azure-batch, cluster-generic, cluster-sync, deeporigin, donau, drmaa, finished, flux, googlebatch, htcondor, ihep, kubernetes, kueue, latch, lsf, lsf-jobstep, lsf-sanger, panda, pcluster-slurm, sge, slurm, slurm-gustave-roussy, slurm-jobstep, tes, vastai. Storage plugins: apd, azure, cached-http, deeporigin, dirac, fs, ftp, gcs, git, http, irods, latch, lfs, nersc, orcestra, pelican, rucio, s3, sftp, sharepoint, webdav, xrootd, zenodo. Report plugins: fairscape, nanopub. Logger plugins: dart, flowo, github-status, panoptes, prometheus, pypsa, rich, rich-loguru, snakesee, snkmt, stomp, sumerrs. Scheduler plugins: firstfit, grapheonrl, greedy, milp ([Snakemake plugin catalog](https://snakemake.github.io/snakemake-plugin-catalog/)).

### Inferences
- The language keeps growing in small steps (pathvars, resource_scopes, software, new helper functions). A Rust implementation must pin one Snakemake version as its reference (for example 9.27.0) and track changes deliberately.
- The plugin split helps a re-implementation. The plugin interfaces document what a job needs from storage and from software deployment, so iunctus can mirror these boundaries in its own design.
- The new `software:` directive will likely appear in a new major version. Iunctus should design its software-environment model so both the old directives and the new `software:` expression map onto it.

### Gaps
- No release date or version number has been announced for the release containing `software:`. "10.0" is my guess from the `feat!` prefix only.
- Plugin category totals came from a page summary tool that gave slightly inconsistent counts. The name lists above are more reliable than totals.

## Q2. How Snakefiles are parsed, and the full keyword list

### Takeaway
Snakemake has no formal grammar. `parser.py` runs Python's `tokenize` module over the Snakefile. A hand-written state machine spots Snakemake keywords at line starts and rewrites them into decorated Python functions (`@workflow.rule(...)`, `@workflow.input(...)`, ...). Everything else passes through as plain Python. The result is then executed as Python. So a Snakefile's meaning is defined by running it, not by a static grammar.

### Cited Findings
- `parse()` opens the Snakefile, calls `tokenize.generate_tokens`, runs the `Python` token automaton, joins the emitted tokens into one Python source string and keeps a line map for error messages (parser.py lines 1323-1381) ([parser.py](https://github.com/snakemake/snakemake/blob/main/src/snakemake/parser.py)).
- Rule keywords compile to decorators: `yield f"@workflow.{self.keyword}("` (line 290), `@workflow.run` for the rule body (line 584), `@workflow.norun()` for rules without a body (line 773) and `@workflow.userule(rules=..., from_module=..., exclude_rules=..., name_modifier=...)` for `use rule` (line 1039). `shell`, `script`, `notebook`, `wrapper`, `template_engine` and `cwl` are command states that emit `@workflow.shellcmd` (and similar) and a call inside the generated run function ([parser.py](https://github.com/snakemake/snakemake/blob/main/src/snakemake/parser.py)).
- A `run:` block becomes a generated function `def __rule_<name>(input, output, params, wildcards, threads, resources, log, rule, conda_env, container_img, ...)` at v9.27.0. On `main` the argument list comes from `RunArgs.rulefunc_args_signature()` (diff v9.27.0..main) ([parser.py](https://github.com/snakemake/snakemake/blob/main/src/snakemake/parser.py)).
- `snakemake --print-compilation` prints the generated Python ([cli.py](https://github.com/snakemake/snakemake/blob/main/src/snakemake/cli.py)).
- Global keywords (the `Python.subautomata` table, parser.py lines 1267-1296 on `main`): `envvars`, `include`, `workdir`, `configfile`, `pathvars`, `pepfile`, `pepschema`, `report`, `ruleorder`, `rule`, `checkpoint`, `localrules`, `onsuccess`, `onerror`, `onstart`, `wildcard_constraints`, `singularity`, `container`, `containerized`, `conda`, `scattergather`, `inputflags`, `outputflags`, `storage`, `resource_scopes`, `module`, `use`. `subworkflow` is rejected as deprecated ("Use module directive instead") ([parser.py](https://github.com/snakemake/snakemake/blob/main/src/snakemake/parser.py)).
- Rule property directives (`rule_property_subautomata`, lines 713-738): `name`, `input`, `output`, `params`, `threads`, `resources`, `retries`, `priority`, `log`, `message`, `benchmark`, `conda`, `singularity`, `container`, `software` (only on `main`, not in 9.27.0), `containerized`, `envmodules`, `wildcard_constraints`, `shadow`, `group`, `cache`, `handover`, `default_target`, `localrule`, `pathvars`. `version` is rejected as deprecated ([parser.py](https://github.com/snakemake/snakemake/blob/main/src/snakemake/parser.py)).
- Rule execution directives (`rule_run_subautomata`): `run`, `shell`, `script`, `notebook`, `wrapper`, `template_engine`, `cwl`. The parser errors on "Multiple {} keywords in rule {}" and "No rule keywords allowed after {} in rule {}" (lines 805-813) ([parser.py](https://github.com/snakemake/snakemake/blob/main/src/snakemake/parser.py)).
- `module` sub-keywords: `name`, `snakefile`, `meta_wrapper`, `config`, `skip_validation`, `replace_prefix`, `prefix`, `pathvars`. `use rule` accepts the same property directives as `rule` (lines 870-1023) ([parser.py](https://github.com/snakemake/snakemake/blob/main/src/snakemake/parser.py)).
- Some keywords map to different workflow methods: `envvars` to `register_envvars`, global `pathvars` to `register_pathvars`, `inputflags`/`outputflags` to `set_default_input_flags`/`set_default_output_flags`, global `singularity`/`container` to `global_container`, rule `singularity` to `container`, `cache` to `cache_rule`, `default_target` to `default_target_rule` ([parser.py](https://github.com/snakemake/snakemake/blob/main/src/snakemake/parser.py)).
- Names injected into the Snakefile namespace: `protected`, `temp`, `temporary`, `ancient`, `directory`, `expand`, `glob_wildcards`, `flag`, `touch`, `unpack`, `local`, `pipe`, `service`, `repeat`, `report`, `multiext`, `ensure`, `from_queue` ([workflow.py](https://github.com/snakemake/snakemake/blob/main/src/snakemake/workflow.py)); `update`, `before_update` ([ioflags.py](https://github.com/snakemake/snakemake/blob/main/src/snakemake/ioflags.py)); `access.random/sequential/multi` ([io/flags/access_patterns.py](https://github.com/snakemake/snakemake/blob/main/src/snakemake/io/flags/access_patterns.py)); and the helper functions `lookup`, `evaluate`, `branch`, `collect`, `exists`, `parse_input`, `extract_checksum`, `flatten`, `subpath`, `prepend_param`, `choose_file`, `choose_folder`, `choose_tmp`, `as_py_module`, plus deferred rule item proxies `input`, `output`, `params`, `resources`, `threads` ([ioutils/__init__.py](https://github.com/snakemake/snakemake/blob/main/src/snakemake/ioutils/__init__.py)). The namespace also gets `shell`, `workflow`, `checkpoints`, `scatter`, `gather`, `github`, `gitlab`, `gitfile`, `storage`, `access` and `config` ([workflow.py](https://github.com/snakemake/snakemake/blob/main/src/snakemake/workflow.py) lines 240-268).
- The tokenizer approach breaks on new Python syntax from time to time. Recent fixes: "fix f-string parsing at file end" (9.25.1) and "compilation error when final line contains an f-string" (9.24.0) ([CHANGELOG.md](https://github.com/snakemake/snakemake/blob/main/CHANGELOG.md)).
- snakefmt, the official formatter (v2.0.3, 2026-06-26), has its own parser. It is also built on Python `tokenize`, with a grammar module that defines vocabularies for rules, modules, `use rule` and global keywords. It formats the Python parts with Black ([snakefmt](https://github.com/snakemake/snakefmt)).
- tree-sitter-snakemake (Oliver Thomas, grammar version 2.1.0, last push 2026-05-12) "is an extension of tree-sitter-python". It ships Rust bindings (crate name `tree-sitter-snakemake` in its Cargo.toml). I did not find it on crates.io ([tree-sitter-snakemake](https://github.com/osthomas/tree-sitter-snakemake)).
- snakemake-lang (Cade Mirchandani, created 2026-05-19, 24 commits) is a Rust "Parser, AST, and compiler for the Snakemake workflow language". Its plan says it "Replaces `parser.py` in the Snakemake engine" and builds on ruff's Python parser crates. Its CLI has `compile` (Snakefile to "virtual Python"), `parse` (AST as JSON) and `check`. It is very early ([snakemake-lang](https://github.com/cademirch/snakemake-lang)).
- `ruff_python_parser` is published on crates.io as 0.0.15 (2026-09-24) and describes itself as "an internal component crate of Ruff" ([crates.io](https://crates.io/crates/ruff_python_parser)).
- A crates.io search for "snakemake" on 2026-09-27 found no crate that implements Snakemake parsing or semantics ([crates.io search](https://crates.io/search?q=snakemake)).

### Inferences
- Parsing is the easy part. A Rust parser can copy the snakefmt and snakemake-lang approach: use ruff's lexer, detect Snakemake keywords at line starts, and parse each directive value as a Python expression with `ruff_python_parser`. That gives a full Python AST for every value, which an evaluator needs anyway.
- The hard part is that the parsed file is still a Python program. Rules are registered as a side effect of running top-level code. Loops can create rules. `include:` can be conditional or computed. Rule names can be computed. A static reader cannot handle these without evaluating Python.
- `ruff_python_parser` is marked internal and has a 0.0.x version. Pin it, or vendor it. tree-sitter is good for editor tooling, but it gives a concrete syntax tree, not a Python AST.
- snakemake-lang is worth watching. If it becomes the official parser, iunctus could reuse it or match it.

### Gaps
- I did not run snakemake-lang or tree-sitter-snakemake on real Snakefiles. Their coverage is unknown.

## Q3. Execution semantics

### Takeaway
Snakemake is a file-based pull system. It starts from target files, finds rules whose output patterns match (by regex), infers wildcard values, and recurses on the inputs. Rerun decisions use modification times plus recorded metadata (params, input set, code, software environment). Checkpoints make the DAG dynamic. The core is well defined and can be written in Rust. Checkpoints, groups, pipes and services, and exact rerun-trigger compatibility are the hard parts.

### Cited Findings
- Pull model: "Dependencies between jobs are implicit, and inferred automatically" by finding rules that can create the needed input files. This yields "a directed acyclic graph of jobs (DAG)" ([Mölder et al. 2021, F1000Research 10:33](https://doi.org/10.12688/f1000research.29032.2)).
- Default target: "By default, Snakemake always wants to execute the first rule in the snakefile." A rule can be marked with `default_target: True` instead ([rules docs](https://snakemake.readthedocs.io/en/stable/snakefiles/rules.html); docs/snakefiles/rules.rst lines 945-969).
- Wildcard inference: `regex_from_filepattern` turns each `{name}` in an output pattern into a named regex group. If there is no constraint, the default is `.+` (io/__init__.py lines 1285-1304) ([io/__init__.py](https://github.com/snakemake/snakemake/blob/main/src/snakemake/io/__init__.py)).
- Wildcards can be ambiguous. The docs example: pattern `{dataset}.{group}.txt` and file `101.B.normal.txt`. Constraints can be written inline (`{dataset,\d+}`), per rule (`wildcard_constraints:`) or globally ([rules docs, Wildcards](https://snakemake.readthedocs.io/en/stable/snakefiles/rules.html#wildcards)).
- Ambiguity: "When two rules can produce the same output file, snakemake cannot decide which one to use without additional guidance. Hence an `AmbiguousRuleException` is thrown." `ruleorder: rule1 > rule2 > rule3` resolves it. "Only if rule1 and rule2 cannot be applied (e.g. due to missing input files), rule3 is used" ([rules docs, Handling Ambiguous Rules](https://snakemake.readthedocs.io/en/stable/snakefiles/rules.html#handling-ambiguous-rules)).
- Input functions: `input:`, `params:`, `resources:`, `threads:` and others accept Python callables that receive `wildcards`. For resources the docs give the signature `callable(wildcards [, input] [, threads] [, attempt])`, where `attempt` counts restarts from 1 (used with `--retries`). `unpack()` turns a returned dict into named inputs ([rules docs](https://snakemake.readthedocs.io/en/stable/snakefiles/rules.html); docs/snakefiles/rules.rst lines 1181-1207).
- Shell commands: Snakemake prefixes bash commands with `set -euo pipefail; ` ([shell.py](https://github.com/snakemake/snakemake/blob/main/src/snakemake/shell.py) line 111). The `:q` format spec (for example `{input:q}`) quotes each element that contains whitespace ([rules docs](https://snakemake.readthedocs.io/en/stable/snakefiles/rules.html)).
- `expand()` builds the product of wildcard values (or `zip`). `glob_wildcards()` scans the filesystem and returns wildcard values ([io/__init__.py](https://github.com/snakemake/snakemake/blob/main/src/snakemake/io/__init__.py) lines 1554, 1762; [rules docs](https://snakemake.readthedocs.io/en/stable/snakefiles/rules.html)).
- Checkpoints: "From Snakemake 5.4 on, conditional reevaluation of the DAG of jobs based on the content outputs is possible." A rule is declared with `checkpoint`, and input functions call `checkpoints.<name>.get(**wildcards)`. Until the checkpoint job has run, this call signals that the DAG must be re-evaluated later ([rules docs, Data-dependent conditional execution](https://snakemake.readthedocs.io/en/stable/snakefiles/rules.html#data-dependent-conditional-execution); [checkpoints.py](https://github.com/snakemake/snakemake/blob/main/src/snakemake/checkpoints.py)).
- Checkpoints are still a bug source in 9.x: "fix bug in checkpoint handling that led to exceptions in case checkpoint output was missing upon rerun" (9.0.0), "fix checkpoint handling corner cases (#3870 and #3559)" (9.17.0) and "do not rerun when checkpoint job missing but downstream file exists" (9.21.1) ([CHANGELOG.md](https://github.com/snakemake/snakemake/blob/main/CHANGELOG.md)).
- File flags defined in code: `temp`/`temporary`, `protected`, `directory`, `ancient`, `pipe`, `service`, `touch`, `ensure(non_empty=..., sha256=..., md5=...)`, `local`, `repeat`, `report`, `multiext`, `from_queue`, `update`, `before_update`, and storage access-pattern hints ([io/__init__.py](https://github.com/snakemake/snakemake/blob/main/src/snakemake/io/__init__.py) lines 1341-1760; [ioflags.py](https://github.com/snakemake/snakemake/blob/main/src/snakemake/ioflags.py); update handling in [dag.py](https://github.com/snakemake/snakemake/blob/main/src/snakemake/dag.py) lines 2422-2430). Their meaning (temp files deleted when no longer needed, protected files made read-only, pipes and services run concurrently with consumers, ancient ignores timestamps) is described in the [rules docs](https://snakemake.readthedocs.io/en/stable/snakefiles/rules.html).
- Rerun triggers: `RerunTrigger` has MTIME, PARAMS, INPUT, SOFTWARE_ENV, CODE ([settings/enums.py](https://github.com/snakemake/snakemake/blob/main/src/snakemake/settings/enums.py)). The CLI default is all of them: "By default, all triggers are used, which guarantees that results are consistent with the workflow code and configuration" ([cli.py](https://github.com/snakemake/snakemake/blob/main/src/snakemake/cli.py) line 802).
- Metadata: each output file gets a `MetadataRecord` with rule, input, log, shellcmd, params, code, software_stack_hash, software, job_hash, starttime, endtime, incomplete, external_jobid and input_checksums. `RECORD_FORMAT_VERSION = 7` ([persistence/__init__.py](https://github.com/snakemake/snakemake/blob/main/src/snakemake/persistence/__init__.py)).
- Metadata storage: the default "file" backend uses `.snakemake/metadata`, `.snakemake/incomplete` and `.snakemake/locks`. The "db" backend uses SQLAlchemy/SQLModel, with SQLite at `.snakemake/metadata.db` by default or a database URL ([persistence/file.py](https://github.com/snakemake/snakemake/blob/main/src/snakemake/persistence/file.py); [persistence/db.py](https://github.com/snakemake/snakemake/blob/main/src/snakemake/persistence/db.py); [cli.py](https://github.com/snakemake/snakemake/blob/main/src/snakemake/cli.py) line 1643).
- Scheduler: the default scheduler is `"ilp"` ([settings/types.py](https://github.com/snakemake/snakemake/blob/main/src/snakemake/settings/types.py) line 299). It builds a PuLP `LpProblem` that maximizes, in order: job priority, core load, temp file removal, temp file size. Each resource is a constraint (sum of scheduled job needs <= available) ([scheduling/milp.py](https://github.com/snakemake/snakemake/blob/main/src/snakemake/scheduling/milp.py)). The paper describes the same four lexicographic criteria as a MILP ([Mölder et al. 2021](https://doi.org/10.12688/f1000research.29032.2)). A greedy scheduler is also built in ([scheduling/greedy.py](https://github.com/snakemake/snakemake/blob/main/src/snakemake/scheduling/greedy.py)).
- Groups: grouped jobs "will be executed together in cluster or cloud mode, as a so-called group job, i.e., all jobs of a particular group will be submitted at once, to the same computing node." "When executing locally, group definitions are ignored." Resource scopes, added in 7.11.0 ("improved resource handling in groups and ability to define resource scopes"), decide how resources add up inside a group job ([grouping docs](https://snakemake.readthedocs.io/en/stable/executing/grouping.html); [CHANGELOG.md](https://github.com/snakemake/snakemake/blob/main/CHANGELOG.md)).
- Core module sizes on `main`: dag.py 3,462 lines, workflow.py 2,507, jobs.py 1,942, rules.py 1,505, parser.py 1,381, io/__init__.py about 66 KB, resources.py about 52 KB (own measurement, `wc -l`/`ls` on the clone).

### Inferences
- The pattern matching, DAG building, mtime checks, temp cleanup and resource-aware scheduling are standard algorithms. Rust can do them well.
- Exact reuse of Snakemake's `.snakemake` metadata is not realistic. PARAMS and CODE triggers depend on Python object representation and on Python source text of the rule. Iunctus should keep its own metadata store, and treat "mtime + input set + resolved command + container image" as its rerun triggers.
- Snakemake's regex matching has subtle rules (default `.+`, constraints, ruleorder, preference when one candidate lacks inputs). The Snakemake tests are the only reliable specification of these corner cases.
- The ILP scheduler does not need to be reproduced. For TES, the remote backend does placement, so a simple priority plus resource-limit scheduler is enough at first.

### Gaps
- I did not trace Snakemake's full candidate-selection logic in `dag.py` (how it chooses between matching rules when some cannot be satisfied). This needs a code read or test-driven reverse engineering.
- The exact hash inputs for the CODE trigger were not examined.

## Q4. The Python dependency and options for Rust

### Takeaway
Real workflows depend on Python a lot. In my sample of 33 workflows, 31 define helper functions, 27 import pandas and 27 use lambdas. So a static-only subset covers few production workflows. The Python needed at DAG-building time (config, sample sheets, input functions) is different from the Python needed at job time (`script:`, `wrapper:`, `run:`). Job-time Python can run inside the job container. DAG-time Python must run inside iunctus. Only embedding CPython (PyO3) gives high compatibility with pandas-based Snakefiles. RustPython and Monty can cover pure-Python helper code. starlark-rust is not Python and would need rewriting.

### Cited Findings
- Snakemake is "implemented as an extension to a generic programming language" (Python), and "access to the full power of the underlying programming language is maintained (e.g. for implementing conditional execution and handling configuration)" ([Mölder et al. 2021](https://doi.org/10.12688/f1000research.29032.2)).
- Own measurement, method: shallow clones of 30 non-archived repositories in the [snakemake-workflows GitHub organization](https://github.com/snakemake-workflows) (skipping `docs`, the cookiecutter template and `ngs-test-data`) plus 5 widely used external workflows ([PyPSA/pypsa-eur](https://github.com/PyPSA/pypsa-eur), [metagenome-atlas/atlas](https://github.com/metagenome-atlas/atlas), [moiexpositoalonsolab/grenepipe](https://github.com/moiexpositoalonsolab/grenepipe), [maxplanck-ie/snakePipes](https://github.com/maxplanck-ie/snakePipes), [sequana/sequana](https://github.com/sequana/sequana)). I counted regex matches in `Snakefile`, `*.smk` and `*.snakefile` files outside test folders. 33 repositories had Snakemake files: 372 files, 53,818 lines, 1,753 rules.
- Own measurement, execution directives: `shell:` 745, `script:` 404, `wrapper:` 362, `run:` 145, `notebook:` 1, `template_engine:` 3, `cwl:` 0. `run:` is concentrated in pypsa-eur (67), atlas (31) and snakePipes (26).
- Own measurement, repositories using a feature (out of 33): shell 28, script 28, wrapper 22, run 9, checkpoints 7 (14 checkpoint rules, 30 `checkpoints.x.get(` calls), helper `def` 31 (743 functions), `lambda` 27 (697), pandas 27, `expand()` 29 (898 calls), `config[...]`/`config.get` 32 (2,125), `lookup()` 13, `validate()` 21, `use rule` 3, `module` 2, `class` 2 in total, `pepfile` 0.
- Own measurement, top-level imports by number of repositories: snakemake 28, pandas 27, os 13, glob 9, pathlib 8, yaml 6, gzip 6, re 5, sys 5, math 4, numpy 4, json 4, urllib 4, subprocess 3, shutil 3, datetime 3, itertools 3.
- `script:` supports `.py`, `.R`, `.Rmd`, `.jl`, `.rs`, `.sh`, `.xsh`, `.hy` and Jupyter `.ipynb` via `notebook:` ([script/__init__.py](https://github.com/snakemake/snakemake/blob/main/src/snakemake/script/__init__.py) lines 1015-1081).
- The Python script preamble pickles a `Snakemake` object with protocol 4, extends `sys.path` with Snakemake's own install path because "The module is needed for unpickling in the script", then runs `snakemake = pickle.loads(...)`. So a Python script job needs the `snakemake` package importable at run time. Bash scripts get a generated preamble from `BashEncoder` (associative arrays for input, output, log, resources, wildcards, params and a config dict). R scripts get an R object built by `RScript` ([script/__init__.py](https://github.com/snakemake/snakemake/blob/main/src/snakemake/script/__init__.py) lines 378-445, 512, 905-948).
- Own measurement on [snakemake-wrappers](https://github.com/snakemake/snakemake-wrappers) (release v9.18.0, 2026-09-22): 473 Python `wrapper.py` and 26 `wrapper.R` files under `bio/` and `utils/`; 432 Python wrappers contain `from snakemake.shell import shell`; 135 import `snakemake_wrapper_utils`; 482 wrapper test Snakefiles.
- RustPython: crate `rustpython` 0.5.0 (crates.io, 2026-03-31). The README says it targets Python 3.14 and that "RustPython is not totally production-ready". It does not support C extension packages such as NumPy or pandas. It can be embedded in Rust ([RustPython](https://github.com/RustPython/RustPython); [crates.io](https://crates.io/crates/rustpython)).
- Monty (pydantic): v1.0.0 released 2026-09-25, Rust crate `monty` 1.0.0. "Monty is a sandbox, not a Python implementation aiming for completeness." Rejected at parse time: "Class inheritance and metaclasses", "Decorators on methods", `yield`, `match`, `del`, `async with`/`async for`, wildcard imports. "Third-party packages. There is no `sys.path` and no site-packages." Stdlib modules present: asyncio, base64, binascii, collections, copy, dataclasses, datetime, functools, itertools, json, math, os, pathlib, random, re, sys, time, typing, unicodedata. "Notably absent: `enum`, `contextlib`, `io`, `string`, `struct`, `operator`, `inspect`, `logging`, `traceback`, `hashlib`, `uuid`, `urllib`." Host code is reached "only through the functions and mounts you pass in" ([Monty limitations index](https://github.com/pydantic/monty/blob/main/docs/limitations/index.md); [modules.md](https://github.com/pydantic/monty/blob/main/docs/limitations/modules.md); [Monty README](https://github.com/pydantic/monty); [crates.io](https://crates.io/crates/monty)).
- starlark-rust: release v0.14.0 (2026-06-01), crate `starlark` 0.14.2 (2026-06-05). Its `Dialect` has switches such as `enable_def`, `enable_lambda`, `enable_load`, `enable_top_level_stmt` and `enable_f_strings` ([starlark-rust](https://github.com/facebook/starlark-rust); [dialect.rs](https://github.com/facebook/starlark-rust/blob/main/starlark_syntax/src/dialect.rs); [crates.io](https://crates.io/crates/starlark)). Starlark (as described for Bazel) does not support `class`, `import`, `while`, `yield`, generators, `is`, `try/raise/except/finally`, `global/nonlocal` or recursion; "Global variables are immutable" ([Bazel Starlark language](https://bazel.build/rules/language)).
- PyO3: v0.29.2 (2026-08-05). Embedding CPython: "Embedding the Python interpreter dynamically is much easier than doing so statically." With dynamic linking, "you will have to consider including the Python shared library in your distribution". With static linking, "The C compiler and flags which were used to create `libpython.a` must be compatible with your Rust compiler and flags". The guide points to PyOxidizer for single-file shipping ([PyO3 building and distribution](https://pyo3.rs/v0.29.2/building-and-distribution.html); [PyO3 repo](https://github.com/PyO3/pyo3)).
- peprs, a Rust implementation of the PEP (Portable Encapsulated Projects) spec, is on crates.io at 0.2.2 (2026-09-05). This is relevant to `pepfile:` support ([crates.io](https://crates.io/crates/peprs)).

### Evaluation of the five options (inference, based on the findings above)

| Option | Fit with real Snakefiles | Fit with "native Rust app" | Main problem |
|---|---|---|---|
| (a) RustPython 0.5 | Medium for pure-Python helpers, `os`/`re`/`glob`/`json`/`pathlib`, simple classes. None for pandas/numpy (27 of 33 sampled repos import pandas). A pure-Python YAML library could work. | Good: pure Rust, embeddable. | Not production-ready. Slower than CPython. Must re-implement Snakemake's Python-visible API (`expand`, `Namedlist`, `config`, `checkpoints`, flags) as Python objects backed by Rust. |
| (b) Monty 1.0 | Low-to-medium. Covers lambdas and small helper functions. No `glob`, `yaml`, `csv`, pandas, class inheritance, generators. | Very good: Rust crate, fast start, sandboxed, host functions map well to Snakemake builtins. | Designed for LLM-generated code, not general Python. Many real Snakefiles import modules Monty lacks. |
| (c) starlark-rust | Low. No imports, classes, exceptions, `while`. Every workflow would need rewriting. | Very good: mature, deterministic, used by Buck2. | It is not Python. It suits a new "iunctus dialect", not existing Snakefiles. |
| (d) PyO3 + CPython | High. Real `pandas`, `yaml`, `peppy` etc. can be imported if installed. | Weak: needs a Python runtime and the workflow's Python packages on the host. Static embedding is hard. Does not shell out to the snakemake binary, but depends on a Python install. | Environment management (which Python, which packages) becomes iunctus's problem. Temptation to import the `snakemake` package itself, which would defeat the goal. |
| (e) Restricted static subset | Low for curated workflows (31 of 33 define helper functions). Probably higher for small ad-hoc workflows. | Best: fully native, analyzable, safe. | Needs a clear error when a Snakefile leaves the subset. |

### Inferences
- Split Python into two kinds. DAG-time Python (top-level code, config handling, input functions, params, resources) must run inside iunctus. Job-time Python (`run:`, `script:`, `wrapper:`, `notebook:`) can run inside the job's container, like any other tool.
- For job-time Python, iunctus must generate the preamble itself. For Python scripts and wrappers, the container needs an importable `snakemake` module. A small pure-Python shim (a `snakemake` object with `input`, `output`, `params`, `wildcards`, `threads`, `resources`, `log`, `config`, plus `snakemake.shell.shell`) could be injected as a TES input. This avoids requiring the real Snakemake package in the container. Wrappers that import `snakemake_wrapper_utils` (135 of 473) need that package too.
- `run:` blocks are DAG-time-defined but job-time-executed Python that can use any Snakefile global. They need the whole Snakefile context. Supporting them remotely requires shipping the evaluated globals. This is a late or never feature.
- Recommended layering: (e) native evaluator as the base, (b) Monty or (a) RustPython as an optional embedded interpreter for pure-Python helper functions, and (d) PyO3/CPython as an opt-in "compatibility mode" build feature for pandas-heavy workflows. Monty's host-function model fits best for exposing `expand`, `config`, `checkpoints` and file flags. RustPython fits better if classes and more stdlib are needed.
- A common pattern is `samples = pd.read_csv(config["samples"], sep="\t").set_index("sample")` followed by lookups. A native evaluator could special-case a small, well-defined pandas subset (read a TSV/CSV, `set_index`, `.loc`, `.index`, `unique()`) plus Snakemake's own `lookup()` helper. This is guesswork until measured on a larger set of test workflows.

### Gaps
- My survey is small (33 repositories) and biased toward curated, well-maintained workflows. The 5,755 non-standardized catalog workflows may use less Python. A larger scan of the catalog is needed to size the static subset.
- I did not test RustPython or Monty on real Snakefile helper code.
- I found no published study that measures Python feature use in Snakefiles.

## Q5. Plugins, the TES executor, storage, and how a job is run remotely

### Takeaway
The TES executor plugin exists but is small and stale: v0.1.3 (2024-03-12), last commit 2024-09-07, 6 open issues. Like all Snakemake remote executors, it does not send the real tool command to TES. It sends a container that re-runs Snakemake on one job (`python -m snakemake --target-jobs ... --mode remote`). Storage plugins run inside that inner Snakemake. A "native" TES executor with real commands and real TES inputs/outputs was proposed in 2023/2024 but not built. This is exactly the gap iunctus would fill.

### Cited Findings
- Catalog entry: minimum Snakemake >=8.6, author Sven Twardziok, options `--tes-url` (required), `--tes-user`, `--tes-password`, `--tes-token`. Usage: `snakemake --executor tes --default-resources --jobs N ...` ([plugin catalog, TES](https://snakemake.github.io/snakemake-plugin-catalog/plugins/executor/tes.html)).
- Repository: version 0.1.3 in `pyproject.toml`, tags v0.1.1 to v0.1.3, last commit 2024-09-07 ("chore: update to stable snakemake"). Depends on `py-tes ^0.4.2` and `snakemake-interface-executor-plugins ^9.0.0` ([snakemake-executor-plugin-tes](https://github.com/snakemake/snakemake-executor-plugin-tes)).
- Plugin settings: `non_local_exec=True`, `implies_no_shared_fs=True`, `job_deploy_sources=True`, `pass_default_storage_provider_args=True`, `auto_deploy_default_storage_provider=True`, `init_seconds_before_status_checks=40` ([plugin source](https://github.com/snakemake/snakemake-executor-plugin-tes/blob/main/snakemake_executor_plugin_tes/__init__.py)).
- Task shape: one TES executor with image `remote_execution_settings.container_image` and command `["/bin/bash", "/tmp/run_snakemake.sh"]`, with the source comment "TODO: info about what is executed is opaque". The job script is passed by content. Local inputs become `file://` URLs. Files on a storage provider are skipped as TES inputs. All files "must be in the same working directory". Resources: `_cores` to `cpu_cores`, `mem_mb` to `ram_gb` (ceil of /1000), `disk_mb` to `disk_gb` ([plugin source](https://github.com/snakemake/snakemake-executor-plugin-tes/blob/main/snakemake_executor_plugin_tes/__init__.py)).
- Open issues (2026-09-27): #3 "Implement 'native' TES executor" (opened 2023-10, updated 2024-10); #4 "Rename repository/plugin"; #17 "Remove Testing Funnel code"; #18 "Crash on missing GITHUB_WORKSPACE when running outside GitHub Actions" (2026-05-19); #19 "Not declaring task outputs to TES endpoint" (2026-06-30); #21 "Move implementation-specific artifact handling out of core TES error logic" (2026-09-10) ([issues](https://github.com/snakemake/snakemake-executor-plugin-tes/issues)).
- Issue #19: the plugin "generates task JSON with an empty `outputs` array". With Azure Blob storage the task succeeds, but "Job fails with 'missing files after X seconds' timeout" ([issue #19](https://github.com/snakemake/snakemake-executor-plugin-tes/issues/19)).
- Issue #3 lists the costs of wrapping: per-task Snakemake overhead, logs hard to read, "every `tesTask.executors[].command` is a `snakemake` call", the 128 KiB TES limit on content-passed files, and "this executor cannot be used to run any workflows that follow Snakemake's best practices" on directory layout. It proposes that `executors[].command` hold the real command, `image` the real image, and `inputs[]`/`outputs[]` the real files ([issue #3](https://github.com/snakemake/snakemake-executor-plugin-tes/issues/3)).
- Johannes Köster's reply on #3: wrapping gives "increased flexibility (works with any storage plugin, shell, as well as script, notebook and wrapper support, as well as all the different software deployment backends". He prefers the change in core: "an extremely shallow wrapper for handling storage and software deployment that does not need the entire workflow source". On groups: "In such a case, there is really no way around a mini-workflow invocation" ([issue #3](https://github.com/snakemake/snakemake-executor-plugin-tes/issues/3)).
- How a remote job is serialized: `format_job_exec` builds `<prefix> && <env vars> <precommand> && python -m snakemake --snakefile <file> --target-jobs '<rule>:<wildcard>=<value>,...' --allowed-rules <rules> <general args> --mode remote`. The inner Snakemake re-parses the Snakefile and rebuilds the DAG for that one target job ([executor interface real.py](https://github.com/snakemake/snakemake-interface-executor-plugins/blob/main/snakemake_interface_executor_plugins/executors/real.py) lines 82-185, interface version 9.4.0).
- The general arguments include `--force`, `--target-files-omit-workdir-adjustment`, `--max-inventory-time 0`, `--retries 0` ("retries are handled by the main process"), `--nocolor`, `--no-hooks`, `--nolock`, `--ignore-incomplete`, the rerun triggers and the software deployment and storage settings. The precommand may run `pip install --target ... <storage plugin packages>` and `python -m snakemake --deploy-sources <archive> <checksum>` to fetch the workflow source archive from storage ([spawn_jobs.py](https://github.com/snakemake/snakemake/blob/main/src/snakemake/spawn_jobs.py) lines 170-300).
- Storage: `--default-storage-provider s3 --default-storage-prefix s3://mybucket/` maps "all input and output files that are not explicitly mapped" to storage. Storage providers can also be registered in the Snakefile with the `storage` keyword, and several tagged instances of one provider are allowed ([storage docs](https://snakemake.readthedocs.io/en/stable/snakefiles/storage.html)).
- Storage and remote jobs are still being fixed in 9.x: "in remote jobs, upload storage in topological order such that modification dates are preserved" (9.0.0), "temp files not deleted when using remote storage plugins" (9.23.1), "don't delete local storage copies of currently running jobs" (9.26.0) ([CHANGELOG.md](https://github.com/snakemake/snakemake/blob/main/CHANGELOG.md)).

### Inferences
- For iunctus, a Snakemake job should become a "native" TES task as proposed in issue #3: the fully resolved shell command, the rule's container image, TES inputs from storage URLs, and TES outputs to storage URLs (including log and benchmark files). The TES server then handles transfers.
- To do this, iunctus itself must resolve the shell command string (Snakemake's `{input}`, `{output:q}`, `{params.x}`, `{threads}`, `{resources.mem_mb}`, `{wildcards.x}` formatting rules), resolve the container image, and map file paths to storage URLs. No part of the job may need the Snakefile.
- Groups, pipes and services could map to one TES task with several executors that share a working directory (TES supports multiple executors, as noted in issue #3). This keeps "no Snakemake in the container".
- Checkpoint outputs must be readable by iunctus after the task (to re-evaluate the DAG). So iunctus needs its own storage client (for example S3 via a Rust crate) at least for checkpoint outputs and for mtime/existence checks.
- `conda:` directives need a container that can build the environment. A native path is to pre-build images per environment file, or to use a generic micromamba image with a generated setup step as a first TES executor.

### Gaps
- I did not test the TES plugin against a TES server (Funnel, TESK, Planetary TES).
- I did not check how other Snakemake executors (Kubernetes, Google Batch) serialize jobs. They share the same `format_job_exec` base, so they very likely re-invoke Snakemake too, but I did not verify each.

## Q6. Export features and the internal DAG model

### Takeaway
Snakemake has no export that a native engine can consume. `--export-cwl` produces a CWL workflow in which every step runs `snakemake` in the Snakemake container. The internal model is Python classes (`Rule`, `Job`, `GroupJob`, `DAG`, `Checkpoints`) tied to the Python runtime. The debugging outputs (`--print-compilation`, `--dry-run`, `--dag`, `--summary`) are useful as test oracles, not as inputs.

### Cited Findings
- `dag_to_cwl()` builds a CWL `CommandLineTool` with `"baseCommand": "snakemake"`, a `dockerPull` of the Snakemake image and arguments such as `--force`, `--target-files-omit-workdir-adjustment`, `--notemp`, `--no-hooks`, `--nolock`, `--mode subprocess` ([cwl.py](https://github.com/snakemake/snakemake/blob/main/src/snakemake/cwl.py) lines 207-240).
- Docs: "The resulting workflow will by default use the Snakemake docker image for every step", then run with `cwltool workflow.cwl` ([interoperability docs](https://snakemake.readthedocs.io/en/stable/executing/interoperability.html)).
- The `cwl:` rule directive runs an external CWL tool through `cwltool`, which "must be in PATH" ([cwl.py](https://github.com/snakemake/snakemake/blob/main/src/snakemake/cwl.py) lines 21-88).
- Other export and inspection options: `--dag` (dot or mermaid), `--rulegraph`, `--filegraph`, `--d3dag`, `--summary`, `--detailed-summary`, `--list-changes`, `--archive`, `--containerize` (Dockerfile, or apptainer format since 9.17.0), `--generate-unit-tests`, `--lint`, `--report`, `--print-compilation` ([cli.py](https://github.com/snakemake/snakemake/blob/main/src/snakemake/cli.py) lines 997-1320; [CHANGELOG.md](https://github.com/snakemake/snakemake/blob/main/CHANGELOG.md)).
- Internal classes: `Rule`, `Ruleorder`, `RuleProxy` ([rules.py](https://github.com/snakemake/snakemake/blob/main/src/snakemake/rules.py)); `AbstractJob`, `JobFactory`, `Job`, `GroupJobFactory`, `GroupJob`, `Reason` ([jobs.py](https://github.com/snakemake/snakemake/blob/main/src/snakemake/jobs.py)); `DAG`, which implements the executor, report and scheduler DAG interfaces, plus `CandidateGroup` and processors for wrappers, scripts and notebooks ([dag.py](https://github.com/snakemake/snakemake/blob/main/src/snakemake/dag.py)); `Checkpoints`, `Checkpoint`, `CheckpointJob` ([checkpoints.py](https://github.com/snakemake/snakemake/blob/main/src/snakemake/checkpoints.py)).

### Inferences
- iunctus's IR can mirror Snakemake's split: a rule template (patterns and deferred values) and concrete jobs (bound wildcards, resolved files, resolved command). A `Reason` equivalent (why a job must run) is useful for users.
- `--export-cwl` is not a route to "Snakemake via CWL" without Snakemake. Iunctus should not rely on it.

### Gaps
- None critical for this question.

## Q7. Test corpora and conformance testing

### Takeaway
There is a large public set of test workflows. The workflow catalog lists 6,220 repositories (465 "standardized"). Snakemake's own `tests/` folder has about 395 runnable test workflows with expected outputs. About half of those avoid `run:`, scripts, checkpoints, software environments, storage, modules and groups. That half is a natural first conformance suite.

### Cited Findings
- Workflow catalog `data.json` (repository last updated 2026-09-21) has 6,220 entries: 465 marked standardized and 5,755 others. `skips.json` lists 4,785 more repositories that were skipped. Entries by `updated_at` year: 2026: 2,557; 2025: 1,175; 2024: 681; 2023: 511; 2022: 614; 2021: 433; 2020 and earlier: 249. 189 entries list wrappers. Software stack flags: conda 444, apptainer+conda 94, apptainer 82, singularity+conda 40, singularity 25; report 84 (own measurement on [data.json](https://github.com/snakemake/snakemake-workflow-catalog/blob/main/data.json); [catalog site](https://snakemake.github.io/snakemake-workflow-catalog/)).
- The snakemake-workflows GitHub organization has 32 non-archived repositories ([snakemake-workflows](https://github.com/snakemake-workflows)).
- Citations: at publication time the paper reported "on average, more than five new citations per week, and over 700 citations in total" ([Mölder et al. 2021](https://doi.org/10.12688/f1000research.29032.2)). This number is from 2021 and is old.
- Snakemake `tests/` (own measurement): 435 test folders, 424 Snakefiles, 397 folders with `expected-results`, 555 `def test_` functions (396 in `tests.py`). 62 test calls in `tests.py` use `shouldfail`. 15 tests are marked `@connected` (need network). `tests_using_conda.py` has 38 conda-related tests ([tests/](https://github.com/snakemake/snakemake/tree/main/tests)).
- The harness `run()` in `tests/common.py` copies the test folder to a temporary folder, runs the workflow through the Snakemake Python API and compares produced files against `expected-results` (MD5 check by default; `check_md5=False` to relax). It supports expected failures via `shouldfail` ([tests/common.py](https://github.com/snakemake/snakemake/blob/main/tests/common.py)).
- Own measurement, feature classification of the 395 test folders that have both a Snakefile and `expected-results`: `run:` 54, `script:`/`notebook:`/`wrapper:` 37, checkpoints 28, software environments (`conda:`/`container:`/`singularity:`/`envmodules:`) 51, storage 14, `module`/`use rule` 46, groups/pipes/services 28, Python `def`/`lambda` 88, `import` 66. 191 folders use none of run/script/checkpoint/software-env/storage/module/group. 139 of those also have no `def`, `lambda` or `import`.
- snakemake-wrappers has 482 wrapper test Snakefiles under `bio/*/test/` (own measurement; 499 `meta.yaml` files) ([snakemake-wrappers](https://github.com/snakemake/snakemake-wrappers)).
- The recommended workflow layout says "the repository should contain a `.tests` directory with two subdirectories: `.tests/integration` and `.tests/unit`", where the integration folder holds "all the input data, configuration specifications and shell commands required to run an integration test of the whole workflow" ([deployment docs](https://snakemake.readthedocs.io/en/stable/snakefiles/deployment.html)). Many standardized workflows use a `.test` folder for CI instead; I did not measure how many.
- Snakemake's own code is under an MIT-style license ("Permission is hereby granted, free of charge, ...") ([LICENSE.md](https://github.com/snakemake/snakemake/blob/main/LICENSE.md)).

### Inferences
- Snakemake's `tests/` folder can serve as a conformance suite, in the same spirit as the CWL conformance tests. Iunctus needs a small harness that copies a test folder, runs iunctus with the same targets and options as `tests.py`, and compares against `expected-results`. The per-test options (targets, config, flags) live in Python test functions, so a one-time extraction into a manifest (for example YAML) is needed. The MIT-style license allows reuse with attribution.
- The 139 "static shell-only" tests make a good first milestone. The 191 set (allowing helper functions and lambdas) is the second.
- For catalog workflows, the test configs of the 465 standardized workflows give realistic end-to-end tests. Many need conda or containers and large test data, so they suit nightly runs, not unit tests.
- Differential testing: in CI only (never at runtime), run real Snakemake with `--dry-run --printshellcmds`, `--dag` or `--print-compilation`, and compare the job set, wildcards and shell commands with iunctus's plan. This checks semantics without executing tools, and it does not break the "no Snakemake at runtime" rule.

### Gaps
- The catalog counts include forks, toy projects and non-workflow repositories that match the search. I did not measure how many are real, runnable workflows.
- I did not count how many of the 5,755 non-standardized workflows fall into a static subset.

## Q8. A realistic native subset, and how to verify it

### Takeaway
A realistic first target is "shell rules with static or lightly dynamic Python": config access, `expand`, wildcards, simple lambdas and helper functions evaluated by an embedded interpreter, `container:` images, and TES execution with storage URLs. Scripts and wrappers can follow with a generated preamble and a small `snakemake` shim module in the container. `run:` blocks, pandas-heavy DAG code, notebooks, reports and exact metadata compatibility should be out of scope at first.

### Cited Findings
- Shell rules are the most common execution directive in my sample (745 of 1,660 execution directives), then scripts (404) and wrappers (362); `run:` is 145 (own measurement, see Q4).
- Checkpoints appear in 7 of 33 sampled repositories and in 28 of 395 Snakemake test folders (own measurements, see Q4 and Q7).
- 139 of 395 Snakemake test folders need no `run:`, scripts, checkpoints, software environments, storage, modules, groups, `def`, `lambda` or `import` (own measurement, see Q7).

### Inferences
- Tier 1 (static core): parser for all keywords (reject unknown ones with clear errors); `configfile:` (YAML/JSON) and `--config` overrides; `include:` with literal paths; `rule` with `input`/`output`/`log`/`benchmark`/`params`/`threads`/`resources`/`message`/`shell`; literal strings, f-strings, lists, dicts, `expand`, `multiext`, `temp`, `protected`, `directory`, `ancient`, `touch`; `wildcard_constraints`; `ruleorder`; `localrules`; `default_target`; `container:` (global and per rule); `retries`/`priority`; mtime-based rerun; dry run; local executor and TES executor. Verify with the 139-test subset and differential dry runs.
- Tier 2 (embedded interpreter): top-level Python (assignments, loops, `if`, helper `def`, `lambda`, comprehensions, `os.path`, `glob`, `re`, `json`, `pathlib`), input functions and `unpack`, `glob_wildcards`, `lookup`/`branch`/`evaluate`/`collect`, dynamic resources with `attempt`, `use rule`/`module` with local Snakefiles, `checkpoint` with DAG re-evaluation, `group:` mapped to multi-executor TES tasks. Verify with the 191-test subset plus a few catalog workflows.
- Tier 3 (job-time Python and environments): `script:` for Python/R/Bash/Julia/Rust with generated preambles; `wrapper:` via a vendored `snakemake` shim module plus `snakemake_wrapper_utils`; `conda:` via pre-built or on-the-fly environments in containers; `storage` keyword and default storage provider mapped to TES URLs; params/input/code rerun triggers in iunctus's own metadata format. Verify with wrapper tests (482) and standardized catalog workflows.
- Out of scope initially: `run:`, `notebook:`, `template_engine:`, `cwl:`, `pepfile`/`pepschema` (unless peprs is used), `report`, `service`/`pipe`, `handover`, `cache`, `shadow`, `scattergather`, `onstart`/`onsuccess`/`onerror` hooks with Python bodies, `envmodules`, pandas-driven sample sheets (unless an optional CPython mode is built).

### Gaps
- The share of catalog workflows each tier would cover is unknown. A larger scan with a proper parser (for example snakemake-lang or tree-sitter-snakemake) would give real numbers.

## Q9. What a native Rust Snakemake implementation needs: checklist, effort and risk

### Takeaway
A useful subset (Tiers 1 and 2) is feasible. Full Snakemake compatibility without CPython is not, because Snakefiles are arbitrary Python and most curated workflows use pandas at DAG-building time. The biggest risks are the Python evaluation strategy, checkpoint semantics and the moving target of the language.

### Cited Findings
- The facts behind this checklist are cited in Q1 to Q8. The checklist and estimates below are my own inferences.

### Inferences

Checklist:

1. Parsing
   - [ ] Lexer and parser for Snakemake keywords on top of a Python parser (`ruff_python_parser`, pinned), covering every global keyword, rule directive, execution directive and module keyword listed in Q2, including `use rule ... from ... as ... with:` and the new `software:` expression.
   - [ ] Source positions for errors; handling of f-strings and multi-line strings in `shell:`.
   - [ ] A test that compares iunctus's view of each rule with Snakemake's `--print-compilation` output (CI only).
2. Evaluation of DAG-time Python
   - [ ] A native evaluator for a safe subset: literals, f-strings, `%` and `.format`, lists, dicts, comprehensions, `config` access, `expand`, `multiext`, flag functions, `os.path`, simple lambdas over `wildcards`.
   - [ ] An embedded interpreter (Monty or RustPython) for helper functions, with Snakemake builtins exposed as host functions or host objects.
   - [ ] Optional CPython mode (PyO3) behind a build feature for pandas-heavy workflows.
   - [ ] Deferred values: input functions, params functions, dynamic `threads`/`resources` with `attempt`.
   - [ ] Config: YAML/JSON config files, `--config` overrides, `validate()` against JSON Schema.
3. Rule model and DAG
   - [ ] Output pattern to regex with default `.+`, inline, rule and global wildcard constraints.
   - [ ] Target resolution (default first rule, `default_target`, explicit file or rule targets).
   - [ ] Candidate rule selection, `ruleorder`, `AmbiguousRuleException`, cycle detection, missing-input errors.
   - [ ] Checkpoints and incremental DAG re-evaluation after checkpoint jobs finish.
   - [ ] File flags: temp (with deletion), protected, directory, ancient, touch, ensure, local; later pipe, service, update.
   - [ ] Modules and `use rule` with prefixes, name modifiers and config.
   - [ ] Groups, and resource scopes for groups.
4. Execution
   - [ ] Shell command formatting compatible with Snakemake (`{input}`, `{input.name}`, `{output:q}`, `{params.x}`, `{threads}`, `{resources.x}`, `{wildcards.x}`, `{log}`), with the bash strict-mode prefix Snakemake uses.
   - [ ] Script preambles for Python, R, Bash (and later Julia, Rust), plus a small `snakemake` shim module for Python scripts and wrappers.
   - [ ] Wrapper fetching from snakemake-wrappers by version tag, with caching.
   - [ ] Container image per rule; conda environment files mapped to images.
   - [ ] Scheduler with priorities, `--jobs`/`--cores` limits and resource limits; retries.
   - [ ] Local executor and TES executor that sends real commands, real images and real TES inputs/outputs (the "native TES" design in plugin issue #3).
   - [ ] Storage: S3 (and others) URL mapping, existence and mtime checks from Rust, download of checkpoint outputs.
5. State and rerun logic
   - [ ] Own metadata store (incomplete markers, locks, per-output records).
   - [ ] Rerun triggers: mtime first; then input set, resolved command/params and container image.
   - [ ] Benchmark files (the TSV format) if TES or the container can report usage.
6. Verification
   - [ ] Conformance harness over Snakemake's `tests/` (manifest extracted from `tests.py`), starting with the 139-test static subset.
   - [ ] Differential dry-run tests against real Snakemake in CI.
   - [ ] End-to-end runs of selected standardized catalog workflows on a TES test server (for example Funnel).

Effort and risk estimate (rough, for engineers experienced in Rust and workflow engines):

| Scope | Effort | Risk | Notes |
|---|---|---|---|
| Tier 1: parser, static evaluator, DAG, shell rules, local and TES execution | about 4 to 6 person-months | Medium | Well-defined. Main risk is matching wildcard and rule-selection corner cases. |
| Tier 2: embedded interpreter, input functions, modules, checkpoints, groups | about 6 to 9 more person-months | High | Checkpoints and the interpreter API surface are the hard parts. Monty or RustPython gaps may block real workflows. |
| Tier 3: scripts, wrappers, conda, storage, own rerun triggers | about 6 to 12 more person-months | Medium to high | Many small features. Wrapper and conda behavior must match users' expectations. |
| Full parity (run:, notebooks, reports, pandas without CPython, exact metadata) | not realistic without CPython | Very high | Snakefiles are arbitrary Python. Even with PyO3 this is years of catch-up with a fast-moving project. |

Main risks:
- Python evaluation. Without CPython, many curated workflows fail at DAG-building time (27 of 33 sampled import pandas).
- Moving target. Snakemake shipped 27 minor releases in 9.x and has a breaking `software:` change pending.
- Semantics are defined by code and tests, not a specification. Corner cases (ambiguity, checkpoints, groups) need test-driven reverse engineering.
- Job-time Python needs a `snakemake` module in the container. A shim must match what scripts and wrappers use.
- Users expect Snakemake's `.snakemake` state and CLI flags. Iunctus will differ and must document this.

### Gaps
- The effort numbers are judgment calls, not based on a measured comparison. No public re-implementation of Snakemake exists to calibrate against.
