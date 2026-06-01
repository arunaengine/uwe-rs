<!-- Snakemake test workflows for iunctus: how it was built, a Snakefile parser evaluation, and a feature inventory of real workflows and the Snakemake tests -->
<!-- Copyright (c) 2026 The iunctus Contributors -->
<!-- SPDX-License-Identifier: MIT or Apache-2.0 -->

# Snakemake test workflows: parser evaluation and feature inventory

Research date: 2026-09-27. This note builds on [snakemake.md](snakemake.md). That note gives the keyword lists and the execution model. This note measures what real workflows use.

All numbers marked "own measurement" come from scripts in `~/.cache/iunctus/test-workflows/snakemake/tools/`. The raw results are in `~/.cache/iunctus/test-workflows/snakemake/results/`. Section 5 lists what I could not verify.

## Summary

- The test workflows are 464 of the 465 standardized catalog workflows and the `tests/` folder of Snakemake v9.27.0. It is text only: 8,375 text files from the sources (29.2 MB). With the catalog index, the tools and the results it is 38.8 MB.
- 62 of the 464 standardized workflows define no rule. Most are templates. The inventory uses the 399 workflows in which the Snakemake parser found at least one rule. Together they have 10,416 rules and 346,130 lines of Snakemake code.
- Parsers, measured against Snakemake's own `parser.py` on the files each workflow really loads:
  - tree-sitter-snakemake parses 97.5% of those files without error, and all files of 87.6% of the workflows with rules.
  - snakemake-lang (Rust, built on ruff) accepts 67.3% of those files, and all files of 40.0% of the workflows with rules. Its failures come from a line-based scanner. Examples are docstrings in rules, comments after values and multi-line strings.
  - `ruff_python_parser` 0.0.15 parses 3,806 of 3,810 Python programs that `parser.py` generates from Snakefiles. The 4 failures are also rejected by CPython. It parses 2,375 of 2,384 helper `.py` files. CPython rejects the same 9.
- DAG-time Python is the norm. Of the 399 workflows, 70% define functions, 68% use lambdas and 64% import pandas. 49% touch the file system while the DAG is built. Only 32 workflows (8%) are fully static.
- The pandas use is narrow but has a long tail. About 40 pandas names (module functions such as `read_csv` plus DataFrame and Index members) cover about 68% of the pandas workflows. About 95 names cover all of them. This counting is a heuristic.
- Execution: `shell:` in 91% of workflows, `script:` in 66% (Python 54%, R 28%), `wrapper:` in 42%, `run:` in 18%, `notebook:` in 2%. Software: `conda:` in 86% of workflows, containers in 33%.
- Snakemake tests: 391 test folders have a Snakefile and `expected-results`. 210 of them need no Python helpers, no scripts, wrappers or notebooks, no checkpoints and no software environment. 109 test folders remain after also removing `run:`, modules, storage, groups, handlers, PEP, rejected files, expected failures and folders that no test function uses.

## 1. Test workflows

### 1.1 Workflow catalog

