<!-- The Kubernetes requests of the real nf-k8s 1.5.4-patch1, which a test compares. -->
<!-- Copyright (c) 2026 The uwe Contributors -->
<!-- SPDX-License-Identifier: MIT or Apache-2.0 -->

# Kubernetes fixtures

`nf_k8s_requests.jsonl` was recorded on 2026-11-06 at about 13:38 Europe/Berlin with Nextflow
26.04.6 and nf-k8s 1.5.4-patch1, against a fake Kubernetes API on 127.0.0.1 that ran each
container command on the recording machine and reported the pod as terminated. No cluster ran.

The pipeline had two processes: `TRAIN` (2 CPUs, 3 GB, 20 GB of disk, 1 hour, one accelerator of
type `nvidia.com`, queue `gpu-queue`, resource labels `team: 'genomics'` and `'cost-center': 'A
12'`) and `SMALL`, with `process.container = 'ubuntu:24.04'`, the namespace `lab`, the service
account `runner`, Jobs, and for `TRAIN` nine pod options: a node selector, a label, an
annotation, a toleration, an environment variable, a Secret key mount, a read-only claim, an
image pull secret and a priority class.

One JSON object per request, in arrival order: method, path, the headers that matter and the
body. The bearer token is masked; `<probe>` stands for the recording folder, and the run name,
session id and Job name suffixes differ per run.
