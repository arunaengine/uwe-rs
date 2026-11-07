<!-- The AWS Batch requests of the real nf-amazon 3.10.0, which the tests compare uwe's with. -->
<!-- Copyright (c) 2026 The uwe Contributors -->
<!-- SPDX-License-Identifier: MIT or Apache-2.0 -->

# AWS Batch fixtures

`nf_amazon_requests.jsonl` was recorded on 2026-11-06 at about 13:40 Europe/Berlin with
Nextflow 26.04.6 and nf-amazon 3.10.0, against a fake AWS Batch on 127.0.0.1 (named with
`AWS_ENDPOINT_URL_BATCH`) and a private RustFS container for S3. The fake answered every job as
succeeded, so no container ran. The pipeline had two processes: `ALIGN` (2 CPUs, 3 GB, 1 hour,
1 GPU, two resource labels, container options) and `SMALL` (a samtools image), with the config
`process.queue = 'probe-queue'`, `aws.region = 'eu-west-1'` and an `aws.batch` block with a job
role, 3 spot attempts, the volumes `/scratch` and `/refs:/refs:ro`, the log group
`/probe/batch`, the share `blue` and the priority 10.

One JSON object per request, in arrival order: method, path, the headers that matter and the
body. The `authorization` value is masked; the user agent, the `nextflow.io/createdAt` times,
the `nf-token` digests and the S3 task folders differ per run.
