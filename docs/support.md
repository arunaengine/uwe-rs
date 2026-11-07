<!-- Which language versions, executors, staging modes and storage providers this revision of uwe supports. -->
<!-- Copyright (c) 2026 The uwe Contributors -->
<!-- SPDX-License-Identifier: MIT or Apache-2.0 -->

# What uwe supports

This page describes uwe 0.1.0. `uwe --version` prints the version you run. Where a row says "refused", uwe stops with exit code 33 and a message before any task starts. It never runs such a workflow with different behavior.

## Languages

| Language | Follows | Not supported |
| --- | --- | --- |
| Nextflow | DSL2 as in Nextflow 26.04 | DSL1 scripts, some directives and options (refused by `uwe check`), Nextflow's `work/` and `.nextflow` layouts |
| Snakemake | Snakemake 9.27 | Group jobs on a local run, editable, local or VCS `pip:` requirements, Python objects that cross into container helpers |
| CWL | v1.0, v1.1 and v1.2 | Direct in-place updates of a Directory, and conflicting views of one parent folder (both refused) |
| Argo Workflows | `argoproj.io/v1alpha1`, as in Argo 4 (older fields are converted as by `argo convert`) | Sidecars and daemon steps on SLURM and TES |
| WDL | 1.0, 1.1 and 1.2 | `else` clauses, enumerations, `hints`, `input` and `output` literals, `read_object`, `read_objects`, `write_object`, `write_objects`, the header forms of `read_tsv` and `write_tsv`, retries (`maxRetries`), disks, outputs whose files only the task's results name (like `read_lines(stdout())` as `Array[File]`), and the inputs of calls in subworkflows from the input file |
| wir | Versions 0.1 and 0.2, see [Running wir documents](wir.md) | Reserved op kinds, and native blocks other than CWL and Nextflow (refused) |

Each language is a subset that uwe implements in Rust. The exact limits are in [Known pitfalls](pitfalls.md). `uwe check <workflow>` answers the question for one workflow.

## Executors

| Executor | Container runtime | Staging modes |
| --- | --- | --- |
| `local` | Docker or Podman | Local folders |
| `slurm` | Apptainer (default), Singularity, Podman or Docker on the nodes | `shared-path`, `object-store` |
| `kubernetes` | The container runtime of the cluster | `shared-path`, `object-store`, `kubedal` |
| `tes` | The runtime of the TES server, for example Funnel | Server-side, or `object-store` |
| `simulator` | None, outputs are placeholders | None |

Object-store staging on SLURM and Kubernetes needs a static Linux build of uwe for the machine type of the nodes. Tasks with several containers need `shared-path`. See [Remote executors and staging](remote-executors.md).

## Storage providers

Inputs can come from S3 (and S3-compatible services), Google Cloud Storage, Azure Blob Storage, HTTPS, FTP and local folders. Outputs and the store need a provider that can be written: S3, Google Cloud Storage, Azure Blob Storage or a local folder. Any number of providers can be configured at once. Repositories come from GitHub, GitLab, Bitbucket and Gitea through their APIs, and from any Git server over HTTPS. SSH Git URLs are not supported.

## Hosts

uwe builds and runs on Linux and macOS, and on Windows through WSL 2. The static release build is for x86_64 Linux. Remote nodes that receive the uwe binary must run Linux.

## How it is tested

| Area | Tested by |
| --- | --- |
| Parsers, planners, runtimes, store and database | The workspace tests, on every push |
| Local Docker runs: failure, resume, cancellation, simulation | Real containers in CI, and `just container-runs` |
| Language coverage | Recorded outcomes of the nf-core, Snakemake, CWL conformance and Argo example corpora (`just` recipes, run on request) |
| S3, Google Cloud Storage, Azure | `just s3-test` against RustFS, `just emulator-test` against fake-gcs-server and Azurite |
| TES | `just tes-test` against a local Funnel server |
| Kubernetes | `just kind-staging-test` against a local kind cluster |
| SLURM | Tests with simulated SLURM tools, and use on a real cluster |

Not yet tested on live systems as part of a release: rootless Podman and Docker, managed Kubernetes services, cloud TES services, and recovery after a crash of the machine that runs uwe on every remote executor.