- Source: [snakemake-workflow-catalog](https://github.com/snakemake/snakemake-workflow-catalog), `data.json` at commit `caf36c8` (2026-09-21). It has 6,220 entries, and 465 of them have `standardized: true`.
- Fetched: 464 repositories, each at the head of its default branch on 2026-09-27. `kr1stoff/KML-RNASEQ` returns 404 on the GitHub API (deleted or private), so it is missing.
- Method: `git clone --depth 1 --filter=blob:none --no-checkout`. Then file sizes come from the GitHub tree API, and a sparse checkout uses exact paths. The `.git` folders were removed after the copy.
- Files kept:
  - every `Snakefile`, `*.smk` and `*.snakefile`;
  - every `*.py` under `workflow/`;
  - `*.yaml`, `*.yml`, `*.tsv` and `*.csv` under any `config/` folder, up to 200 kB;
  - `.snakemake-workflow-catalog.yml`.
  Three config files were larger and were skipped (in `karltayeb/laplace-bfs` and `r1cheu/imputation`).
- Location: `~/.cache/iunctus/test-workflows/snakemake/catalog/<owner>__<repo>/`. Each folder has `.iunctus-meta.json` with branch, commit and skipped files. The catalog entries themselves are in `catalog-standardized.json`. All commits are in `revisions.txt`.

| Kind | Files | Bytes | Repositories |
|---|---:|---:|---:|
| Snakemake files (outside test folders) | 3,286 | 13,060,642 | 464 |
| Snakemake files in `.test`/`test`/`tests` folders | 27 | 39,498 | 25 |
| Python files under `workflow/` | 2,359 | 11,700,985 | 300 |
| Config YAML (`.yaml`, `.yml`) | 920 | 2,197,609 | 393 |
| Config tables (`.tsv`, `.csv`) | 624 | 1,280,353 | 230 |
| `.snakemake-workflow-catalog.yml` | 462 | 254,352 | 462 |

Own measurement. Config rows include config folders inside test folders.

**Which files count.** Many repositories contain old or unused Snakemake files. So the inventory only uses files that a workflow really loads. I start at the main Snakefile (`workflow/Snakefile`, else `Snakefile`). Then I follow `include:` and local `module` paths. Paths can be literal strings, f-strings with constant parts, `os.path.join`, `workflow.basedir` or simple string variables. This reaches 3,021 of the 3,313 catalog Snakemake files.
- 6 workflows (14 includes) use include paths I could not compute. For them I count every Snakemake file outside test folders.
- 27 include targets in 12 workflows do not exist in the repository or have an unusual name such as `Snakefile_0`.

**Size of the 399 workflows with rules** (own measurement): the median is 16 rules and 461 lines. The 90th percentile is 59 rules and 1,897 lines. The largest has 207 rules and 7,117 lines.

### 1.2 Snakemake tests

- Source: [snakemake/snakemake](https://github.com/snakemake/snakemake) tag `v9.27.0`, commit `ca4a762`. This is the latest release (2026-09-11) according to the GitHub releases API on 2026-09-27.
- Files kept (696 files, 3.0 MB):
  - `Snakefile`, `*.smk` and `*.snakefile` outside `expected-results`;
  - `*.yaml`, `*.yml`, `*.json`, `*.tsv` and `*.csv` outside `expected-results`, up to 100 kB;
  - the top-level harness files `tests/*.py` (`tests.py`, `common.py` and others). I added these because the targets, options and `shouldfail` flags of each test live there.
- `tests-file-listing.tsv` lists all 2,003 files under `tests/` with their sizes. This includes the 872 files (18.2 MB) in `expected-results` folders, which were not downloaded. 393 test folders have an `expected-results` folder.

### 1.3 How to rebuild

1. `tools/fetch_catalog.py` rebuilds the catalog part. It reads `/tmp/iunctus-smk/catalog/data.json`, so clone the catalog repository there first. It uses 4 parallel clones under `nice -n 19`.
2. `tools/fetch_tests.sh [tag]` rebuilds the tests part.
3. For the evaluation, add `src/snakemake/parser.py` of the same tag to `tools/oracle/snakemake/`. Then run `tools/oracle/oracle.py`, `tools/inventory.py catalog|tests`, `tools/tests_classify.py`, `tools/subsets.py`, `tools/pandas_cov.py` and `tools/mdtables.py`.
4. `tools/parse-eval/` is the small Rust program for ruff and tree-sitter. It needs a clone of tree-sitter-snakemake next to it.

The scripts still use the working folder `/tmp/iunctus-smk`.

### 1.4 Limits of the test workflows

- The standardized workflows lean toward the recommended layout. Their authors know Snakemake well. The 5,755 non-standardized catalog entries are not included.
- Heads of default branches are not releases. 177 of the 465 entries name a latest release in `data.json`. I did not fetch those tags.
- Helper `.py` files outside `workflow/` are missing. Some workflows import `scripts.*` modules from other places (see 3.11).

## 2. Parser evaluation

### 2.1 How Snakemake parses a Snakefile

Line numbers refer to [parser.py at v9.27.0](https://github.com/snakemake/snakemake/blob/v9.27.0/src/snakemake/parser.py).

- `parse()` (lines 1384-1407) runs Python's `tokenize.generate_tokens` over the file. It feeds each token to a state machine. The states are `TokenAutomaton` subclasses. The machine yields strings that are joined into one Python program. It also keeps a line map for error messages.
- The top state is `Python` (lines 1293-1346). Before each token it checks whether the token starts a physical line (`is_line_start`, line 25). If so, it looks the token text up in its keyword table (the global keywords listed in [snakemake.md](snakemake.md) Q2). If the text is not a keyword, the token is copied through as Python.
- Each keyword state expects a colon (`KeywordState.colon`, line 202 onward). Then it copies the tokens of the inline value or the indented block. The block ends when indentation drops back or at end of file. Comment-only lines are ignored. The value is placed inside a call:
  - Generic global keywords become `workflow.<name>(<value>)`, for example `workflow.include(...)` and `workflow.configfile(...)`. Some names are mapped, for example global `container` to `workflow.global_container` and `envvars` to `workflow.register_envvars`.
  - Rule property directives become decorators `@workflow.input(<value>)`, `@workflow.output(<value>)` and so on.
  So a directive value is simply the argument list of a Python call. That is why it can contain keyword arguments, lambdas, `*args` and trailing commas.
- `rule` and `checkpoint` (lines 780-876) read an optional name and a colon, then a block. In the block, each line must start with a directive keyword, a comment or a string. A string becomes `@workflow.docstring(...)`. At most one execution directive is allowed, and it must come last (errors at lines 831 and 839). The rule ends with a generated function `def __rule_<name>(input, output, params, wildcards, threads, resources, log, rule, ...)`.
  - For `run:` the block becomes the body of this function (`Run`, lines 571-605).
  - For `shell:`, `script:`, `notebook:`, `wrapper:`, `template_engine:` and `cwl:` the value is used twice (`AbstractCmd`, lines 607-738). It goes into a decorator such as `@workflow.shellcmd(...)`, and into a call such as `shell(<value>, ...)` inside the function body.
  - Rules without an execution directive get `@workflow.norun()`.
- `use rule` (`UseRule`, line 1048) parses these forms:
  - `use rule <names or *> from <module> [exclude <names>] [as <pattern>] [with:]`
  - `use rule <name> as <name> with:`
  It emits `@workflow.userule(rules=..., from_module=..., exclude_rules=..., name_modifier=...)`. The `with:` block takes the rule property directives. `module` (line 953) has its own small keyword table and emits `workflow.module(..., name=...)`.
- `onstart`, `onsuccess` and `onerror` become decorated functions with a Python body.

What happens after parsing matters too:
- The generated program runs with `exec` in one shared dictionary, `workflow.globals` ([workflow.py at v9.27.0](https://github.com/snakemake/snakemake/blob/v9.27.0/src/snakemake/workflow.py), lines 1690-1745). So all included files share one namespace. That is why a helper defined in `common.smk` works in every rule file.
- `include` also puts the Snakefile's folder on `sys.path`. This lets workflows import local modules such as `scripts.common`.
- The shell command is formatted when the job runs. `snakemake.utils.format` (lines 529-560 in `utils.py`) merges the caller's globals and locals with the job's `input`, `output` and other values. So a shell command can use any global variable of the Snakefile.

Own tests of edge cases with this parser (files `k1.smk` to `k4.smk` in `tools/edge-cases/`):
- A line that starts with a keyword name is always read as that keyword. This is true inside function bodies and even inside open brackets. So `report = 1` inside a `def` fails with "Colon expected after keyword report".
- Rules can be defined inside functions, `if` blocks and `for` loops.
- Anonymous rules (`rule:`) work. Their names can come from a `name:` directive with an f-string, as in `for` loops.

### 2.2 Method

The reference is `parser.py` from v9.27.0. It runs unchanged with a small stub for `snakemake.common`. A file counts as parsed when `parser.py` succeeds and CPython 3.14.7 `compile()` accepts the generated program. I ran every tool on all 3,826 Snakemake files: 3,313 catalog files and 513 test files.

- **snakemake-lang**: [cademirch/snakemake-lang](https://github.com/cademirch/snakemake-lang) at `51bf76c` (2026-05-19, 24 commits, not on crates.io). I built the `cli` feature in release mode.
  - "Accepted" means `snakemake-lang check <file>` exits with code 0.
  - "Correct" also requires two things: `snakemake-lang compile` produces Python that CPython accepts, and that Python has the same number of `@workflow.rule(` decorators as the reference output.
  - No file caused a panic. A minimal file with `report = 1` inside brackets does panic (`src/parser/global.rs:23`).
- **tree-sitter-snakemake**: [osthomas/tree-sitter-snakemake](https://github.com/osthomas/tree-sitter-snakemake) at `6801043` (grammar 2.1.0, last commit on `main` 2025-09-18). I used the Rust bindings from the clone and `tree-sitter` 0.25.
  - "Accepted" means the tree has no `ERROR` or `MISSING` node.
  - "Correct" also requires the same number of `rule_definition` plus `checkpoint_definition` nodes as the reference rule count.
- **ruff_python_parser** 0.0.15 from crates.io (2026-09-24, MIT). It is not a Snakefile parser, so I tested it on the Python parts:
  - all `.py` files of the test workflows;
  - the Python that `parser.py` generates from each Snakefile;
  - each Snakefile read as plain Python.

### 2.3 Results

Files (own measurement):

| File set | Files | Reference parser | snakemake-lang accepted | snakemake-lang correct | tree-sitter accepted | tree-sitter correct |
|---|---:|---:|---:|---:|---:|---:|
| Catalog, all Snakemake files | 3,313 | 3,300 (99.6%) | 2,213 (66.8%) | 2,154 (65.0%) | 3,210 (96.9%) | 3,206 (96.8%) |
| Catalog, loaded files only | 3,021 | 3,014 (99.8%) | 2,034 (67.3%) | 2,005 (66.4%) | 2,945 (97.5%) | 2,943 (97.4%) |
| Tests, loaded files only | 506 | 498 (98.4%) | 360 (71.1%) | 329 (65.0%) | 474 (93.7%) | 473 (93.5%) |

"Correct" counts only files that the reference also parses.

Workflows in which every loaded file passes (own measurement):

| Set | Workflows | Reference parser | snakemake-lang accepted | snakemake-lang correct | tree-sitter accepted |
|---|---:|---:|---:|---:|---:|
| Catalog, all fetched | 464 | 458 | 222 (47.8%) | 212 (45.7%) | 413 (89.0%) |
| Catalog, with at least one rule | 402 | 397 | 161 (40.0%) | 153 (38.1%) | 352 (87.6%) |
| Test folders, with at least one rule | 419 | 415 | 296 (70.6%) | 278 (66.3%) | 393 (93.8%) |

Some test folders are broken on purpose, because they test error messages.

The reference parser rejects 7 loaded catalog files in 6 workflows. The reasons are:
- inconsistent tabs and spaces (2 files);
- a hyphen in a rule name, or a rule without a name;
- a bad line continuation;
- the removed `subworkflow` keyword;
- `shell:` without a string.

These workflows do not run on Snakemake 9.27.0 either.

`ruff_python_parser` 0.0.15 (own measurement):

| Input | Files | Parsed | CPython 3.14 `compile()` |
|---|---:|---:|---:|
| `.py` files (catalog `workflow/` and test harness) | 2,384 | 2,375 | 2,373 |
| Python generated by `parser.py` from Snakefiles | 3,810 | 3,806 | 3,805 |
| Snakefiles read as plain Python | 3,826 | 363 | not measured |

- The 9 `.py` failures are Python 2 `print` statements and plain syntax errors. CPython rejects them too.
- Ruff accepts 3 programs that CPython rejects: 2 `.py` files with "'return' outside function" and 1 generated program with "'continue' not properly in loop". Ruff's parser leaves such checks to a later pass.
- All 363 Snakefiles that parse as plain Python contain no rule. These are files with helper code only, such as many `common.smk` files. So ruff alone cannot read Snakefiles, but it handles all the Python inside them.

### 2.4 snakemake-lang

snakemake-lang does its own line-by-line scan for Snakemake constructs. It hands text spans to ruff's `parse_unchecked` (doc comment in `src/parser/mod.rs`). It does not use ruff's tokens to find block ends. Most failures come from this design. Failure causes on loaded catalog files that the reference parses (own measurement, by first error message):

| Cause | Files |
|---|---:|
| Docstring inside a rule ("unknown directive") | 348 |
| Comment after a directive value on the same line ("unexpected EOF") | 269 |
| `localrules:` or `ruleorder:` with the names in an indented block | 107 |
| Multi-line string with lines indented less than the directive (tabs or column 0) | 61 |
| Comment or code after a `run:` block at directive indentation, and similar indentation cases | 56 |
| `use rule a as b with:` without `from` | 50 |
| Snakemake statement after `else:` and similar ("Expected a statement") | 30 |
| `if` with a condition over several lines, followed by rules | 22 |
| `singularity:` rule directive not known | 14 |
| Other | 25 |

In the tests, anonymous rules (`rule:`) are the largest cause (43 files).

Among files it accepts, 86 compile to invalid Python. Two examples:
- `use rule * from m as m_*` becomes `def __userule_m_*():`.
- A trailing comment inside `shell:` swallows the closing arguments of the generated `shell(...)` call.

The code is well organized, and each bug looks fixable. But the project has had no commit since 2026-05-19. Its `Cargo.toml` names `github.com/snakemake/snakemake-lang` as the repository, but that address returns 404. Its "real-world validation" milestone is not done (`PLAN.md`, milestone 6). It pins ruff by git tag `0.14.0`.

### 2.5 tree-sitter-snakemake

tree-sitter-snakemake extends tree-sitter-python. It handles most files and all common keywords. It is fast: 3,826 files in 1.9 s on one core. Failure causes on loaded files that the reference parses (own measurement):

| Cause | Catalog files | Test files |
|---|---:|---:|
| Attribute access inside braces in a string, such as `"{u.sample}"` in `expand` | 47 | 2 |
| `version` used as a variable name (the grammar treats it as a keyword) | 12 | 0 |
| `str.format` on a directive string with `{0}` or `{{...}}` | 4 | 2 |
| Python keyword as a rule name (`rule import:`) | 2 | 0 |
| `storage` directive | 0 | 4 |
| `module` blocks: no name, dynamic name, module in a loop, dictionary values | 0 | 15 |
| Other | 6 | 2 |

tree-sitter gives a concrete syntax tree for editors. It does not give a Python AST that an evaluator can use. It also parses wildcard braces inside strings, which Snakemake itself does not do at parse time. GitHub reports a push on 2026-05-12, but the newest commit on any branch is from 2025-09-18. I could not see what the push changed.

### 2.6 What a Rust Snakefile parser must do

These requirements come from `parser.py` and the failure lists above.

1. **Tokenize like Python.** It must handle `INDENT`/`DEDENT`, logical and physical newlines, comments, triple-quoted strings, PEP 701 f-strings, line continuations and tab errors. Block ends must come from tokens, not from raw line indentation. Otherwise multi-line strings and trailing comments break, as they do in snakemake-lang. `ruff_python_parser::lexer::lex` is public in 0.0.15 and gives these tokens, including `Indent` and `Dedent`.
2. **Find keywords at line starts, at any depth.** Rules sit inside `if`/`else` (662 rules in 70 workflows), inside `for` loops (15 rules in 5 workflows), inside `try` (tests) and even inside functions. The parser must allow Snakemake statements inside Python compound statements.
3. **Parse each directive value as a Python call argument list.** This covers positional and keyword arguments, `*args`, lambdas, comments and trailing commas. One way is to parse the span as the arguments of a synthetic call with ruff, keeping the original offsets.
4. **Implement the keyword tables of v9.27.0.** These are 27 global keywords, 24 rule property directives, 7 execution directives, 8 module keywords and the `use rule` grammar. It must also support anonymous rules, rule docstrings, `ruleorder: a > b`, `localrules:` inline or as a block, `storage <tag>:`, and deprecation errors for `subworkflow` and `version`.
5. **Parse `run:`, `onstart:`, `onsuccess:` and `onerror:` bodies as Python statements.** The `run:` body sees `input`, `output`, `params`, `wildcards`, `threads`, `resources`, `log` and `rule` as local names.
6. **Keep one namespace for all included files.** Also record source positions for every value and statement.

There are two ways to shape the output. One is to emit "virtual Python", as `parser.py` and snakemake-lang do, and parse it with ruff into one AST. The other is a mixed AST of Python and Snakemake nodes. The first is easier to check against the reference: compare the rule count and the directive set per rule. The harness in `tools/` already compares rule counts over 3,826 files.

### 2.7 Assessment (inference)

- Use `ruff_python_parser` and its lexer as the base, and port the `parser.py` state machine to Rust. The Python part is solved: ruff parses everything that CPython parses in these test workflows. Pin the exact version. The crate says it is "an internal component crate of Ruff". It had 15 releases (0.0.0 to 0.0.15) between 2026-06-23 and 2026-09-24, and it needs Rust 1.96.
- snakemake-lang is the closest existing work, but it is not ready. It fails a third of the loaded files. Its fixes are known (use tokens for block ends, add docstrings, local `use rule`, anonymous rules and `singularity`). Iunctus could contribute these fixes upstream or reuse its structure.
- tree-sitter-snakemake is not a good base for execution, because it has no Python AST. It could still serve editor features later.
- Keep `parser.py` as an oracle that runs only in CI. In my reading this fits decision 1, because it never runs as part of iunctus.

## 3. Feature inventory of the catalog

**Method.** `parser.py` was instrumented so that every keyword it recognizes is recorded with its context (global, rule, `use rule ... with:` or module). The generated Python was then walked with Python's `ast` module:
- Decorators give the directive values of each rule.
- The `run:` function bodies and the handler bodies are job-time code.
- Everything else is DAG-time code: top-level statements, helper functions, and directive values other than the command itself.

Only loaded files count (see 1.1). "Occurrences" counts every use. "Workflows" counts workflows with at least one use, out of the 399 with rules. Tables list items used by at least one workflow. Full tables are in `results/inventory-catalog.json`.

### 3.1 Global keywords

| Keyword | Occurrences | Workflows | Share |
|---|---:|---:|---:|
| `rule` | 10,320 | 399 | 100% |
| `include` | 2,587 | 356 | 89% |
| `configfile` | 434 | 356 | 89% |
| `wildcard_constraints` | 140 | 128 | 32% |
| `onerror` | 107 | 107 | 27% |
| `onsuccess` | 107 | 107 | 27% |
| `report` | 99 | 97 | 24% |
| `onstart` | 92 | 91 | 23% |
| `localrules` | 207 | 63 | 16% |
| `checkpoint` | 99 | 58 | 15% |
| `container` | 54 | 54 | 14% |
| `use` (`use rule`) | 174 | 50 | 13% |
| `ruleorder` | 69 | 39 | 10% |
| `containerized` | 37 | 37 | 9% |
| `module` | 32 | 21 | 5% |
| `pathvars` | 15 | 15 | 4% |
| `envvars` | 11 | 9 | 2% |
| `pepfile` | 8 | 7 | 2% |
| `scattergather` | 7 | 7 | 2% |
| `workdir` | 6 | 6 | 2% |
| `conda` | 4 | 4 | 1% |
| `inputflags` | 3 | 3 | 1% |
| `singularity` | 2 | 2 | 1% |
| `pepschema` | 1 | 1 | 0% |
| `subworkflow` (removed in 8.0) | 1 | 1 | 0% |

Not used by any workflow: `storage`, `outputflags` and `resource_scopes`. The handlers are mostly `print` calls. Over all 464 workflows, 271 of 306 handler bodies (95 workflows) call `print`, and 46 bodies (22 workflows) call `shell`.

### 3.2 Rule directives

| Directive | Occurrences | Workflows | Share |
|---|---:|---:|---:|
| `input` | 9,707 | 398 | 100% |
| `output` | 9,610 | 397 | 99% |
| `params` | 5,318 | 376 | 94% |
| `shell` | 5,618 | 364 | 91% |
| `log` | 8,043 | 348 | 87% |
| `conda` | 6,433 | 345 | 86% |
| `threads` | 3,770 | 308 | 77% |
| `script` | 2,208 | 263 | 66% |
| `resources` | 3,065 | 220 | 55% |
| `wrapper` | 1,458 | 168 | 42% |
| `default_target` | 107 | 103 | 26% |
| `message` | 1,265 | 95 | 24% |
| `benchmark` | 1,554 | 93 | 23% |
| `run` | 198 | 72 | 18% |
| `wildcard_constraints` | 383 | 65 | 16% |
| `cache` | 189 | 52 | 13% |
| `container` | 830 | 45 | 11% |
| `priority` | 222 | 37 | 9% |
| `retries` | 167 | 33 | 8% |
| `localrule` | 188 | 32 | 8% |
| `group` | 224 | 20 | 5% |
| `shadow` | 81 | 19 | 5% |
| `singularity` | 37 | 11 | 3% |
| `envmodules` | 137 | 7 | 2% |
| `notebook` | 97 | 6 | 2% |
| `name` | 20 | 5 | 1% |
| `template_engine` | 5 | 5 | 1% |

Not used: `cwl`, `handover`, `containerized` (as a rule directive) and rule-level `pathvars`. `software:` exists only on `main` and is not in 9.27.0.

Rules by kind: 10,317 rules and 99 checkpoints. 21 rules in 5 workflows are anonymous. The `use rule` statements split into two kinds:
- 114 local inheritances (`use rule a as b with:`) in 36 workflows;
- 60 imports from modules in 21 workflows, 27 of them `use rule *`.

`with:` blocks mostly override `output` (38 workflows), `log` (33), `input` (28) and `params` (24).

### 3.3 Execution directives, scripts and wrappers

| Execution per rule | Rules | Share of 10,416 rules | Workflows |
|---|---:|---:|---:|
| `shell` | 5,616 | 53.9% | 364 (91%) |
| `script` | 2,208 | 21.2% | 263 (66%) |
| `wrapper` | 1,458 | 14.0% | 168 (42%) |
| none (target rules such as `all`) | 834 | 8.0% | 376 (94%) |
| `run` | 198 | 1.9% | 72 (18%) |
| `notebook` | 97 | 0.9% | 6 (2%) |
| `template_engine` | 5 | 0.05% | 5 (1%) |

- **Script languages** (by file extension): `.py` 1,365 scripts in 214 workflows, `.R`/`.r` 709 in 113, `.sh` 63 in 21, `.Rmd` 52 in 16, `.xsh` 7 in 1, `.rs` 2 in 2. 10 script paths in 4 workflows are computed. Notebooks are all `.ipynb`.
- **Wrappers**: 1,363 references with a version tag (158 workflows), 40 URL or `file:` references (11), 46 computed (7) and 9 on `master` (1). There are 184 distinct wrapper names. The most used are `bio/fastqc` (75 workflows), `bio/multiqc` (73), `bio/samtools/index` (48), `bio/samtools/faidx` (45), `bio/bwa/index` (38), `bio/reference/ensembl-sequence` (35) and `bio/fastp` (33).
- **`run:` bodies**: only 13 of 198 contain nothing but `shell()` calls. The rest run other Python code. Their imports include `pandas` (7 workflows), `os` (8) and `xarray` (3).

### 3.4 File flags

Counted inside directive values.

| Flag in directive | Occurrences | Workflows | Share |
|---|---:|---:|---:|
| `directory` in `output` | 744 | 195 | 49% |
| `temp` in `output` | 2,025 | 177 | 44% |
| `multiext` in `output` | 219 | 114 | 29% |
| `report` in `output` | 1,055 | 111 | 28% |
| `unpack` in `input` | 242 | 77 | 19% |
| `touch` in `output` | 348 | 58 | 15% |
| `multiext` in `input` | 119 | 48 | 12% |
| `ancient` in `input` | 136 | 23 | 6% |
| `protected` in `output` | 51 | 23 | 6% |
| `pipe` in `output` | 25 | 12 | 3% |
| `ensure` in `output` | 30 | 10 | 3% |
| `directory` in `params` | 30 | 7 | 2% |
| `update` in `output` | 79 | 4 | 1% |
| `repeat` in `benchmark` | 39 | 3 | 1% |
| `temp` in `log` | 74 | 2 | 1% |
| `storage` in `input` | 32 | 2 | 1% |
| `access.random` in `input` | 21 | 2 | 1% |
| `local` in `input` | 4 | 3 | 1% |

`service` is used by no catalog workflow and by 1 test folder. `before_update`, `from_queue` and `flag` appear only in the tests.

### 3.5 Input functions, lambdas and other value kinds

Each directive argument was classified by its top-level form. A "function reference" is a name that is defined as a function (`def` or an assigned `lambda`) in one of the workflow's loaded files. A "user function call" is a call to such a function when the rule is defined.

| Kind of value | `input` workflows | `params` workflows | `output` workflows | `resources` workflows | `threads` workflows |
|---|---:|---:|---:|---:|---:|
| string literal | 299 | 261 | 299 | 41 | 0 |
| number or other constant | 0 | 123 | 0 | 156 | 249 |
| `expand(...)` | 293 | 4 | 58 | 0 | 0 |
| function reference (input function) | 187 | 89 | 1 | 16 | 5 |
| `lambda` | 162 | 231 | 0 | 49 | 12 |
| `rules.<name>.output...` | 142 | 1 | 0 | 0 | 0 |
| `config[...]` | 114 | 244 | 11 | 34 | 68 |
| `unpack(...)` | 77 | 0 | 0 | 0 | 0 |
| user function call | 92 | 64 | 21 | 6 | 12 |
| f-string | 63 | 36 | 59 | 0 | 0 |
| conditional expression (`a if c else b`) | 54 | 50 | 22 | 0 | 0 |
| `lookup(...)` | 10 | 43 | 1 | 2 | 1 |
| `branch(...)` | 16 | 4 | 3 | 0 | 0 |

- 272 workflows (68%) use an input function in `input:`: a function reference, a lambda or `unpack`.
- 254 (64%) use a function or lambda in `params:`.
- 64 (16%) use one in `resources:` or `threads:`.
- Lambdas appear in 270 workflows (4,154 lambdas in DAG-time code).
- `rules.<name>` references appear 2,104 times in 145 workflows (36%).

### 3.6 Snakemake helper functions in directive values

| Helper | Occurrences | Workflows | Share |
|---|---:|---:|---:|
| `expand` | 3,025 | 314 | 79% |
| `lookup` | 389 | 58 | 15% |
| `branch` | 46 | 19 | 5% |
| `collect` | 97 | 9 | 2% |
| `subpath` | 21 | 8 | 2% |
| `evaluate` | 10 | 5 | 1% |
| `exists` | 5 | 3 | 1% |
| `glob_wildcards` | 1 | 1 | 0% |
| `parse_input` | 1 | 1 | 0% |

Anywhere in DAG-time code: `expand` is called 4,256 times in 342 workflows (86%), `glob_wildcards` 74 times in 30, `min_version` in 178 and `validate` in 174. `workflow.source_path` appears in 53 workflows, `workflow.basedir` in 46 and `workflow.cores` in 21.

### 3.7 Checkpoints, modules, scatter-gather and wildcard constraints

- **Checkpoints**: 99 checkpoint rules in 58 workflows (15%). `checkpoints.<name>.get(...)` is called 183 times in 52 workflows. Only 3 of these calls sit directly in a directive value. The rest are inside input functions.
- **Modules**: 32 `module` statements in 21 workflows (5%). The `snakefile:` values:
  - `github(...)` in 16 workflows;
  - `config.get(...)` in 3;
  - a local path in 2;
  - `Path(...)` in 1.
  4 workflows use `meta_wrapper`. `use rule` of any kind appears in 50 workflows (13%).
- **Scatter-gather**: `scattergather:` in 7 workflows, with `scatter.<name>()` and `gather.<name>()` calls in the same 7.
- **Wildcard constraints**:
  - global `wildcard_constraints:` 140 times in 128 workflows;
  - rule-level 383 times in 65 workflows;
  - inline `{name,regex}` in patterns 68 times in 16 workflows;
  - some form in 170 workflows (43%).
- **Rules inside Python control flow**: 662 rules in `if` blocks (70 workflows) and 15 in `for` loops (5 workflows).

### 3.8 Software environments

- `conda:` is used in 345 workflows (86%). Most values are env file paths: 5,899 uses in 320 workflows. There are 470 computed values in 31 workflows and 61 named environments in 8.
- Container images appear in 133 workflows (33%) in some form:
  - rule `container:` in 45 workflows (318 `docker://` strings, 546 computed values);
  - `singularity:` in 11;
  - global `container:` in 54;
  - `containerized:` in 37.
- `envmodules:` appears in 7 workflows.

### 3.9 Shell command fields

These are the format fields found in literal `shell:` strings, counted in workflows.

| Field | Workflows | Field | Workflows |
|---|---:|---|---:|
| `{input.x}` | 309 | `{log.x}` | 55 |
| `{params.x}` | 308 | `{resources.x}` | 49 |
| `{output.x}` | 277 | other global names | 36 |
| `{log}` | 244 | any `:q` quoting | 20 |
| `{output}` | 234 | `{output[i]}` | 18 |
| `{input}` | 222 | `{config[...]}` | 14 |
| `{threads}` | 202 | `{rule}` | 6 |
| `{wildcards.x}` | 177 | f-string commands | 4 |

"Other global names" means a field like `{SCRIPTDIR}` that names a Snakefile global. It works because the formatter sees the module globals (see 2.1).

### 3.10 Config access patterns

| Pattern | Occurrences | Workflows | Share |
|---|---:|---:|---:|
| any use of `config` | 17,398 | 381 | 95% |
| `config["key"]` with a constant key | 14,068 | 351 | 88% |
| `validate(config, schema)` | 178 | 171 | 43% |
| `config.get(...)` | 1,659 | 103 | 26% |
| `config["a"].get(...)` (nested) | 1,792 | 78 | 20% |
| `lookup(within=config, ...)` | 307 | 58 | 15% |
| `"x" in config` or `"x" in config[...]` | 277 | 54 | 14% |
| `.keys()` on a nested config value | 139 | 36 | 9% |
| assignment into `config[...]` | 83 | 23 | 6% |
| `.items()` on a nested config value | 58 | 20 | 5% |
| `config[<computed key>]` | 34 | 16 | 4% |
| `config.pop` | 25 | 8 | 2% |
| `config.setdefault` | 22 | 3 | 1% |

- `configfile:` is used in 356 workflows (89%).
- The deepest subscript chain per workflow (`config["a"]["b"]...`) is 1 level in 129 workflows, 2 in 107, 3 in 82, 4 in 33, 5 in 4 and 7 in 1.
- `{config[...]}` inside shell strings appears in 14 workflows.

### 3.11 Top-level imports

These are imports in DAG-time code, counted in workflows.

| Module | Workflows | Module | Workflows |
|---|---:|---|---:|
| `snakemake` | 271 | `csv` | 20 |
| `pandas` | 251 | `scripts` (local package) | 20 |
| `os` | 209 | `math` | 19 |
| `pathlib` | 76 | `itertools` | 16 |
| `glob` | 73 | `subprocess` | 15 |
| `sys` | 70 | `gzip`, `shutil`, `typing` | 13 each |
| `yaml` | 69 | `collections` | 12 |
| `re` | 54 | `socket`, `logging`, `platform` | 8 to 9 |
| `datetime` | 31 | `snakemake_interface_executor_plugins` | 5 |
| `numpy` | 24 | `peppy` | 2 |
| `json` | 23 | `polars`, `pysam`, `Bio` | 1 to 2 |

- Imports from `snakemake`: `snakemake.utils.validate` (193 workflows), `snakemake.utils.min_version` (180), `snakemake.logging.logger` (16), `snakemake.shell.shell` (7) and `snakemake.exceptions.WorkflowError` (5).
- 5 workflows still import `snakemake.remote.HTTP`, which was removed in 8.0.
- Only 123 of the 399 workflows import nothing at DAG time besides the standard library, `yaml` and `snakemake`. After `pandas`, the most common other imports are `numpy` (24), local helper packages such as `scripts` (20 workflows), `utils` (3) and `helper_functions` (3), and `psutil` (3).

### 3.12 pandas

pandas is imported in 254 workflows (64%). I tracked names assigned from pandas calls, and names derived from them, across all loaded files of a workflow. Then I counted member access on those names. This is a heuristic:
- It misses DataFrames passed as function arguments.
- It can count list methods on values derived from pandas.
- 19 workflows import pandas with no detected use.

Module functions (own measurement):

| Function | Occurrences | Workflows |
|---|---:|---:|
| `pd.read_csv` | 335 | 187 |
| `pd.read_table` | 95 | 47 |
| `pd.DataFrame` | 47 | 24 |
| `pd.isnull` | 35 | 23 |
| `pd.isna` | 46 | 18 |
| `pd.concat` | 19 | 11 |
| `pd.notna` | 16 | 8 |
| `pd.merge` | 7 | 7 |
| `pd.Series` | 9 | 6 |
| `pd.unique` | 25 | 3 |
| `pd.read_excel`, `pd.read_parquet` | 3 each | 2 each |
| `pd.to_numeric`, `pd.wide_to_long` | 2, 1 | 1 each |

Members used on pandas objects, as names in the pandas 3.0.6 API (DataFrame, Series, Index, MultiIndex, `.str`, group-by), counted in workflows:
- `loc` 118, `index` 102, `tolist` 69, `unique` 45, `dropna` 31, `columns` 28, `itertuples` 26, `str` 25, `set_index` 21, `apply` 19, `to_dict` 19, `values` 18, `iterrows` 18, `iloc` 17, `astype` 16;
- `shape` 14, `drop_duplicates` 14, `levels` 14, `set_levels` 14, `to_list` 12, `groupby` 11, `all` 11, `isin` 10, `replace` 10, `isnull` 10, `reset_index` 8, `sort_values` 7, `duplicated` 7, `query` 5.

Column access by attribute (for example `units.fq1`) appears in 61 workflows.

Coverage: I added API names one at a time, always picking the name that completes the most workflows (a greedy choice).

| Number of API names | pandas workflows fully covered (of 254) |
|---:|---:|
| 10 | 100 (39%) |
| 20 | 126 (50%) |
| 40 | 172 (68%) |
| 60 | 204 (80%) |
| 80 | 231 (91%) |
| 95 | 254 (100%) |

The common pattern is to read a TSV sample sheet, set an index, and use `.loc[...]` lookups, `.index`, `.unique()` and `.tolist()`. The `set_levels`/`levels` pair comes from one line that many workflows copied: `units.index = units.index.set_levels([i.astype(str) for i in units.index.levels])`.

### 3.13 Python constructs at DAG-building time

Counted in top-level code, helper functions and directive values. Rule bodies and handlers are excluded.

| Construct | Occurrences | Workflows | Share |
|---|---:|---:|---:|
| top-level assignment | 4,946 | 371 | 93% |
| top-level call statement (not `workflow.*`) | 1,800 | 303 | 76% |
| `def` | 3,500 | 281 | 70% |
| `lambda` | 4,154 | 270 | 68% |
| f-string | 6,636 | 249 | 62% |
| list comprehension | 1,252 | 188 | 47% |
| conditional expression | 1,433 | 173 | 43% |
| top-level `if` | 1,580 | 160 | 40% |
| `for` inside functions | 706 | 150 | 38% |
| `raise` | 644 | 118 | 30% |
| `"...".format(...)` | 1,097 | 90 | 23% |
| top-level `for` | 196 | 77 | 19% |
| `with` (inside functions) | 155 | 72 | 18% |
| generator expression | 159 | 67 | 17% |
| `try` (inside functions) | 106 | 60 | 15% |
| top-level `with` | 83 | 54 | 14% |
| dict comprehension | 108 | 53 | 13% |
| `assert` | 143 | 48 | 12% |
| top-level `try` | 48 | 23 | 6% |
| nested `def` | 56 | 22 | 6% |
| `%` formatting | 86 | 12 | 3% |
| `class` | 13 | 10 | 3% |
| `yield` | 18 | 9 | 2% |
| `del` | 52 | 8 | 2% |
| decorated `def` | 14 | 7 | 2% |
| `global` | 10 | 6 | 2% |
| `while` (9 in functions, 1 at top level) | 10 | 5 | 1% |
| `match` | 5 | 4 | 1% |
| `:=` | 3 | 1 | 0% |

- **Side effects while the DAG is built.** These are calls such as `glob.glob`, `os.listdir`, `os.path.exists`, `os.path.isfile`, `open`, `glob_wildcards` and `Path.glob`/`.exists`. They appear in 197 workflows (49%). `subprocess`, `os.system` or `shell()` run at DAG time in 26 workflows (7%). Network access appears in 1.
- **Most called functions** at DAG time, in workflows: `expand` 342, user functions 222, `directory` 196, `temp` 180, `min_version` 178, `validate` 174, `len` 133, `list` 132, `.append` 129, `str` 126, `.get` 125, `multiext` 124, `os.path.join` 123, `report` 111, `open` 110, `ValueError` 93, `print` 80, `int` 78, `unpack` 77, `set` 77, `os.path.dirname` 74, `Path` 72, `lookup` 61, `glob.glob` 38, `yaml.safe_load` 30, `os.listdir` 28, `sys.exit` 24, `logger.info` 21.

### 3.14 Resources

The most used resource keys, in workflows:
- `mem_mb` 173, `runtime` 69, `mem` 22, `time` 20, `tmpdir` 19, `cpus_per_task` 11, `partition` 9, `disk_mb` 7, `time_min` 7, `gpu` 5, `slurm_partition` 3.

Resource values are constants in 156 workflows, lambdas in 49 and function references in 16.

### 3.15 How many workflows each feature set leaves

These are the 399 workflows with rules (own measurement, `tools/subsets.py`).

| Workflows that avoid ... | Workflows |
|---|---:|
| pandas | 145 (36%) |
| pandas and `run:` | 126 |
| pandas, `run:` and checkpoints | 114 (29%) |
| pandas, `run:`, checkpoints and modules/`use rule` | 102 |
| ... and also DAG-time subprocess calls | 97 |
| `def` and `lambda` | 69 (17%) |
| pandas, `def`, `lambda`, `class`, checkpoints, `run:`, DAG-time file or process access, top-level `for` | 32 (8%) |
| the same, and also `script:`, `wrapper:` and `notebook:` | 17 (4%) |

## 4. Snakemake tests: which need no Python helpers, scripts, checkpoints or software environments

**Method.** I used the same instrumented parser, starting at each test folder's `Snakefile` (or `workflow/Snakefile`) and following includes and local modules. Test functions in `tests/*.py` were matched to folders through their `dpath("...")` arguments. A folder counts as an expected failure if any `run(dpath(...), shouldfail=True)` call names it. Results are in `results/tests-classified.json`.

- 431 test folders have a main Snakefile. 391 of them also have `expected-results`. 356 of those 391 are used by a test function. 42 folders are used with `shouldfail=True`.
- The table below counts, among the 391 folders with `expected-results`, how many need each feature.

| Feature | Test folders |
|---|---:|
| Python helpers: `def`, `lambda`, `class` or an import other than `snakemake` | 123 |
| the same, counting `snakemake` imports too | 135 |
| `run:` | 53 |
| `script:`, `wrapper:`, `notebook:`, `template_engine:` or `cwl:` | 39 |
| checkpoints | 28 |
| software environment (`conda`, `container`, `singularity`, `envmodules`, `containerized`) | 52 |
| `module` or `use rule` | 45 |
| `group`, `pipe` or `service` | 28 |
| storage | 9 |
| `onstart`/`onsuccess`/`onerror` | 7 |
| `pepfile`/`pepschema` | 3 |
| a file the reference parser rejects (error tests) | 6 |

Results:

| Test folders with `expected-results` that need none of ... | Count |
|---|---:|
| Python helpers, scripts/wrappers/notebooks, checkpoints, software environments | 210 |
| the above and `run:` | 189 |
| the above and modules, storage, groups/pipes/services, handlers, PEP, rejected files | 135 |
| the above, used by a test function, and not an expected failure | 109 |

Of the 189 folders in the second row, 180 import nothing at all, not even `snakemake`.

The 109 folders are listed in `results/tests-first-suite.txt`. 45 of their test functions carry `@skip_on_windows`. A few need S3 (2), `strace` (1), conda (1) or network (1). These should be removed by hand before the list becomes a first conformance suite.

These numbers differ a little from [snakemake.md](snakemake.md) Q7, which reported 191 and 139. That note used regular expressions over all files in a folder. This note uses the parser and only the loaded files, and it counts `snakemake` imports separately.

## 5. Gaps and things I could not verify

- **Branch heads, not releases.** The catalog snapshot uses branch heads. Some workflows may be broken at head but fine at their release tag.
- **Python version.** The reference parser ran on CPython 3.14.7. Snakemake 9.27.0 supports Python 3.11 and newer. Tokenizer behavior for f-strings and tabs differs between Python versions. I did not repeat the run on 3.11 to 3.13.
- **Include resolution.** It is static. 6 workflows use computed include paths, and for them all non-test files count. Conditional includes (inside `if`) count as always loaded.
- **Module snakefiles.** Remote module snakefiles (`github(...)`, 16 workflows) were not fetched. The rules those workflows import from other repositories are not in the counts.
- **"Correct" for snakemake-lang and tree-sitter.** It only compares rule counts and Python validity. I did not compare every directive value with the reference output.
- **pandas.** The member counts are a heuristic (see 3.12). They are a lower bound on the needed API surface. They do not show which arguments are used (for example `sep="\t"`, `dtype=str`, `comment="#"`).
- **Job-time scripts.** I did not scan the Python or R scripts that `script:` runs. They run inside containers, so they matter less for the parser. They matter for the `snakemake` object that the scripts expect.
- **The unexplained push.** I could not find what the 2026-05-12 push to tree-sitter-snakemake changed. No branch has a newer commit than 2025-09-18.

## 6. Inferences for iunctus

These are my own conclusions, not measured facts.

1. **Parser.** Build it on `ruff_python_parser` (lexer and parser, exact version pinned). Port the `parser.py` state machine. Test it in CI against `parser.py` with the harness in `tools/`. The goal is the reference result on all 3,021 loaded catalog files.
2. **A Python evaluator is required.**
   - A static evaluator reaches only 8% of the catalog workflows.
   - A Python subset with `def`, `lambda`, comprehensions, f-strings, `os.path`, `glob`, `re`, `yaml` and `pathlib`, but no pandas, reaches at most 36%. It reaches 29% if `run:` and checkpoints are also left for later.
   - A small table type that behaves like the common pandas subset (3.12) would raise this a lot. With 40 names it would cover about 68% of the pandas workflows. This fits decision 1 ("no Python install").
3. **DAG-time side effects are common.** 49% of workflows read the file system while the DAG is built, and 7% run commands. The evaluator needs controlled file access relative to the working directory, and either a clear error or an explicit opt-in for subprocess calls.
4. **Software environments.** 86% of the workflows rely on `conda:` env files. Only 33% name containers. Running tools in containers therefore needs a mapping from conda env files to images early on. `containerized:` (9%) already names one image for all environments.
5. **Early execution features.** Python and R script preambles (54% and 28% of workflows) and wrappers (42%) come right after shell. `run:` (18%) needs Python at job time with the Snakefile globals, so it should come late.
6. **A first Snakemake test suite.** Start with the 109 test folders in `results/tests-first-suite.txt`. Next, allow Python helpers (32 more folders, 141 in total) and then checkpoints and modules.

## 7. Running the test workflows

`just snakemake-run-test-workflows-test` runs every workflow of the test workflows with the mock TES executor (`snakemake-runtime/tests/run_test_workflows.rs`). Each workflow runs in a child process with a timeout of two minutes. Up to eight tasks are pending at once and finish in a shuffled order (seed 1, or `SNAKEMAKE_TEST_WORKFLOWS_SEED`), as tasks of a real run may. Input files that no rule makes and that do not exist are assumed to exist. `run_test_workflows.txt` records the outcome of each workflow: `ok` and the number of tasks, or the exit code and the first line of the error.

### 7.1 How a workflow is launched

`launches.py` writes `run_test_workflows_launches.txt`. A catalog workflow runs plainly in its repository, unless Snakemake cannot evaluate it that way and it can with the launch of its CI or with the mandatory flags of its catalog entry. A test of Snakemake runs as the first test function of `tests.py` runs it: its folder, Snakefile, config files and values, targets (also written as `"a b".split()`), and `--profile` or `--workflow-profile`. iunctus takes the profiles of a launch in `Launch.profiles`, by path or by the name of a workflow profile.

### 7.2 Evidence from Snakemake

`oracle.py` runs `snakemake --dry-run --printshellcmds` of Snakemake 9.27.0 in a copy of each workflow, with the Python packages and plugins its header names. `--printshellcmds` makes Snakemake format the shell command of each job, as iunctus does when it builds a task. The first mode records every launch of the workflows whose preferred launch Snakemake cannot evaluate; launches.py uses it to choose launches. `--only FILE` records the workflows that iunctus cannot run, as evidence. Snakemake is only the reference here; iunctus never runs it.

### 7.3 Why a workflow cannot run

Every workflow that does not run has a record in `run_test_workflows_rejected.txt`: `invalid` when Snakemake fails the same way because the workflow is broken, `missing` when it needs something that is not in its repository (data, a config file, a Python package, environment variables), or `unsupported (decision N)`. The run test fails for a failing workflow without a record, for a record whose workflow runs, and for an `invalid` or `missing` record without a failing dry run of Snakemake in the oracle file.

A mock run cannot know the content of task outputs. Some workflows read that content while they are planned, like a sample list that a checkpoint downloads; the mock executor makes these files empty, so their input functions fail. A real run gives the content. Errors of mock runs say which task output the workflow read, and which input files the run assumed to exist.

### 7.4 Commands while the workflow is planned

Workflow code may run commands while it is planned: `shell()`, `subprocess`, `os.system` and `os.popen`. They never run on the machine of iunctus (decision 11). Each becomes a task of its own: the image is the workflow's global `container:`, else `debian:stable-slim`, and the working directory of the workflow is its input. The driver hands the task to the engine as the answer to the request it is handling and waits for its outcome; outcomes of other tasks that come meanwhile wait until the request is done. The mock executors give such a command no output and exit code 0. Files that a command writes are not kept yet.

### 7.5 Files on the web and in storage

Modules and includes may come from GitHub or GitLab (`github(...)`, `gitlab(...)`, `gh:owner/repo@tag`) or from web addresses. iunctus mirrors each repository at its reference into the downloads folder of the run and downloads a file the first time the workflow reads it, so relative includes, schemas and scripts work as for local files. Errors name such files by their web address. Tasks get scripts of remote modules from their web address.

A file in storage (`storage.http(url)`, `storage("s3://...")`) has the local path that Snakemake gives it, `.snakemake/storage/<provider>/<address without scheme>`. Its task gets it from its address, and it counts as present while the workflow is planned.
