<!-- The Nomad requests of the real nf-nomad 0.5.0-edge7, which the tests compare uwe's with. -->
<!-- Copyright (c) 2026 The uwe Contributors -->
<!-- SPDX-License-Identifier: MIT or Apache-2.0 -->

# Nomad fixtures

`nf_nomad_requests.jsonl` was recorded on 2026-11-06 at about 13:39 Europe/Berlin with Nextflow
26.04.6 and nf-nomad 0.5.0-edge7, against a fake Nomad HTTP API on 127.0.0.1 that ran each job's
command on the recording machine and reported one complete allocation. No Nomad agent ran.

The pipeline had two processes: `ALIGN` (2 CPUs, 3 GB, 1 hour, 1 GPU) and `SMALL` (no
resources), with `process.container = 'ubuntu:24.04'`, the config `nomad.client.token`, the
datacenter `dc1`, the namespace `lab`, cleanup `never`, and for `ALIGN` the `nomadOptions`
node pool `highmem`, meta `owner: team-x`, priority `high` and reschedule `attempts: 2, delay:
'10s'`.

One JSON object per request, in arrival order: method, path, the headers that matter and the
body. The `x-nomad-token` value is masked; `<probe>` stands for the recording folder, and the
session id, session name and task hashes differ per run.
