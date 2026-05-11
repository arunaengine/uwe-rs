<!-- What nextflow-parser covers, how it is checked against the official parser, and known differences. -->
<!-- Copyright (c) 2026 The iunctus Contributors -->
<!-- SPDX-License-Identifier: MIT or Apache-2.0 -->

# nextflow-parser

Reads Nextflow scripts (`.nf`) and config files (`.config`) into syntax trees. It follows the official strict syntax grammar, which Nextflow uses by default since version 26.04 (`modules/nf-lang/src/main/antlr` in the Nextflow repository). Legacy syntax that strict syntax rejects is rejected here too, with the official error messages where practical.

```rust
let parsed = nextflow_parser::parse_script(source);
for error in &parsed.errors {
    // Each error has a message and a byte span; LineIndex turns spans into lines and columns.
}
let items = parsed.tree.items;
```

`parse_config` works the same way for config files. The parser recovers after an error and keeps going, so it can report more errors than `nextflow lint`, which stops at the first one.

## How it is checked

- Unit tests next to the code cover precedence, line break rules, calls without parentheses, closures, strings and statements. The tests in `tests/` cover script declarations, config files and the lexer.
- `just test-workflows` fetches a text-only copy of all 144 nf-core pipelines and the nf-core/modules repository, at the commits listed in `tests/fixtures/nf_core_revisions.txt`.
- `just test-workflows-test` parses all 14,624 scripts and configs in it. It passes when the parser rejects exactly the files that `nextflow lint` 26.04.6 rejects with syntax errors, listed in `tests/fixtures/nf_core_rejected.txt`, and accepts all others. `nf-test.config` files are skipped because they belong to nf-test.

## Known differences

- Invalid escapes such as `\|` in a string: Nextflow 26.04.6 accepts them, but Nextflow reports them since 2026-09-18 ([nextflow-io/nextflow#7608](https://github.com/nextflow-io/nextflow/pull/7608)). This parser follows the newer behaviour. Two nf-core files are affected; both are listed in the fixture.
- Checks that depend on typed processes (`nextflow.enable.types`) are not made yet, and warnings are not reported.
