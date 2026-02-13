<!-- Snakemake corpus for iunctus: how it was built, a Snakefile parser evaluation, and a feature inventory of real workflows and the Snakemake tests -->
<!-- Copyright (c) 2026 The iunctus Contributors -->
<!-- SPDX-License-Identifier: MIT or Apache-2.0 -->

# Snakemake corpus: parser evaluation and feature inventory

Research date: 2026-09-27. This note builds on [snakemake.md](snakemake.md). That note gives the keyword lists and the execution model. This note measures what real workflows use.

All numbers marked "own measurement" come from scripts in `~/.cache/iunctus/corpus/snakemake/tools/`. The raw results are in `~/.cache/iunctus/corpus/snakemake/results/`. Section 5 lists what I could not verify.

## Summary

- The corpus has 464 of the 465 standardized catalog workflows and the `tests/` folder of Snakemake v9.27.0. It is text only: 8,375 text files from the sources (29.2 MB). With the catalog index, the tools and the results it is 38.8 MB.
- 62 of the 464 standardized workflows define no rule. Most are templates. The inventory uses the 399 workflows in which the Snakemake parser found at least one rule. Together they have 10,416 rules and 346,130 lines of Snakemake code.
- Parsers, measured against Snakemake's own `parser.py` on the files each workflow really loads:
  - tree-sitter-snakemake parses 97.5% of those files without error, and all files of 87.6% of the workflows with rules.
  - snakemake-lang (Rust, built on ruff) accepts 67.3% of those files, and all files of 40.0% of the workflows with rules. Its failures come from a line-based scanner. Examples are docstrings in rules, comments after values and multi-line strings.
  - `ruff_python_parser` 0.0.15 parses 3,806 of 3,810 Python programs that `parser.py` generates from Snakefiles. The 4 failures are also rejected by CPython. It parses 2,375 of 2,384 helper `.py` files. CPython rejects the same 9.
- DAG-time Python is the norm. Of the 399 workflows, 70% define functions, 68% use lambdas and 64% import pandas. 49% touch the file system while the DAG is built. Only 32 workflows (8%) are fully static.
- The pandas use is narrow but has a long tail. About 40 pandas names (module functions such as `read_csv` plus DataFrame and Index members) cover about 68% of the pandas workflows. About 95 names cover all of them. This counting is a heuristic.
- Execution: `shell:` in 91% of workflows, `script:` in 66% (Python 54%, R 28%), `wrapper:` in 42%, `run:` in 18%, `notebook:` in 2%. Software: `conda:` in 86% of workflows, containers in 33%.
- Snakemake tests: 391 test folders have a Snakefile and `expected-results`. 210 of them need no Python helpers, no scripts, wrappers or notebooks, no checkpoints and no software environment. 109 test folders remain after also removing `run:`, modules, storage, groups, handlers, PEP, rejected files, expected failures and folders that no test function uses.

## 1. Corpus

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
- Location: `~/.cache/iunctus/corpus/snakemake/catalog/<owner>__<repo>/`. Each folder has `.iunctus-meta.json` with branch, commit and skipped files. The catalog entries themselves are in `catalog-standardized.json`. All commits are in `revisions.txt`.

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

### 1.4 Limits of the corpus

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
  - all `.py` files in the corpus;
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

- Use `ruff_python_parser` and its lexer as the base, and port the `parser.py` state machine to Rust. The Python part is solved: ruff parses everything that CPython parses in this corpus. Pin the exact version. The crate says it is "an internal component crate of Ruff". It had 15 releases (0.0.0 to 0.0.15) between 2026-06-23 and 2026-09-24, and it needs Rust 1.96.
- snakemake-lang is the closest existing work, but it is not ready. It fails a third of the loaded files. Its fixes are known (use tokens for block ends, add docstrings, local `use rule`, anonymous rules and `singularity`). Iunctus could contribute these fixes upstream or reuse its structure.
- tree-sitter-snakemake is not a good base for execution, because it has no Python AST. It could still serve editor features later.
- Keep `parser.py` as an oracle that runs only in CI. In my reading this fits decision 1, because it never runs as part of iunctus.

