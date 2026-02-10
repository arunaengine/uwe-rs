# Rust building blocks for iunctus (native Nextflow, Snakemake and CWL engine)

How the data was collected (all on 2026-09-27):

- Versions, release dates, licenses, download counts and crate owners come from the crates.io API (`https://crates.io/api/v1/crates/<name>`). Each crate row links its crates.io page.
- Stars, last push, archive flag and "commits in the last 6 months" come from the GitHub API. "Commits in the last 6 months" means commits on the default branch since 2026-03-27.
- Architecture notes for sprocket, crankshaft, tes, commonwl, monty, tree-sitter-nextflow, snakemake-lang, ga4gh-sdk and metis come from shallow clones of their default branches in /tmp. File links point to the GitHub copies.
- The "Fit" column in each table is our own assessment (inference), not a sourced fact. Values: Dependency (reuse as a dependency), Ideas only, Watch, Avoid.

## 1. Embedded Python engines for Snakemake

### Takeaway
No pure-Rust Python can run arbitrary real-world Snakefiles, because many import CPython packages such as pandas. The practical options are: Ruff's parser for syntax, Monty (pydantic, 1.0.0 released two days ago) for sandboxed evaluation of simple Python, and PyO3-embedded CPython as a fallback for full compatibility.

### Cited Findings

