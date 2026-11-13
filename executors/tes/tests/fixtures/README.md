<!-- The TES requests of the real nf-ga4gh 1.5.0, which a test compares uwe's with. -->
<!-- Copyright (c) 2026 The uwe Contributors -->
<!-- SPDX-License-Identifier: MIT or Apache-2.0 -->

# TES fixtures

`funnel.yaml` configures the Funnel server of the Funnel tests.

`nf_ga4gh_requests.jsonl` was recorded on 2026-11-06 at about 13:29 Europe/Berlin with Nextflow
26.04.6 and nf-ga4gh 1.5.0, against a fake TES server on 127.0.0.1 that answered every task as
complete at once; nothing ran. The pipeline had one process `ALIGN` (2 CPUs, 3 GB, 10 GB of
disk, 1 hour, one input file, a plain and a glob output), with `process.container =
'ubuntu:24.04'`, `tes.endpoint` on the fake, `tes.oauthToken` and the tags `project = 'demo'`
and `owner = 'lab'`.

One JSON object per request, in arrival order: method, path, the headers that matter and the
body. The bearer token is masked; `<probe>` stands for the recording folder, and the task
folder differs per run.
