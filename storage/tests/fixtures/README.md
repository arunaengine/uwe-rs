<!-- Where the recorded files of the storage tests come from. -->
<!-- Copyright (c) 2026 The uwe Contributors -->
<!-- SPDX-License-Identifier: MIT or Apache-2.0 -->

# Storage fixtures

`quilt_manifest.jsonl` is the manifest that nf-quilt 1.0.1 pushed with Nextflow 26.04.6 on
2026-11-06 to a local RustFS bucket `nfquilt-test` for the package `test/probe`, from a
process that published `out/a.txt`, `out/report.html` and `out/table.tsv`. The scratch path
is written `<scratch>`, the home folder `<home>`, the user `<user>`, the host `<host>`, and
base64 fields `<masked base64>`, so its name hash no longer matches its content.

`cas_store/` holds the blocks, publish coordinates, Store Log entry and `store-id` of the store
`lab` that nf-blocks 0.11.0-beta.1 wrote with Nextflow 26.04.6 and nf-amazon 3.9.2 on
2026-11-06, byte for byte, from a pipeline whose workflow output `aligned` published `A/A.bam`
and `B/B.bam` (`bam-A` and `bam-B`). The blocks hold no machine data, so their CIDs match.
`cas_blocks.json` is the same DAG-CBOR blocks as JSON, links written `{"/": "<cid>"}`, from a
decoder written for the plugin's specification.