| Crate / project | Purpose | Latest (date) | License | Activity | Fit | Source |
|---|---|---|---|---|---|---|
| `monty` (pydantic/monty) | Secure, snapshotable Python sandbox written in Rust | 1.0.0 (2026-09-25) | MIT | 8,356 stars; 435 commits in 6 months; crate owner samuelcolvin; 58,083 downloads | Dependency (evaluation tier 1) | [crates.io](https://crates.io/crates/monty), [GitHub](https://github.com/pydantic/monty) |
| `ruff_python_parser` / `ruff_python_ast` | Ruff's Python parser and AST | 0.0.15 (2026-09-24) | MIT | Ruff repo: 49,810 stars; 2,714 commits in 6 months; published by `astral-crates-bot` and `charliermarsh` | Dependency (pin exact version) | [crates.io](https://crates.io/crates/ruff_python_parser), [GitHub](https://github.com/astral-sh/ruff/tree/main/crates/ruff_python_parser) |
| `rustpython` / `rustpython-vm` | Full Python 3 interpreter in Rust | 0.5.0 (2026-03-31) | MIT | 22,370 stars; 1,129 commits in 6 months | Watch (tier 2 candidate) | [crates.io](https://crates.io/crates/rustpython), [GitHub](https://github.com/RustPython/RustPython) |
| `rustpython-parser` | Older standalone RustPython parser | 0.4.0 (2024-08-06, older) | MIT | RustPython/Parser: 1 commit in 6 months | Avoid | [crates.io](https://crates.io/crates/rustpython-parser), [GitHub](https://github.com/RustPython/Parser) |
| `pyo3` | Rust bindings to CPython, including embedding | 0.29.2 (2026-08-05) | MIT OR Apache-2.0 | 16,179 stars; 329 commits in 6 months | Dependency (optional fallback, feature flag) | [crates.io](https://crates.io/crates/pyo3), [GitHub](https://github.com/PyO3/pyo3) |
| `starlark` / `starlark_syntax` (Meta) | Starlark (Python-like config language) interpreter | 0.14.2 (2026-06-05) | Apache-2.0 | 1,026 stars; 706 commits in 6 months | Ideas only | [crates.io](https://crates.io/crates/starlark), [GitHub](https://github.com/facebook/starlark-rust) |
| `tree-sitter-python` | tree-sitter grammar for Python | 0.25.0 (2025-09-11) | MIT | 568 stars; 0 commits in 6 months | Dependency (editor or cross-check only) | [crates.io](https://crates.io/crates/tree-sitter-python), [GitHub](https://github.com/tree-sitter/tree-sitter-python) |
| tree-sitter-snakemake (osthomas) | Snakemake grammar based on tree-sitter-python | not on crates.io | MIT | 12 stars; last push 2026-05-12; 0 commits in 6 months | Ideas only | [GitHub](https://github.com/osthomas/tree-sitter-snakemake) |
| snakemake-lang (cademirch) | Parser, AST and compiler for Snakemake (`.smk` to "virtual Python"), optional PyO3 bindings | not on crates.io; last commit 2026-05-19 | not checked | 0 stars | Ideas only (closest Rust precedent for Snakemake parsing) | [GitHub](https://github.com/cademirch/snakemake-lang) |

- Ruff's internal crates are published to crates.io with "no stability guarantees"; they are "versioned as `0.0.x`. The patch version is incremented on every Ruff release, regardless of changes to the crate." - [Ruff versioning docs](https://docs.astral.sh/ruff/versioning/). The crate description itself says "This is an internal component crate of Ruff" - [crates.io](https://crates.io/crates/ruff_python_parser). An older request for publishing is [ruff issue #10417](https://github.com/astral-sh/ruff/issues/10417).
- Ruff's Python parser is a hand-written recursive descent parser that uses Pratt parsing for expressions - [ruff_python_parser README](https://github.com/astral-sh/ruff/tree/main/crates/ruff_python_parser).
- Monty describes itself as "A secure Python sandbox, written in Rust, for code written by AI". It is a "Python 3.14 sandbox". A new sandbox takes under 1 ms from a running pool. "Filesystem, environment variables and network do not exist inside the sandbox: it reaches the host only through the functions and mounts you pass in." - [Monty README](https://github.com/pydantic/monty).
- Monty parses with Ruff's parser (its workspace depends on `ruff_python_parser`, `ruff_python_ast` and related crates at 0.0.14) - [Monty Cargo.toml](https://github.com/pydantic/monty/blob/main/Cargo.toml), [Monty language limitations](https://github.com/pydantic/monty/blob/main/limitations/language.md).
- Monty supports functions, closures, lambdas, decorators on functions and classes, simple classes, `@dataclass` (limited), comprehensions, try/except, f-strings, `with`, async/await and imports. It rejects at parse time: class inheritance and metaclasses, decorators on methods (`@property`, `@staticmethod`), `yield` (no generator functions), `match`, `del`, `try*`, PEP 695 `type` aliases, wildcard imports and complex literals - [Monty limitations index](https://github.com/pydantic/monty/blob/main/limitations/index.md), [language.md](https://github.com/pydantic/monty/blob/main/limitations/language.md).
- Monty ships a fixed set of stdlib modules (asyncio, base64, binascii, collections, copy, dataclasses, datetime, functools, itertools, json, math, os, pathlib, random, re, sys, time, typing, unicodedata). "`import` of anything else raises `ModuleNotFoundError`: there is no `sys.path`, no site-packages" - [Monty modules.md](https://github.com/pydantic/monty/blob/main/limitations/modules.md).
- RustPython's README says it is "A Python-3 (CPython >= 3.14.0) Interpreter written in Rust" - [RustPython README](https://github.com/RustPython/RustPython). The GitHub releases page has no releases; versions are published only on crates.io - [RustPython releases](https://github.com/RustPython/RustPython/releases), [crates.io](https://crates.io/crates/rustpython).
- A crate named `rustpython-ruff_python_parser` exists on crates.io - [crates.io](https://crates.io/crates/rustpython-ruff_python_parser). (Owner not checked.)
- PyO3 embedding: the `auto-initialize` feature starts an interpreter on first use; embedding needs a Python build with a shared library (or `libpython.a` for static linking); `auto-initialize` is deliberately disabled for static embedding - [PyO3 building and distribution guide](https://pyo3.rs/main/building-and-distribution).
- Starlark is "a deterministic language inspired by Python3, used for configuration in the build systems" - [starlark-rust README](https://github.com/facebook/starlark-rust).
- snakemake-lang's README: "Parser, AST, and compiler for the Snakemake workflow language. Rust crate with optional CLI, JSON output, and PyO3 bindings." Its CLI has `compile` (".smk -> virtual Python"), `parse` (AST as JSON) and `check`. It depends on `ruff_python_parser` and `ruff_python_ast` from the Ruff git tag 0.14.0 - [snakemake-lang](https://github.com/cademirch/snakemake-lang).

### Inferences
- Snakefiles are Python plus rule syntax. The rule syntax needs a custom front end. The embedded Python blocks can be parsed with `ruff_python_parser`. snakemake-lang shows this split works.
- Monty fits well for the common case: `params` lambdas, input functions, `config` lookups and string formatting. Its host-function model lets iunctus implement `expand`, `glob_wildcards`, `temp`, `protected` and similar helpers in Rust and expose them to the sandbox.
- Monty will fail on Snakefiles that import pandas, numpy, yaml or custom modules, or that use class inheritance or generators. For those, only real CPython works. RustPython cannot load CPython C-extension packages such as pandas (not verified in this session; it follows from RustPython being a separate VM, not the CPython C API).
- A realistic design: tier 1 = Monty (default, sandboxed, no Python install needed); tier 2 = PyO3 with embedded CPython behind a cargo feature and a runtime flag. PyO3 still keeps iunctus "native" in the sense that it never calls the `snakemake` program, but it adds a libpython runtime dependency.
- Pin Ruff crates with `=0.0.15` style exact versions, because every Ruff release bumps them without stability promises.
- Starlark lacks too much of Python (for example imports of arbitrary modules and classes) to run Snakefiles. It is a useful model for a deterministic, sandboxed evaluator.

### Gaps
- No public measurement of how many real Snakefiles (for example in the Snakemake workflow catalog) stay inside Monty's subset. This needs an experiment.
- Could not verify whether a Monty host can register a whole custom module (for example a fake `snakemake` module or `pandas` shim). Docs say sandboxed code cannot load extra modules; host-side module injection was not checked.
- No RustPython compatibility score (CPython test pass rate) was found.

## 2. JavaScript engines for CWL expressions

### Takeaway
CWL needs only ECMAScript 5.1 expressions. `boa_engine` (pure Rust, 95.60% Test262 in v0.22) is the best default and is already used by the Rust CWL engine `commonwl`. `rquickjs` (QuickJS-NG, C) is a fast fallback. V8 via `deno_core` is overkill.

### Cited Findings

| Crate | Purpose | Latest (date) | License | Activity | Fit | Source |
|---|---|---|---|---|---|---|
| `boa_engine` | Embeddable JS engine written in Rust | 0.22.0 (2026-08-28) | Unlicense OR MIT | 7,569 stars; 142 commits in 6 months | Dependency (default) | [crates.io](https://crates.io/crates/boa_engine), [GitHub](https://github.com/boa-dev/boa) |
| `rquickjs` | High-level bindings to QuickJS-NG (C) | 0.14.0 (2026-09-18) | MIT | 1,019 stars; 182 commits in 6 months | Dependency (fallback) | [crates.io](https://crates.io/crates/rquickjs), [GitHub](https://github.com/DelSkayn/rquickjs) |
| `deno_core` | Deno's V8-based runtime core | 0.412.0 (2026-09-16) | MIT | old repo denoland/deno_core is archived; crate now lives in denoland/deno | Avoid for CWL | [crates.io](https://crates.io/crates/deno_core), [GitHub (archived)](https://github.com/denoland/deno_core) |
| `v8` (rusty_v8) | Rust bindings to V8 | 152.2.0 (2026-08-20) | MIT | 3,950 stars; 87 commits in 6 months | Avoid for CWL | [crates.io](https://crates.io/crates/v8), [GitHub](https://github.com/denoland/rusty_v8) |
| `rustyscript` | Easy wrapper over deno_core | 0.12.3 (2025-08-21) | MIT OR Apache-2.0 | 388 stars; 0 commits in 6 months | Avoid | [crates.io](https://crates.io/crates/rustyscript), [GitHub](https://github.com/rscarson/rustyscript) |
| `nova_vm` | Research JS engine in Rust (data-oriented) | 1.0.0 on crates.io (2026-03-15); GitHub latest release 0.3.0 (2025-08-19) | MPL-2.0 | 1,951 stars; 13 commits in 6 months; 1,739 total downloads | Avoid (immature) | [crates.io](https://crates.io/crates/nova_vm), [GitHub](https://github.com/trynova/nova) |
| `javy` (Bytecode Alliance) | Run JS in WebAssembly via QuickJS | 8.1.0 (2026-07-30) | Apache-2.0 WITH LLVM-exception | active | Ideas only | [crates.io](https://crates.io/crates/javy) |
| `quick-js` | Old QuickJS bindings | 0.4.1 (2021-03-15, older) | MIT | unmaintained | Avoid | [crates.io](https://crates.io/crates/quick-js) |

- CWL v1.2: "A code fragment wrapped in the `$(...)` syntax must be evaluated as a ECMAScript expression", linking to ECMAScript 5.1 - [CWL v1.2 CommandLineTool spec](https://www.commonwl.org/v1.2/CommandLineTool.html).
- Boa v0.22 (released August 28, 2026) reached 95.60% Test262 conformance, enabled Temporal by default and added bytecode optimizations and a polymorphic inline cache. Boa v0.21 (October 21, 2025) was at 94.12% and moved from a stack-based to a register-based VM - [Boa releases](https://github.com/boa-dev/boa/releases).
- rquickjs: "a high level bindings of the QuickJS-NG JavaScript engine, a fork of the QuickJS Javascript engine" - [rquickjs README](https://github.com/DelSkayn/rquickjs).
- The Rust CWL engine `commonwl` uses `boa_engine = "0.21.1"` for expression evaluation - [commonwl Cargo.toml](https://github.com/fairagro/commonwl/blob/main/Cargo.toml), [engine expression.rs](https://github.com/fairagro/commonwl/tree/main/crates/engine). It reports 100% on the CWL `Workflow` and `ExpressionTool` conformance tests and 98% on `CommandLineTool` - [commonwl README](https://github.com/fairagro/commonwl).

### Inferences
- Boa is the best default: pure Rust (no C toolchain, simple cross-compilation), ES5.1 is far below its feature level, and commonwl proves it works for CWL.
- rquickjs is a good second option if expression throughput becomes a bottleneck (large scatters with many `valueFrom` evaluations). It adds a C build step.
- V8 adds a very large native dependency for no CWL benefit.
- The engine choice should hide behind a small `ExpressionEngine` trait so it can be swapped.

### Gaps
- No benchmark comparing Boa and QuickJS on CWL-style short expressions was found.
- Did not verify Boa's support for execution time limits or memory limits (useful for untrusted expressions).

## 3. Groovy and the JVM for Nextflow

### Takeaway
There is no usable Groovy interpreter in Rust. The only mature Rust-accessible Nextflow artifact is the official `tree-sitter-nextflow` grammar (rewritten on 2026-09-23 to mirror Nextflow's ANTLR grammar). Writing our own parser plus a tree-walking interpreter for the Nextflow subset of Groovy is effectively the only native path. Embedding a JVM (jni or j4rs) is possible but defeats the native goal.

### Cited Findings

Grammars and parsers:

| Crate / project | Purpose | Latest (date) | License | Activity | Fit | Source |
|---|---|---|---|---|---|---|
| tree-sitter-nextflow (nextflow-io) | Official Nextflow tree-sitter grammar; Rust, Python, C bindings | 0.4.0 (2026-09-23); not on crates.io (git dependency) | MIT | 5 stars; last push 2026-09-24; authors Edmund Miller, Ben Sherman | Dependency (editor, test oracle) | [GitHub](https://github.com/nextflow-io/tree-sitter-nextflow) |
| nextflow-io/language-server | Official Nextflow language server (Java) | v26.04.4 (2026-09-16) | Apache-2.0 | 63 commits in 6 months | Ideas only | [GitHub](https://github.com/nextflow-io/language-server) |
| `tree-sitter-groovy` (amaanq) | Groovy tree-sitter grammar | 0.1.2 (2024-11-19, older) | MIT | 3 stars; 0 commits in 6 months | Avoid | [crates.io](https://crates.io/crates/tree-sitter-groovy), [GitHub](https://github.com/amaanq/tree-sitter-groovy) |
| `dekobon-tree-sitter-groovy` | Fork of a Groovy tree-sitter grammar | 0.3.0 (2026-08-30); crate created 2026-05-19 | Apache-2.0 OR MIT | 3 stars; 66 commits in 6 months | Watch | [crates.io](https://crates.io/crates/dekobon-tree-sitter-groovy), [GitHub](https://github.com/dekobon/tree-sitter-groovy) |
| tree-sitter-groovy (murtaza64) | Groovy tree-sitter grammar | GitHub only | MIT | 26 stars; 2 commits in 6 months | Ideas only | [GitHub](https://github.com/murtaza64/tree-sitter-groovy) |
| `arborium-groovy` | Groovy grammar bundled for the arborium highlighter | 2.18.2 (2026-08-28) | MIT | part of bearcove/arborium | Avoid (highlighting only) | [crates.io](https://crates.io/crates/arborium-groovy) |
| `groovy` (groovyrs) | Groovy lexer, parser and compiler to "fusevm" bytecode, run on a Cranelift JIT, "no JVM" | 0.1.11 (2026-09-26); crate created 2026-07-21 | MIT | 109 downloads; single developer (MenkeTechnologies) | Avoid (ideas only) | [crates.io](https://crates.io/crates/groovy), [GitHub](https://github.com/MenkeTechnologies/groovyrs) |
| `oak-groovy` | Experimental Groovy parser (ygg-lang "oaks") | 0.0.11 (2026-03-30) | MPL-2.0 | 439 downloads | Avoid | [crates.io](https://crates.io/crates/oak-groovy) |
| `antlr4rust` | ANTLR4 runtime for Rust (new fork, run by `alexsnaps`) | 0.5.2 (2025-10-25) | BSD-3-Clause | antlr4rust/antlr4: 32 stars; last push 2026-08-17; 2.3M downloads in last 90 days | Watch (fast path to a spec-exact parser) | [crates.io](https://crates.io/crates/antlr4rust), [GitHub](https://github.com/antlr4rust/antlr4) |
| `antlr-rust` | Original ANTLR4 Rust runtime | 0.2.2 / 0.3.0-beta (2022, older) | BSD-3-Clause | rrevenantt/antlr4rust last push 2023-02-14 | Avoid | [crates.io](https://crates.io/crates/antlr-rust), [GitHub](https://github.com/rrevenantt/antlr4rust) |

JVM bridges:

| Crate | Purpose | Latest (date) | License | Activity | Fit | Source |
|---|---|---|---|---|---|---|
| `jni` | Low-level JNI bindings (call a JVM from Rust) | 0.22.4 (2026-03-16) | MIT OR Apache-2.0 | 1,602 stars; 10 commits in 6 months | Avoid (unless a JVM fallback is wanted) | [crates.io](https://crates.io/crates/jni), [GitHub](https://github.com/jni-rs/jni-rs) |
| `j4rs` | High-level "Java for Rust", starts a JVM, loads jars at runtime | 0.25.2 (2026-05-25) | MIT OR Apache-2.0 | 750 stars; 12 commits in 6 months | Avoid (same reason) | [crates.io](https://crates.io/crates/j4rs), [GitHub](https://github.com/astonbitecode/j4rs) |
| `duchess` | Typed Java interop from Rust | 0.3.3 (2026-05-29) | Apache-2.0 OR MIT | 211 stars; 2 commits in 6 months | Avoid | [crates.io](https://crates.io/crates/duchess), [GitHub](https://github.com/duchess-rs/duchess) |
| `robusta_jni` | JNI helper macros | 0.2.2 (2023-10-30, older) | MIT | stale | Avoid | [crates.io](https://crates.io/crates/robusta_jni) |

- tree-sitter-nextflow README: "Mirrors the official Nextflow ANTLR grammar", which lives in `modules/nf-lang/src/main/antlr` of nextflow-io/nextflow. Rust use is by "a path or git dependency on this repo" - [tree-sitter-nextflow README](https://github.com/nextflow-io/tree-sitter-nextflow).
- Its 0.4.0 changelog (2026-09-23): "The grammar is rewritten from scratch to mirror the official Nextflow grammar (`ScriptParser.g4` in nextflow-io/nextflow)." Nextflow semantics are no longer baked into the syntax (`Channel.of(...)`, `FOO(x)`, `ch | map { }` are plain calls, member accesses and binary expressions). Process and workflow bodies have section nodes (`input_section`, `script_section`, `take_section`, `emit_section`) - [CHANGELOG](https://github.com/nextflow-io/tree-sitter-nextflow/blob/main/CHANGELOG.md).
- Its CI "Parses every .nf file in nf-core/modules and fails if any file has an ERROR node" - [parse-rate workflow](https://github.com/nextflow-io/tree-sitter-nextflow/blob/main/.github/workflows/parse-rate.yml).
- A separate grammar for Nextflow config files exists but is stale (last push 2024-01-16) - [matthuska/tree-sitter-nextflow-config](https://github.com/matthuska/tree-sitter-nextflow-config).
- GitHub search for `groovy interpreter language:Rust` returned no repositories; a crates.io search for "groovy" found only the young or highlighting-only crates listed above - [GitHub search](https://github.com/search?q=groovy+interpreter+language%3ARust&type=repositories), [crates.io search](https://crates.io/search?q=groovy).
- antlr4rust: generate with `java -jar <antlr tool> -Dlanguage=Rust MyGrammar.g4`; "Development is going on in this repository but eventually it will be merged to main ANTLR4 repo" - [docs.rs antlr4rust](https://docs.rs/crate/antlr4rust/latest), [antlr4rust org](https://github.com/antlr4rust).
- GraalVM native image (SubstrateVM) does not support dynamic class loading, dynamic class generation and invokedynamic, which makes dynamic Groovy "almost 99% incompatible" with native images without heavy configuration - [klokwrk Groovy + GraalVM article](https://github.com/croz-ltd/klokwrk-project/blob/master/support/documentation/article/groovy-graalvm-native-image/groovy-graalvm-native-image.md). Spring Boot states Groovy is not supported with GraalVM - [Spring Boot with GraalVM wiki](https://github.com/spring-projects/spring-boot/wiki/Spring-Boot-with-GraalVM).
- j4rs uses a custom `J4rsClassLoader` that allows adding jars to the classpath after the JVM is started - [j4rs README](https://github.com/astonbitecode/j4rs).

### Inferences
- Nextflow's strict syntax (the `nf-lang` ANTLR grammar) narrows Groovy to a defined subset. That makes a hand-written parser plus tree-walking interpreter tractable. The hard part is not syntax but runtime semantics: closures, GStrings, Groovy collection methods (the GDK), Java types used in scripts (`file()`, `Path`, `MemoryUnit`, `Duration`), and Java regex.
- Suggested path: hand-write the Nextflow parser in Rust (logos lexer + rowan tree, same pattern as sprocket's WDL parser) using `ScriptParser.g4` as the specification, and use tree-sitter-nextflow as a differential test oracle over nf-core modules and pipelines.
- antlr4rust could generate a parser straight from `ScriptParser.g4` for a quick prototype. Risks: a young runtime with one visible owner, and ANTLR parse trees would still need conversion to our own tree.
- Embedding a JVM (jni, j4rs) to run real Groovy would need a JRE on every machine and would pull in most of Nextflow's own runtime. That conflicts with the "no original engine" rule. GraalVM native image does not help for dynamic Groovy.

### Gaps
- No parse-rate numbers for tree-sitter-nextflow on full nf-core pipelines (only nf-core/modules is in CI) or on `nextflow.config` files.
- No data on antlr4rust performance, error recovery quality or maintainer count.
- Could not find any Rust implementation of Groovy runtime semantics (GDK methods, GString, closures delegate rules).

## 4. Parsing infrastructure and front-end design

### Takeaway
The leading Rust language tools (rust-analyzer, ruff, biome, oxc) all use hand-written recursive descent parsers. rust-analyzer and biome build lossless rowan-style trees; ruff and oxc build typed ASTs. St. Jude's WDL front end uses logos + rowan + a hand-written parser, which is the closest template for iunctus. tree-sitter is best used for editors and cross-checks, not as the interpreter's front end.

### Cited Findings

| Crate | Purpose | Latest (date) | License | Activity | Fit | Source |
|---|---|---|---|---|---|---|
| `tree-sitter` | Incremental, error-tolerant parser generator runtime (C core) | 0.27.0 (2026-08-30) | MIT | 27,055 stars; 299 commits in 6 months | Dependency (editor/oracle path) | [crates.io](https://crates.io/crates/tree-sitter), [GitHub](https://github.com/tree-sitter/tree-sitter) |
| `rowan` | Lossless red-green syntax trees (rust-analyzer) | 0.17.0 (2026-08-02); 0.18.0-alpha.1 exists | MIT OR Apache-2.0 | 921 stars; 35 commits in 6 months | Dependency | [crates.io](https://crates.io/crates/rowan), [GitHub](https://github.com/rust-analyzer/rowan) |
| `cstree` | rowan fork with interning, thread-safe trees | 0.14.0 (2026-04-22) | MIT OR Apache-2.0 | 133 stars; 16 commits in 6 months | Watch (alternative to rowan) | [crates.io](https://crates.io/crates/cstree), [GitHub](https://github.com/domenicquirl/cstree) |
| `logos` | Fast lexer generator (derive macro) | 0.16.1 (2026-01-30) | MIT OR Apache-2.0 | 3,567 stars; 45 commits in 6 months | Dependency | [crates.io](https://crates.io/crates/logos), [GitHub](https://github.com/maciejhirsz/logos) |
| `ungrammar` | Grammar notation used to generate typed AST APIs over rowan | 1.16.1 (2022-03-05, older) | MIT OR Apache-2.0 | stable, low change | Ideas only | [crates.io](https://crates.io/crates/ungrammar) |
| `chumsky` | Parser combinators with error recovery | 0.13.0 (2026-05-06); 1.0.0-alpha.8 | MIT | repo moved to Codeberg | Ideas only | [crates.io](https://crates.io/crates/chumsky), [Codeberg](https://codeberg.org/zesterer/chumsky) |
| `lalrpop` | LR(1) parser generator | 0.23.1 (2026-03-11) | Apache-2.0 OR MIT | 3,502 stars; 13 commits in 6 months | Avoid for main grammar | [crates.io](https://crates.io/crates/lalrpop), [GitHub](https://github.com/lalrpop/lalrpop) |
| `pest` | PEG parser generator | 2.9.2 (2026-09-21) | MIT OR Apache-2.0 | 5,402 stars; 31 commits in 6 months | Avoid for main grammar | [crates.io](https://crates.io/crates/pest), [GitHub](https://github.com/pest-parser/pest) |
| `winnow` | Parser combinators (nom successor style) | 1.0.4 (2026-07-13) | MIT | very widely used | Dependency (small formats, e.g. memory/duration units) | [crates.io](https://crates.io/crates/winnow) |
| `nom` | Parser combinators | 8.0.0 (2025-01-26) | MIT | widely used | Ideas only | [crates.io](https://crates.io/crates/nom) |

- rust-analyzer uses a hand-written recursive descent parser that emits events ("start node X", "finish node Y"), modeled on Kotlin's parser for error handling. Trees are rowan red-green trees. A typed `ast` layer sits on top. An `ungrammar` description generates syntax kinds and AST code. The parser is independent of the tree representation - [rust-analyzer architecture](https://rust-analyzer.github.io/book/contributing/architecture.html).
- matklad's tutorial on resilient LL parsing explains the error-recovery approach (2023, older) - [Resilient LL Parsing Tutorial](https://matklad.github.io/2023/05/21/resilient-ll-parsing-tutorial.html).
- Biome's parser uses an internal fork of rowan (`biome_rowan`) implementing green and red trees, and recovers from errors into a lossless CST - [Biome architecture](https://biomejs.dev/internals/architecture/).
- Oxc uses a hand-written recursive descent parser, allocates all AST nodes in an arena (`oxc_allocator`), and defines its own AST that differs from ESTree - [Oxc parser architecture](https://oxc.rs/docs/learn/architecture/parser).
- St. Jude's `wdl-grammar` depends on `logos` and `rowan` and contains `lexer.rs`, `parser.rs`, `grammar/v1.rs` and `tree.rs`. `wdl-analysis` depends on `rowan`, `petgraph`, `lsp-types`, `rayon` and `blake3` - [wdl-grammar](https://github.com/stjude-rust-labs/sprocket/tree/main/crates/wdl-grammar), [wdl-analysis Cargo.toml](https://github.com/stjude-rust-labs/sprocket/blob/main/crates/wdl-analysis/Cargo.toml).
- tree-sitter maintainer maxbrunsfeld (older discussion, number 831): "we haven't yet built out functionality for error messages. So it isn't trivial to find out the exact token/position where the error initiated, and get a list of expected tokens" and "the error recovery currently isn't customizable in domain-specific ways". A user reported that converting the tree-sitter parse tree to an AST "felt like maintaining a second, redundant parser" and that C dependencies made WebAssembly builds hard - [tree-sitter discussion #831](https://github.com/tree-sitter/tree-sitter/discussions/831).

### Inferences
- For an interpreter that must also power a language server later, the sprocket and rust-analyzer pattern fits best: logos lexer, hand-written recursive descent parser emitting events, rowan lossless tree, typed AST wrappers, then a lowering step into the iunctus intermediate representation.
- One shared rowan-based infrastructure can host three front ends: Nextflow (Groovy subset), Snakemake rule syntax (with Python blocks delegated to `ruff_python_parser`), and CWL (YAML, where the "parser" is a YAML library plus a typed model).
- tree-sitter grammars (tree-sitter-nextflow, tree-sitter-python, tree-sitter-yaml) remain useful for editor highlighting and as independent parsers for differential testing.
- Parser generators (pest, lalrpop) make good error recovery and lossless trees harder; skip them for the main languages.

### Gaps
- No published comparison of error-message quality between hand-written rowan parsers and tree-sitter for Groovy-like languages.

## 5. YAML and JSON (CWL documents, configs, nf-schema)

### Takeaway
`serde_yaml` is deprecated and `serde_yml` is flagged unsound and unmaintained. For new code, the pure-Rust saphyr family is the best base: `serde-saphyr` for typed serde loading (already used by the Rust CWL engine) and `saphyr` for lower-level or span-aware work. The `jsonschema` crate covers JSON Schema draft 2020-12, which nf-schema uses.

### Cited Findings

| Crate | Purpose | Latest (date) | License | Activity / status | Fit | Source |
|---|---|---|---|---|---|---|
| `serde_yaml` | Original serde YAML (dtolnay) | 0.9.34+deprecated (2024-03-25, older) | MIT OR Apache-2.0 | repo archived | Avoid | [crates.io](https://crates.io/crates/serde_yaml), [GitHub](https://github.com/dtolnay/serde-yaml) |
| `serde_yml` | serde_yaml fork | 0.0.13 (2026-05-27) | MIT OR Apache-2.0 | repo archived; RUSTSEC-2025-0068; now a shim over noyalib | Avoid | [crates.io](https://crates.io/crates/serde_yml), [GitHub](https://github.com/sebastienrousseau/serde_yml) |
| `noyalib` | New pure-Rust YAML 1.2 library (same author as serde_yml) | 0.0.51 (2026-09-22); crate created 2026-05-10 | MIT OR Apache-2.0 | 587,359 downloads (mostly via the serde_yml shim, inference) | Watch | [crates.io](https://crates.io/crates/noyalib), [GitHub](https://github.com/sebastienrousseau/noyalib) |
| `serde_norway` | Maintained serde_yaml fork (unsafe-libyaml-norway) | 0.9.42 (2024-12-21) | MIT OR Apache-2.0 | low activity | Watch | [crates.io](https://crates.io/crates/serde_norway) |
| `serde_yaml_ng` | serde_yaml fork (unsafe-libyaml) | 0.10.0 (2024-05-26) | MIT | used by sprocket's wdl-engine | Avoid for new code | [crates.io](https://crates.io/crates/serde_yaml_ng) |
| `saphyr` / `saphyr-parser` | "A fully YAML 1.2 compliant YAML library" (successor of yaml-rust2) | 0.1.0 (2026-09-19) | MIT OR Apache-2.0 | 337 stars; 83 commits in 6 months; owners davvid, Ethiraric | Dependency | [crates.io](https://crates.io/crates/saphyr), [GitHub](https://github.com/saphyr-rs/saphyr) |
| `serde-saphyr` | serde (de)serializer on top of saphyr-parser | 1.3.0 (2026-09-16) | MIT OR Apache-2.0 | 224 stars; 384 commits in 6 months | Dependency (default for typed loading) | [crates.io](https://crates.io/crates/serde-saphyr), [GitHub](https://github.com/bourumir-wyngs/serde-saphyr) |
| `yaml-rust2` | Maintained yaml-rust fork | 0.13.0 (2026-09-11) | MIT OR Apache-2.0 | 263 stars; 21 commits in 6 months | Watch | [crates.io](https://crates.io/crates/yaml-rust2), [GitHub](https://github.com/Ethiraric/yaml-rust2) |
| `yaml-rust` | Original pure-Rust YAML | 0.4.5 (2021-01-03, older) | MIT/Apache-2.0 | unmaintained | Avoid | [crates.io](https://crates.io/crates/yaml-rust) |
| `marked-yaml` | YAML with source position markers | 0.8.0 (2025-05-26) | MIT | low activity | Ideas only | [crates.io](https://crates.io/crates/marked-yaml) |
| `serde_json` | JSON | 1.0.151 (2026-07-20) | MIT OR Apache-2.0 | core ecosystem | Dependency | [crates.io](https://crates.io/crates/serde_json) |
| `jsonschema` | JSON Schema validator (drafts 4, 6, 7, 2019-09, 2020-12) | 0.58.1 (2026-09-26) | MIT | 822 stars; 726 commits in 6 months | Dependency | [crates.io](https://crates.io/crates/jsonschema), [GitHub](https://github.com/Stranger6667/jsonschema) |
| `boon` | JSON Schema validator (2020-12 to draft 4) | 0.6.1 (2025-01-07) | MIT OR Apache-2.0 | 0 commits in 6 months | Avoid | [crates.io](https://crates.io/crates/boon), [GitHub](https://github.com/santhosh-tekuri/boon) |

- RUSTSEC-2025-0068 (issued 2025-09-12): "Using `serde_yml::ser::Serializer.emitter` can cause a segmentation fault, which is unsound"; the GitHub repository was archived; no patched versions. Listed alternatives: `serde_norway` ("using `unsafe-libyaml-norway`"), `serde_yaml_ng` ("using unmaintained `unsafe-libyaml`"), and pure-Rust `serde_yaml2` and `yaml-peg` (marked incomplete) - [RustSec advisory](https://rustsec.org/advisories/RUSTSEC-2025-0068.html).
- serde_yml 0.0.13's crates.io description says it is deprecated and unmaintained, and that the release "is a thin compatibility shim that forwards every call to `noyalib`" - [crates.io](https://crates.io/crates/serde_yml). The repository says RUSTSEC-2025-0068 is "structurally fixed in 0.0.13" - [GitHub](https://github.com/sebastienrousseau/serde_yml).
- serde_yaml unmaintained status was tracked in RustSec - [advisory-db issue #2132](https://github.com/rustsec/advisory-db/issues/2132).
- noyalib self-describes as "Pure-Rust YAML 1.2 ecosystem: zero unsafe, 100% spec compliance (406/406 official suite), streaming-first serde, lossless CST, JSON-Schema validation" - [noyalib GitHub](https://github.com/sebastienrousseau/noyalib). These are the author's own claims.
- commonwl uses `serde-saphyr` with the `miette` and `validator` features - [commonwl Cargo.toml](https://github.com/fairagro/commonwl/blob/main/Cargo.toml). Sprocket's wdl-engine uses `serde_yaml_ng` - [wdl-engine Cargo.toml](https://github.com/stjude-rust-labs/sprocket/blob/main/crates/wdl-engine/Cargo.toml).
- commonwl reports that two CWL conformance tests fail because "`serde_json` stores all numbers as f64" (1e42 printed instead of the full integer) - [commonwl README](https://github.com/fairagro/commonwl).
- jsonschema supports drafts 4, 6, 7, 2019-09 and 2020-12; compliance per draft is tracked on Bowtie - [jsonschema docs](https://docs.rs/jsonschema).
- nf-schema (first released April 22, 2024) moved from JSON Schema draft-07 to draft 2020-12; `definitions` became `$defs` - [Seqera blog](https://seqera.io/blog/nf-schema/), [nf-schema migration guide](https://nextflow-io.github.io/nf-schema/latest/migration_guide/). nf-core pipelines such as rnaseq use `"$schema": "https://json-schema.org/draft/2020-12/schema"` - [nf-core/rnaseq schema](https://github.com/nf-core/rnaseq/blob/master/nextflow_schema.json).

### Inferences
- Use `serde-saphyr` for typed CWL and config loading. Use `saphyr` directly where exact source spans or YAML-specific details (anchors, tags, `$import`/`$include` pre-processing for Schema Salad) matter.
- Use `serde_json` with its `arbitrary_precision` feature (or keep numbers as strings in the intermediate representation) to avoid the f64 problem commonwl hit.
- `noyalib` looks promising on paper but is four months old, pre-1.0, and comes from the author of the archived crate. Re-evaluate in 2027.
- `jsonschema` can validate `nextflow_schema.json` and samplesheet schemas. nf-schema adds its own behavior (samplesheet to channel conversion, custom keywords); that part would need to be written.

### Gaps
- No independent verification of noyalib's spec-compliance claims.
- Did not check the Bowtie score of `jsonschema` for draft 2020-12 in detail, nor whether nf-schema's custom keywords (for example `exists`, `format: file-path`) are supported by it.

## 6. Execution core: async runtime, graphs, hashing, state, processes

### Takeaway
The execution core can be built entirely from mature crates: tokio, petgraph, blake3, redb or rusqlite, process-wrap. There is no general scheduling crate worth adopting; sprocket and crankshaft build their own on tokio. Hash choice depends on compatibility goals: blake3 for our own cache, SHA-1 for CWL `checksum`, and Murmur3-128 only if Nextflow `-resume` hash compatibility is ever wanted.

### Cited Findings

| Crate | Purpose | Latest (date) | License | Activity | Fit | Source |
|---|---|---|---|---|---|---|
| `tokio` | Async runtime | 1.53.1 (2026-07-20) | MIT | core ecosystem | Dependency | [crates.io](https://crates.io/crates/tokio) |
| `petgraph` | Graph data structures and algorithms | 0.8.3 (2025-09-30) | MIT OR Apache-2.0 | 4,020 stars; 10 commits in 6 months | Dependency | [crates.io](https://crates.io/crates/petgraph), [GitHub](https://github.com/petgraph/petgraph) |
| `daggy` | DAG wrapper over petgraph | 0.9.0 (2025-04-17) | MIT/Apache-2.0 | low activity | Ideas only | [crates.io](https://crates.io/crates/daggy) |
| `dagrs` | DAG task execution framework | 0.8.1 (2026-04-06) | MIT OR Apache-2.0 | linked GitHub repo is archived | Avoid | [crates.io](https://crates.io/crates/dagrs), [GitHub](https://github.com/dagrs-dev/dagrs) |
| `blake3` | Fast cryptographic hash | 1.8.7 (2026-08-20) | CC0-1.0 OR Apache-2.0 OR Apache-2.0 WITH LLVM-exception | core ecosystem | Dependency | [crates.io](https://crates.io/crates/blake3) |
| `xxhash-rust` | xxHash (non-crypto) | 0.8.18 (2026-07-21) | BSL-1.0 | active | Ideas only | [crates.io](https://crates.io/crates/xxhash-rust) |
| `twox-hash` | xxHash (non-crypto) | 2.1.4 (2026-08-27) | MIT | active | Ideas only | [crates.io](https://crates.io/crates/twox-hash) |
| `murmur3` | MurmurHash3 | 0.5.2 (2022-10-10, older) | MIT/Apache-2.0 | stale | Watch (only for Nextflow hash compatibility) | [crates.io](https://crates.io/crates/murmur3) |
| `sha1` / `sha2` / `md-5` (RustCrypto) | Standard digests | 0.11.0 (2026-03) | MIT OR Apache-2.0 | core ecosystem | Dependency (`sha1` for CWL) | [sha1](https://crates.io/crates/sha1), [sha2](https://crates.io/crates/sha2), [md-5](https://crates.io/crates/md-5) |
| `redb` | Pure-Rust embedded ACID key-value store | 4.3.0 (2026-09-15) | MIT OR Apache-2.0 | 4,808 stars; 373 commits in 6 months | Dependency (state/cache index) | [crates.io](https://crates.io/crates/redb), [GitHub](https://github.com/cberner/redb) |
| `rusqlite` | SQLite bindings | 0.40.2 (2026-08-08) | MIT | 4,411 stars; 146 commits in 6 months | Dependency (if SQL queries on run history are wanted) | [crates.io](https://crates.io/crates/rusqlite), [GitHub](https://github.com/rusqlite/rusqlite) |
| `fjall` | LSM-tree embedded store | 3.1.10 (2026-08-30) | MIT OR Apache-2.0 | 2,353 stars; 56 commits in 6 months | Watch | [crates.io](https://crates.io/crates/fjall), [GitHub](https://github.com/fjall-rs/fjall) |
| `sled` | Embedded database | 0.34.7 stable (2021-09-12, older); 1.0.0-alpha.124 | MIT/Apache-2.0 | 3 commits in 6 months | Avoid | [crates.io](https://crates.io/crates/sled), [GitHub](https://github.com/spacejam/sled) |
| `heed` | LMDB bindings | 0.22.1 (2026-04-07) | MIT | active | Ideas only | [crates.io](https://crates.io/crates/heed) |
| `sqlx` | Async SQL (Postgres, SQLite, MySQL) | 0.9.0 (2026-05-21) | MIT OR Apache-2.0 | active | Watch (for a future server mode) | [crates.io](https://crates.io/crates/sqlx) |
| `process-wrap` | Process groups, sessions, job objects around `Command` (tokio or std) | 10.0.1 (2026-09-23) | Apache-2.0 OR MIT | 144 commits in 6 months | Dependency (local executor) | [crates.io](https://crates.io/crates/process-wrap), [GitHub](https://github.com/watchexec/process-wrap) |
| `command-group` | Predecessor of process-wrap | 5.0.1 (2023-11-18, older) | Apache-2.0 OR MIT | superseded | Avoid | [crates.io](https://crates.io/crates/command-group) |
| `nix` | Unix APIs (signals, rlimits, namespaces) | 0.31.3 (2026-05-11) | MIT | core ecosystem | Dependency | [crates.io](https://crates.io/crates/nix) |
| `landlock` | Linux Landlock filesystem sandboxing | 0.4.7 (2026-07-27) | MIT OR Apache-2.0 | active | Watch | [crates.io](https://crates.io/crates/landlock), [GitHub](https://github.com/landlock-lsm/rust-landlock) |
| `hakoniwa` | Linux process isolation: namespaces, rlimits, cgroups, landlock, seccomp | 1.8.0 (2026-09-25) | LGPL-3.0-only WITH LGPL-3.0-linking-exception | 157 commits in 6 months | Watch (license review needed) | [crates.io](https://crates.io/crates/hakoniwa), [GitHub](https://github.com/souk4711/hakoniwa) |
| `birdcage` | Cross-platform sandbox | 0.8.1 (2024-04-19, older) | GPL-3.0-or-later | stale | Avoid | [crates.io](https://crates.io/crates/birdcage) |
| `libcontainer` (youki) | OCI container runtime library | 0.7.0 (2026-07-25) | Apache-2.0 | youki: 7,609 stars; 225 commits in 6 months | Watch (daemonless containers) | [crates.io](https://crates.io/crates/libcontainer), [GitHub](https://github.com/youki-dev/youki) |
| `regex` | Linear-time regex (no backreferences or look-around) | 1.13.1 (2026-07-15) | MIT OR Apache-2.0 | core ecosystem | Dependency | [crates.io](https://crates.io/crates/regex) |
| `fancy-regex` | Regex with backreferences and look-around (backtracking) | 0.19.2 (2026-09-13) | MIT | active | Dependency (Groovy/Java and Python regex features) | [crates.io](https://crates.io/crates/fancy-regex), [GitHub](https://github.com/fancy-regex/fancy-regex) |
| `pcre2` | PCRE2 bindings | 0.2.11 (2025-09-25) | Unlicense OR MIT | low activity | Watch | [crates.io](https://crates.io/crates/pcre2) |

- Sprocket's `wdl-engine` builds on `tokio`, `petgraph`, `blake3`, `lru`, `sysinfo`, `tokio-retry2` and `crankshaft`. It has a dedicated call-cache module ("The call cache provides caching of WDL task invocations") with its own hashing (`cache/hash.rs`) and a `digest.rs` for content digests - [wdl-engine Cargo.toml](https://github.com/stjude-rust-labs/sprocket/blob/main/crates/wdl-engine/Cargo.toml), [cache.rs](https://github.com/stjude-rust-labs/sprocket/blob/main/crates/wdl-engine/src/cache.rs).
- Nextflow's task hashing code uses Guava's `murmur3_128` (found in `HashBuilder.java`) - [nextflow HashBuilder.java](https://github.com/nextflow-io/nextflow/blob/master/modules/nf-commons/src/main/nextflow/util/HashBuilder.java).
- CWL v1.2 `File.checksum` "must be in the form "sha1$ + hexadecimal string" using the SHA-1 algorithm" - [CWL v1.2 spec](https://www.commonwl.org/v1.2/CommandLineTool.html). commonwl depends on `sha1` 0.11 - [commonwl Cargo.toml](https://github.com/fairagro/commonwl/blob/main/Cargo.toml).
- process-wrap is the "Successor to command-group" and does not implement a single cross-platform API - [process-wrap README](https://github.com/watchexec/process-wrap).
- hakoniwa: "Process isolation for Linux using namespaces, resource limits, cgroups, landlock and seccomp" - [hakoniwa README](https://github.com/souk4711/hakoniwa).
- fancy-regex uses backtracking to support look-around and backreferences, which pure automata regex does not; `regex` "guarantees linear time" matching - [fancy-regex README](https://github.com/fancy-regex/fancy-regex), [regex crates.io](https://crates.io/crates/regex).

### Inferences
- Nextflow is a dataflow language: channels, operators and processes fire as values arrive. It does not fit a static DAG. The iunctus core likely needs a dataflow runtime (tokio tasks plus channels) with petgraph used for static analysis, visualization and Snakemake/CWL graphs.
- Snakemake and CWL are closer to static DAGs (Snakemake has dynamic parts via checkpoints). petgraph plus a custom ready-queue is enough.
- `redb` is a good default for local state (pure Rust, no C, active). `rusqlite` is better if users need ad-hoc queries over run history (Snakemake keeps metadata files; Nextflow keeps a LevelDB cache per run).
- Nextflow `-resume` compatibility (reusing an existing `work/` directory) would require reproducing Nextflow's exact hash inputs with Murmur3-128. That is optional and probably not worth it for version 1.
- Java regex (Groovy `=~`, `replaceAll`) and Python `re` (Snakemake wildcard constraints) both support look-around and backreferences. `fancy-regex` is needed; neither crate replicates Java or Python regex dialects exactly.

### Gaps
- No Rust crate for general workflow scheduling (priorities, resource-aware packing, retries) that is mature and independent; every engine studied builds its own.
- No crate reproduces Java `java.util.regex` or Python `re` semantics exactly.

## 7. Storage and globbing

### Takeaway
`object_store` (Apache Arrow) and Apache OpenDAL are both healthy. `object_store` is lean and covers S3, Azure, GCS and HTTP. OpenDAL covers many more services. For globs, `globset` plus `walkdir` is the safe choice; `wax` has stalled.

### Cited Findings

| Crate | Purpose | Latest (date) | License | Activity | Fit | Source |
|---|---|---|---|---|---|---|
| `object_store` | Async object store (S3, Azure Blob, GCS, HTTP, local) | 0.14.2 (2026-09-15) | MIT/Apache-2.0 | apache/arrow-rs-object-store: 82 commits in 6 months | Dependency (default) | [crates.io](https://crates.io/crates/object_store), [GitHub](https://github.com/apache/arrow-rs-object-store) |
| `opendal` | "One Layer, All Storage" data access layer | 0.59.3 (2026-09-22) | Apache-2.0 | 5,395 stars; 627 commits in 6 months | Dependency (if broad backend list needed) | [crates.io](https://crates.io/crates/opendal), [GitHub](https://github.com/apache/opendal) |
| `aws-sdk-s3` | Official AWS S3 SDK | 1.150.0 (2026-09-25) | Apache-2.0 | very active | Ideas only (heavy; use when S3-specific features needed) | [crates.io](https://crates.io/crates/aws-sdk-s3) |
| `cloud-copy` (St. Jude) | Copy files to and from cloud storage | 0.10.1 (2026-08-27) | not checked | used by wdl-engine | Watch | [crates.io](https://crates.io/crates/cloud-copy), [GitHub](https://github.com/stjude-rust-labs/cloud-copy) |
| `globset` | Glob sets (from ripgrep) | 0.4.20 (2026-08-04) | Unlicense OR MIT | core ecosystem | Dependency | [crates.io](https://crates.io/crates/globset) |
| `glob` | Simple filesystem globbing | 0.3.4 (2026-07-21) | MIT OR Apache-2.0 | core ecosystem | Ideas only | [crates.io](https://crates.io/crates/glob) |
| `wax` | Portable globs with component semantics | 0.7.0 (2026-01-29) | MIT | 0 commits in 6 months | Avoid | [crates.io](https://crates.io/crates/wax), [GitHub](https://github.com/olson-sean-k/wax) |
| `walkdir` | Recursive directory walking | 2.5.0 (2024-03-01) | Unlicense/MIT | stable | Dependency | [crates.io](https://crates.io/crates/walkdir) |

- object_store is "A focused, easy to use, idiomatic, high performance, `async` object store" supporting AWS S3, Azure Blob Storage, GCS and HTTP; cloud features bundle a `reqwest` transport - [object_store README](https://github.com/apache/arrow-rs-object-store).
- OpenDAL "gives every language a unified way to access object storage, file storage, cloud SaaS, databases, protocols, and key-value services" - [OpenDAL README](https://github.com/apache/opendal).
- commonwl stages files through Local, S3 and HTTP(S) storage and depends on `aws-sdk-s3` - [commonwl README](https://github.com/fairagro/commonwl), [Cargo.toml](https://github.com/fairagro/commonwl/blob/main/Cargo.toml). Sprocket's wdl-engine uses `cloud-copy` and `globset` - [wdl-engine Cargo.toml](https://github.com/stjude-rust-labs/sprocket/blob/main/crates/wdl-engine/Cargo.toml).

### Inferences
- `object_store` is enough for TES-style staging (S3, GCS, Azure, HTTP). Pick OpenDAL only if users need SFTP, WebDAV, HDFS or similar.
- Nextflow's `file()` / `Channel.fromPath` globs follow Java NIO glob rules, and Snakemake's `glob_wildcards` uses its own pattern language. `globset` gives the matching engine, but iunctus must write thin translators for each dialect.

### Gaps
- Did not verify how closely `globset` syntax matches Java NIO glob syntax (for example `**` and `{a,b}` edge cases).

## 8. Containers and software environments

### Takeaway
Docker and Podman are covered by `bollard`. Conda is covered natively by `rattler` (conda org, very active): it can solve, download, install and activate environments without Python, so `conda:` directives can be served natively. Apptainer/Singularity, Slurm and LSF have no usable Rust API crates; existing Rust engines call their command-line tools. Nix has only a GPL-3.0 Rust re-implementation (snix). Seqera Wave is a REST service that can be called with plain HTTP.

### Cited Findings

| Crate / project | Purpose | Latest (date) | License | Activity | Fit | Source |
|---|---|---|---|---|---|---|
| `bollard` | Async Docker and Podman API client | 0.21.1 (2026-08-16) | Apache-2.0 | 1,374 stars; 87 commits in 6 months | Dependency | [crates.io](https://crates.io/crates/bollard), [GitHub](https://github.com/fussybeaver/bollard) |
| `podman-api` | Podman libpod API client | 0.11.0 (2026-02-13) | MIT | 0 commits in 6 months | Avoid | [crates.io](https://crates.io/crates/podman-api), [GitHub](https://github.com/vv9k/podman-api-rs) |
| `docker-api` | Docker API client | 0.14.0 (2023-06-05, older) | MIT | stale | Avoid | [crates.io](https://crates.io/crates/docker-api) |
| `oci-client` | Pull and push OCI images from registries | 0.18.0 (2026-09-18) | Apache-2.0 | active (ORAS project) | Watch | [crates.io](https://crates.io/crates/oci-client) |
| `oci-spec` | OCI image/runtime spec types | 0.10.0 (2026-05-27) | Apache-2.0 | active (youki) | Watch | [crates.io](https://crates.io/crates/oci-spec) |
| `rattler` family (`rattler`, `rattler_conda_types`, `rattler_solve`, `rattler_repodata_gateway`, `rattler_shell`, `rattler_lock`) | Conda ecosystem in Rust | rattler 0.50.0, rattler_conda_types 0.54.0, rattler_solve 10.0.0, rattler_repodata_gateway 0.33.6, rattler_shell 0.28.1, rattler_lock 0.33.1 (all 2026-09-24) | BSD-3-Clause | conda/rattler: 458 stars; 354 commits in 6 months | Dependency | [crates.io](https://crates.io/crates/rattler), [GitHub](https://github.com/conda/rattler) |
| pixi (prefix-dev) | Package manager built on rattler | v0.81.0 (2026-09-15) | BSD-3-Clause | 7,779 stars; 684 commits in 6 months | Ideas only | [GitHub](https://github.com/prefix-dev/pixi) |
| snix (`nix-compat`) | Rust re-implementation of Nix components | `nix-compat` on crates.io is a 0.0.0-pre placeholder (2025-03-16) | GPL-3.0-only | development on git.snix.dev | Avoid (license, maturity) | [crates.io](https://crates.io/crates/nix-compat), [snix](https://git.snix.dev/snix/snix) |
| Apptainer / Singularity | none found | only a tiny `bv-runtime-apptainer` (226 recent downloads) | n/a | n/a | CLI only | [crates.io search](https://crates.io/search?q=apptainer) |
| Slurm | `slurm` / `slurm-sys` bindings | 0.1.3 (2018-05-27, older) | not checked | abandoned | Avoid; use CLI | [crates.io](https://crates.io/crates/slurm) |
| Seqera Wave | Container provisioning service (Java) | n/a (REST API) | n/a | n/a | Call via HTTP (reqwest) | [Wave API docs](https://docs.seqera.io/wave/api) |

- bollard: "an asynchronous rust client library for the Docker/Podman API", with Podman as a first-class runtime and "automatic socket discovery for rootless Podman" - [bollard README](https://github.com/fussybeaver/bollard).
- crankshaft-engine depends on `bollard`, `tes`, `ssh2` and `nix` - [crankshaft-engine Cargo.toml](https://github.com/stjude-rust-labs/crankshaft/blob/main/crankshaft-engine/Cargo.toml).
- Sprocket's HPC backends call the command-line tools: `bsub`, `bjobs`, `bkill` (LSF), `sbatch`, `sacct`, `scancel` (Slurm), and an Apptainer executable - [lsf_apptainer.rs](https://github.com/stjude-rust-labs/sprocket/blob/main/crates/wdl-engine/src/backend/lsf_apptainer.rs), [slurm_apptainer.rs](https://github.com/stjude-rust-labs/sprocket/blob/main/crates/wdl-engine/src/backend/slurm_apptainer.rs), [apptainer.rs](https://github.com/stjude-rust-labs/sprocket/blob/main/crates/wdl-engine/src/backend/apptainer.rs).
- Rattler's aim "is to provide all functionality required to work with the conda ecosystem from Rust". Crates cover types, package streaming, repodata download, environment activation and solving, and it can create a complete environment from scratch (the README example installs cowpy "including Python") - [rattler README](https://github.com/conda/rattler), [docs.rs](https://docs.rs/rattler).
- Snix is "a modern Rust re-implementation of the components of the Nix package manager", licensed GPL-3.0 (protobuf definitions MIT). Its evaluator is described as a vehicle to evaluate Nixpkgs, "not a replacement for nix-build" (documentation dated 2025-03-14, older) - [snix](https://git.snix.dev/snix/snix), [snix components](https://snix.dev/docs/components/overview/).
- Wave builds containers on demand from Conda packages or Dockerfiles; requests go to `POST /container-token` and return a `containerToken`, `targetImage` and `expiration`; builds target linux/amd64 and linux/arm64 - [Wave API docs](https://docs.seqera.io/wave/api), [Wave GitHub](https://github.com/seqeralabs/wave).

### Inferences
- `conda:` directives (Nextflow and Snakemake) can be resolved natively with `rattler_solve` + `rattler` install + `rattler_shell` activation for the local executor. For TES, the environment must live inside the container: either build an image (Wave, or our own build using rattler inside a base image) or run a rattler-based bootstrap step inside the TES executor.
- Apptainer and HPC schedulers should be driven through their CLIs, as sprocket and crankshaft do.
- Nix support (`nix` directives are rare) should call the `nix` CLI; linking GPL snix code would change iunctus's license.
- A Wave client is a small reqwest wrapper; no crate is needed.

### Gaps
- No Rust Apptainer/Singularity API; no maintained Slurm, PBS or LSF client crates.
- Did not verify whether rattler handles every conda feature used in bioconda recipes (for example `pip:` sections in environment YAML files, which need a PyPI resolver; pixi uses a separate Python resolver for that).

## 9. Existing Rust workflow systems and task runners

### Takeaway
St. Jude's stack is the strongest precedent: `sprocket` and the `wdl-*` crates (parser, analysis, engine, LSP) sit on `crankshaft`, a language-independent headless task execution library with Docker, TES and generic HPC backends, which itself uses the `tes` crate. A second engine, `commonwl` (Rust CWL, FAIRagro), already reuses crankshaft and Boa and reports about 99% CWL conformance. That makes crankshaft a proven multi-language executor layer and commonwl a direct reuse candidate for iunctus's CWL front end.

### Cited Findings

| Project / crates | Purpose | Latest (date) | License | Activity | Fit | Source |
|---|---|---|---|---|---|---|
| `sprocket` | WDL workflow engine CLI (run, check, lint, format, inputs) | 0.31.0 (2026-09-16) | MIT OR Apache-2.0 | 216 stars; 289 commits in 6 months | Ideas only (architecture template) | [crates.io](https://crates.io/crates/sprocket), [GitHub](https://github.com/stjude-rust-labs/sprocket) |
| `wdl-grammar` 0.26.0, `wdl-ast` 0.27.0, `wdl-analysis` 0.26.0, `wdl-engine` 0.18.0, `wdl-lsp` 0.23.0, `wdl-lint` 0.28.0, `wdl-format` 0.21.0, `wdl` 0.23.7 | WDL parser, AST, analysis, engine, LSP | all 2026-09-16 | MIT OR Apache-2.0 | same repo | Ideas only | [crates.io wdl-engine](https://crates.io/crates/wdl-engine), [GitHub crates dir](https://github.com/stjude-rust-labs/sprocket/tree/main/crates) |
| `crankshaft` (+ `crankshaft-engine`, `-config`, `-docker`, `-events`, `-monitor`) | Headless task execution: Docker, TES, Generic (shell or SSH, e.g. LSF/Slurm) | 0.11.0 (2026-08-26) | MIT OR Apache-2.0 | 59 stars; 20 commits in 6 months | Dependency (executor layer) | [crates.io](https://crates.io/crates/crankshaft), [GitHub](https://github.com/stjude-rust-labs/crankshaft), [docs](https://stjude-rust-labs.github.io/crankshaft/) |
| `tes` | GA4GH TES types and client | 0.10.1 (2026-07-22) | MIT OR Apache-2.0 | 14 stars; 23 commits in 6 months | Dependency | [crates.io](https://crates.io/crates/tes), [GitHub](https://github.com/stjude-rust-labs/tes) |
| ga4gh-sdk (moved from elixir-cloud-aai to ga4gh org) | Generic SDK and CLI for GA4GH APIs (OpenAPI-generated) | not on crates.io; last commit 2026-09-24 | Apache-2.0 | 15 stars; 2 commits in 6 months; README "Basic usage: TBA" | Avoid (ideas only) | [GitHub](https://github.com/ga4gh/ga4gh-sdk) |
| `commonwl` family (`cwl_core`, `cwl_engine`, `cwl_engine_storage`, `cwl_salad`, `cwl_salad_derive`, `cwl-lsp`) | CWL parsing and execution in Rust | 0.15.1 (2026-09-09) | MIT OR Apache-2.0 | 2 stars; 295 commits in 6 months; crate owner JensKrumsieck | Dependency candidate (CWL front end), at least Ideas | [crates.io](https://crates.io/crates/commonwl), [GitHub](https://github.com/fairagro/commonwl) |
| `sciwin` (SciWIn client, FAIRagro) | Authoring and running CWL workflows; headless core library | 2.0.1 (2026-09-09) | MIT OR Apache-2.0 | 13 stars; 393 commits in 6 months | Ideas only | [crates.io](https://crates.io/crates/sciwin), [GitHub](https://github.com/fairagro/sciwin) |
| HyperQueue (IT4Innovations) | HPC sub-node task scheduler (server/worker, auto-allocation on PBS/Slurm) | v0.26.2 (2026-06-02); not on crates.io | MIT | 565 stars; 62 commits in 6 months | Ideas only; possible external executor target | [GitHub](https://github.com/It4innovations/hyperqueue), [docs](https://it4innovations.github.io/hyperqueue/stable/) |
| `torc` (GitHub org NatLabRockies) | YAML-spec workflow manager, local or Slurm | 0.41.0 (2026-09-25) | BSD-3-Clause | 19 stars; 419 commits in 6 months | Ideas only | [crates.io](https://crates.io/crates/torc), [GitHub](https://github.com/NatLabRockies/torc) |
| metis (JaeAeich) | GA4GH WES 1.1.0 service in Rust that wraps workflow runners | under development; last commit 2026-03-09 | not checked | 1 star | Ideas only | [GitHub](https://github.com/JaeAeich/metis) |
| `kerblam` | Rust CLI for managing data-analysis projects | 1.2.1 (2025-04-15) | not checked | low | Avoid | [crates.io](https://crates.io/crates/kerblam), [GitHub](https://github.com/MrHedmad/kerblam) |

Sprocket and the wdl crates (architecture, from source and docs):
- Crates: `wdl-grammar` (logos + rowan lexer/parser), `wdl-ast` (typed AST), `wdl-analysis` (document graph, types, diagnostics; uses rowan, petgraph, lsp-types), `wdl-engine` (evaluation and execution), `wdl-lsp`, `wdl-lint`, `wdl-format`, `wdl-doc`, `wdl-modules`, plus `sprocket-py` bindings - [sprocket crates](https://github.com/stjude-rust-labs/sprocket/tree/main/crates).
- `wdl-engine` modules: `eval/v1/{expr,task,workflow}.rs`, a `stdlib/` with one file per WDL standard library function, `backend/` (`local`, `docker`, `tes`, `apptainer`, `lsf_apptainer`, `slurm_apptainer`, `manager`), `cache` (call cache), `digest`, `inputs`, `outputs`, `path`, `value`, `config` - [wdl-engine src](https://github.com/stjude-rust-labs/sprocket/tree/main/crates/wdl-engine/src).
- The engine has its own internal trait `TaskExecutionBackend` with `name()`, `constraints(inputs, requirements, hints)`, `guest_inputs_dir()`, `needs_local_inputs()` and `execute(request) -> BoxFuture<Result<Option<TaskExecutionResult>>>` - [backend.rs](https://github.com/stjude-rust-labs/sprocket/blob/main/crates/wdl-engine/src/backend.rs).
- The `docker` and `tes` backends are implemented on top of crankshaft (`crankshaft::engine::service::runner::backend::{docker, tes}`, `crankshaft::engine::Task`) - [tes.rs](https://github.com/stjude-rust-labs/sprocket/blob/main/crates/wdl-engine/src/backend/tes.rs), [docker.rs](https://github.com/stjude-rust-labs/sprocket/blob/main/crates/wdl-engine/src/backend/docker.rs).
- Docs: "Sprocket evaluates a workflow locally and decomposes it into tasks. Sprocket dispatches these tasks to a configured execution backend responsible for starting, monitoring, and responding to task lifecycle events." Backends: Docker, TES, LSF + Apptainer (experimental), Slurm + Apptainer (experimental) - [Sprocket backends overview](https://sprocket.bio/reference/backends/overview).
- "Sprocket remains a pre-1.0 project"; CLI commands are "relatively settled"; configuration (`Sprocket.toml`) "is expected to undergo significant changes"; three-week release cycle - [Sprocket project status](https://sprocket.bio/about/project-status).

Crankshaft (architecture):
- "A headless task execution framework that supports local, cloud, and HPC." It "is developed independently of any particular workflow language ... multiple frontends based on different workflow languages can exist (and we hope this is the case)". It aims at "upwards of 20,000 concurrent tasks" - [crankshaft README](https://github.com/stjude-rust-labs/crankshaft).
- Docs describe four parts: Engine (named backends, spawns tasks, broadcasts events), Task (one or more container executions plus inputs, outputs, resources), Backend ("Docker, TES, and Generic (shell or SSH) ship in the box") and Event. It "is designed to manage tens to hundreds of thousands of concurrent tasks" and "doesn't parse workflows or provide a user interface" - [crankshaft introduction](https://github.com/stjude-rust-labs/crankshaft/blob/main/docs/guide/introduction.md).
- `Backend` trait: `fn run(&self, task: Task, events: Option<broadcast::Sender<Event>>, token: CancellationToken) -> Result<BoxFuture<'static, Result<NonEmpty<ExecutionResult>, TaskRunError>>>` - [backend.rs](https://github.com/stjude-rust-labs/crankshaft/blob/main/crankshaft-engine/src/service/runner/backend.rs).
- `Task` fields: `name`, `description`, `inputs`, `outputs`, `resources`, `executions` (non-empty list), `volumes`. This mirrors the TES task shape - [task.rs](https://github.com/stjude-rust-labs/crankshaft/blob/main/crankshaft-engine/src/task.rs).
- Default resources are 1 CPU, 2 GiB memory, 8 GiB disk; Generic backends fill unset values from configured defaults; cancellation makes TES "ask the server to cancel the remote task" and Generic "runs your kill command" - [crankshaft tasks docs](https://github.com/stjude-rust-labs/crankshaft/blob/main/docs/concepts/tasks.md), [engine docs](https://github.com/stjude-rust-labs/crankshaft/blob/main/docs/concepts/engine.md).

tes crate:
- Provides TES types (`types` feature) and "a simple client" (`client` feature); dependencies include reqwest, oauth2 and tokio-retry2; edition 2024, MSRV 1.88.0 - [tes README](https://github.com/stjude-rust-labs/tes), [Cargo.toml](https://github.com/stjude-rust-labs/tes/blob/main/Cargo.toml).

commonwl / SciWIn:
- "`commonwl` is a Rust-based library crate for parsing and executing CWL documents (`CommandLineTool`, `Workflow`, `ExpressionTool`, `Operation`). It is based on the headless task execution framework crankshaft." Features: `engine` and `tes`. Backends: Local, Docker, TES (all about 99% overall conformance; 100% Workflow and ExpressionTool); Slurm planned. Storage: Local, S3, HTTP(S). Also ships a `conformance` CLI and `cwl-lsp` ("Under construction") - [commonwl README](https://github.com/fairagro/commonwl).
- Workspace dependencies include crankshaft 0.11 (docker feature), boa_engine 0.21.1, bollard, serde-saphyr, aws-sdk-s3, sha1, tower-lsp-server, and RDF crates (`rdf`, `horned-owl`) for Schema Salad - [commonwl Cargo.toml](https://github.com/fairagro/commonwl/blob/main/Cargo.toml).

HyperQueue:
- A tool "to simplify execution of large workflows (task graphs) on HPC clusters"; server and workers; automatic allocation submits batch jobs; can run without Slurm/PBS; rich resource model (fractional, non-fungible, variants); interfaces: CLI, Python API, TOML workflow files - [HyperQueue docs](https://it4innovations.github.io/hyperqueue/stable/). The repo has crates `hyperqueue`, `tako` (scheduler) and `pyhq` - [GitHub](https://github.com/It4innovations/hyperqueue/tree/main/crates).

### Inferences
- Crankshaft is the natural executor layer for iunctus: its Task model already mirrors TES, it has Docker and Generic HPC backends, and two independent engines (sprocket, commonwl) use it. Risk: pre-1.0 API (0.11), small team, 20 commits in 6 months.
- Follow sprocket's layering: keep an iunctus-owned backend trait (with `constraints` and `execute`) above crankshaft, so that backends crankshaft lacks (Apptainer, native conda via rattler, HyperQueue) can be added without forking crankshaft.
- For CWL, reusing `cwl_core` / `cwl_salad` (parsing and Schema Salad model) is likely much cheaper than re-implementing, even if iunctus replaces `cwl_engine` with its own scheduler. The project is very active but has a tiny community (2 stars, one main maintainer); vendor or fork if needed.
- `tes` 0.10.1 is the right TES client, used via crankshaft or directly.
- HyperQueue is a good design reference for resource-aware scheduling and could be an extra executor target via its CLI.

### Gaps
- No existing Rust engine for Nextflow or Snakemake (only snakemake-lang's parser/compiler, and tree-sitter-nextflow's grammar).
- Did not verify the TES spec version (1.0 vs 1.1) implemented by the `tes` crate; the README does not state it in the lines checked.
- Did not measure crankshaft's real throughput claims.

## 10. Bioinformatics format crates for Nextflow operators

### Takeaway
`splitFasta`, `splitFastq`, `countFasta`, `countFastq`, `splitCsv` and similar operators can be implemented natively with `needletail` (fast FASTA/FASTQ with built-in compression support), `noodles` (broad formats, async, but API still experimental), and `csv`. `niffler` or `flate2` handle compressed inputs.

### Cited Findings

| Crate | Purpose | Latest (date) | License | Activity | Fit | Source |
|---|---|---|---|---|---|---|
| `needletail` | Minimal-copying FASTA/FASTQ parser and k-mer tools | 0.7.3 (2026-04-20) | MIT | 215 stars; 8 commits in 6 months; features include `compression` (default) and `xz2` | Dependency (split/count FASTA/FASTQ) | [crates.io](https://crates.io/crates/needletail), [GitHub](https://github.com/onecodex/needletail) |
| `noodles` (+ `noodles-fasta` 0.66.0, `noodles-fastq` 0.23.0) | Bioinformatics I/O: BAM, BCF, BED, BGZF, CRAM, CSI, FASTA, FASTQ, GFF3, GTF, SAM, tabix, VCF | 0.116.0 (2026-08-21) | MIT | 728 stars; 468 commits in 6 months | Dependency (when async or other formats needed) | [crates.io](https://crates.io/crates/noodles), [GitHub](https://github.com/zaeleus/noodles) |
| `bio` (rust-bio) | Algorithms and data structures for bioinformatics | 4.0.1 (2026-06-29) | MIT | 1,841 stars; 34 commits in 6 months | Ideas only | [crates.io](https://crates.io/crates/bio), [GitHub](https://github.com/rust-bio/rust-bio) |
| `seq_io` | Fast FASTA/FASTQ parsing | 0.3.4 (2025-03-14); 0.4.0-alpha.0 | MIT | low activity | Avoid | [crates.io](https://crates.io/crates/seq_io) |
| `csv` | CSV/TSV reading and writing | 1.4.0 (2025-10-17) | Unlicense/MIT | core ecosystem | Dependency (splitCsv) | [crates.io](https://crates.io/crates/csv) |
| `niffler` | "Simple and transparent support for compressed files" | 3.0.1 (2026-04-29) | MIT/Apache-2.0 | active | Dependency | [crates.io](https://crates.io/crates/niffler) |
| `flate2` | gzip/DEFLATE streams | 1.1.10 (2026-08-28) | MIT OR Apache-2.0 | core ecosystem | Dependency | [crates.io](https://crates.io/crates/flate2) |

- needletail aims to be "as fast as the readfq C library at parsing FASTX files" - [needletail README](https://github.com/onecodex/needletail).
- noodles supports the formats listed above; "keep in mind that the API is still considered experimental"; the `async` feature adds Tokio-based I/O for BAM, BCF, BGZF, CRAM, CSI, FASTA, FASTQ, GFF, SAM, tabix and VCF - [noodles README](https://github.com/zaeleus/noodles).

### Inferences
- needletail covers the hot path for `splitFasta`/`splitFastq`/`countFasta`/`countFastq`. Writing chunk files back out needs a small writer (needletail writes records; noodles has writers too).
- noodles' frequent minor-version bumps (0.116) mean pinning and regular upgrades.
- `splitCsv` options (header, sep, quote, skip) map to `csv::ReaderBuilder`, but Groovy-style row values (lists or maps) must be built in the iunctus value model.

### Gaps
- Did not check exact Nextflow operator semantics (record grouping by `by`, `size`, `file: true` naming). That belongs to the language research.

## 11. Recommended stack and gaps

### Takeaway
Everything below the language layer (async runtime, graphs, hashing, state, storage, containers, conda, TES, bio formats) is covered by mature, actively maintained crates. The language layer is where the work is: a native Groovy-subset interpreter for Nextflow must be written from scratch; Snakemake needs a custom rule-syntax front end plus a Python evaluator (Monty, with PyO3/CPython as fallback); CWL is largely solved by existing Rust crates (commonwl + Boa).

### Cited Findings
- crankshaft is language-independent and already used by two Rust engines (sprocket for WDL, commonwl for CWL) - [crankshaft README](https://github.com/stjude-rust-labs/crankshaft), [commonwl README](https://github.com/fairagro/commonwl), [wdl-engine tes.rs](https://github.com/stjude-rust-labs/sprocket/blob/main/crates/wdl-engine/src/backend/tes.rs).
- The official Nextflow grammar is ANTLR (`ScriptParser.g4`), mirrored by tree-sitter-nextflow 0.4.0 - [tree-sitter-nextflow](https://github.com/nextflow-io/tree-sitter-nextflow).
- Monty runs a Python 3.14 subset with Ruff's parser and no third-party imports - [Monty](https://github.com/pydantic/monty).
- Boa 0.22 reaches 95.60% Test262; CWL needs ECMAScript 5.1 - [Boa releases](https://github.com/boa-dev/boa/releases), [CWL spec](https://www.commonwl.org/v1.2/CommandLineTool.html).

### Inferences
Recommended stack (our assessment):

- Front-end infrastructure: `logos` + hand-written recursive descent parsers + `rowan` lossless trees + typed AST wrappers (the sprocket and rust-analyzer pattern). Diagnostics via `codespan-reporting` or `miette` (both used by sprocket or commonwl).
- Nextflow: own parser mirroring `ScriptParser.g4`; `tree-sitter-nextflow` (git dependency) as a differential test oracle and editor grammar; own tree-walking interpreter for the Groovy subset and Nextflow runtime objects; `fancy-regex` for Java-style regex; `winnow` for small value grammars (memory units, durations).
- Snakemake: own rule-syntax parser; `ruff_python_parser` / `ruff_python_ast` pinned to an exact `0.0.x` version for Python blocks (see snakemake-lang for a working example); `monty` as the default evaluator; `pyo3` with embedded CPython as an optional feature for Snakefiles that import pandas and similar packages.
- CWL: `serde-saphyr` (and `saphyr` where spans are needed); reuse `cwl_core`/`cwl_salad` from commonwl or fork them; `boa_engine` for expressions (rquickjs as fallback); `sha1` for `File.checksum`.
- Schemas: `jsonschema` for `nextflow_schema.json` and samplesheets (draft 2020-12); nf-schema-specific behavior written by hand.
- Core runtime: `tokio`; `petgraph` for static graphs; own dataflow scheduler for Nextflow channels; `blake3` for content hashes and cache keys; `redb` (or `rusqlite`) for local state and cache index.
- Execution: `crankshaft` (Docker, TES, Generic HPC) behind an iunctus-owned backend trait; `tes` crate for TES; `process-wrap` + `nix` for the local executor; optional `hakoniwa`/`landlock` sandboxing after license review; Apptainer, Slurm and LSF via their CLIs.
- Storage: `object_store` (or OpenDAL if more services are required); `globset` + `walkdir` with per-language glob translators.
- Environments: `bollard` for Docker and Podman; `rattler` crates for native `conda:` support; Wave via a small reqwest client; Nix via the `nix` CLI.
- Bio operators: `needletail`, `noodles`, `csv`, `niffler`/`flate2`.
- Avoid: `serde_yaml`, `serde_yml`, `yaml-rust`, `sled`, `dagrs`, `docker-api`, `podman-api`, `rustpython-parser`, `antlr-rust`, `quick-js`, `rustyscript`, JVM embedding for Groovy.

### Gaps
Places where nothing usable exists today (as of 2026-09-27):
- A Groovy interpreter or Groovy runtime semantics in Rust (GDK methods, GString, closure delegation, Java types). The only candidate, `groovy`/groovyrs, is two months old with 109 downloads.
- A Nextflow semantic model in Rust (type checking, resolution of includes, config file evaluation). Only a tree-sitter grammar exists, and not for `nextflow.config`.
- A Snakemake implementation in Rust. Only snakemake-lang (parser and compiler to Python, early, 0 stars) exists.
- A pure-Rust Python runtime that can load CPython C-extension packages. Full Snakefile compatibility therefore needs embedded CPython.
- Apptainer/Singularity, Slurm, PBS and LSF API crates. Everything uses CLIs.
- A permissively licensed Rust Nix evaluator (snix is GPL-3.0 and not a nix-build replacement).
- A Rust Wave client and a Rust nf-schema implementation (validation is covered by `jsonschema`; samplesheet conversion is not).
- Exact Java-regex, Python-regex and Java-NIO-glob dialect compatibility crates.
- A standalone, mature workflow scheduling crate (resource-aware packing, retries, priorities); all engines studied roll their own.
