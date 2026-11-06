<!-- Where the recorded files of the storage tests come from. -->
<!-- Copyright (c) 2026 The uwe Contributors -->
<!-- SPDX-License-Identifier: MIT or Apache-2.0 -->

# Storage fixtures

`quilt_manifest.jsonl` is the manifest that nf-quilt 1.0.1 pushed with Nextflow 26.04.6 on
2026-11-06 to a local RustFS bucket `nfquilt-test` for the package `test/probe`, from a
process that published `out/a.txt`, `out/report.html` and `out/table.tsv`. The scratch path
is written `<scratch>`, the home folder `<home>`, the user `<user>`, the host `<host>`, and
base64 fields `<masked base64>`, so its name hash no longer matches its content.
