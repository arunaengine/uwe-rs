<!-- Copyright (c) 2026 The iunctus Contributors -->
<!-- SPDX-License-Identifier: MIT or Apache-2.0 -->

# Support and qualification

This matrix describes the 0.1.0 development line as of 2026-10-03. It is a capability boundary, not a production qualification certificate. Qualification must identify the exact source revision, binary digest, workflow/input identities, container digests, platform, backend configuration and observed results. Historical corpus counts in the [roadmap](roadmap.md) are separate evidence.

## Workflow languages

All four languages use the shared task graph and runner. Simulation produces synthetic files and can exercise planning; it does not establish tool output correctness, resource behavior or full compatibility with a native engine.

| Language | Implemented planning and execution subset | Deliberate differences and limits | Qualification still needed |
| --- | --- | --- | --- |
| Nextflow | DSL2 processes, workflows, channels and supported operators; container scripts, declared outputs and delivery | Native Rust Groovy subset, no JVM; DSL1 and class inheritance unsupported; only implemented plugins are available | Full real pipeline results against pinned Nextflow, including dynamic channels, retries, failure hooks and delivery |
| Snakemake | Rules, wildcards, supported checkpoints, container shell tasks and supported embedded code | Native Python subset with container package helpers; arbitrary native Python objects cannot cross the helper boundary; bounded same-image `service()` groups implemented, with internal dependency ordering and group retries pending; FIFO `pipe()` scheduling unavailable | Real outputs against pinned Snakemake, package/environment identity and supported checkpoint behavior |
| CWL | CommandLineTool, workflows, supported scatter/expression features, declared files/directories and outputs | Supported JavaScript subset; image build/load/import requirements unavailable; ontology format relationships beyond exact matches unavailable | Real conformance outputs, writable input modes, failure/output collection and directory fidelity |
| Argo | Supported steps/DAGs, loops, artifacts, container/containerSet templates and child workflows | Translated into the shared runner; cluster resource templates, daemons, sidecars and data templates unavailable | Real loop/aggregation/parallelism and artifact outcomes against pinned reference cases |
| Plain host scripts | No separate workflow-language contract | No plain host task execution path; supply a supported workflow and container image | A separate language contract would require an explicit design decision |

Unsupported constructs should produce a diagnostic, typically exit code 33; parser acceptance alone is not a parity guarantee. See [checks](design/checks.md), [language decisions](decisions.md) and the language-specific runtime pages under [design](README.md#design). Workflow interpretation is native; task and package execution requires containers. Conda locks from local repodata are implemented, but Conda-to-image preparation is unfinished.

## Executors and platforms

| Executor | Implemented behavior | Deployment prerequisites and limits | Qualification status |
| --- | --- | --- | --- |
| Simulator (default) | Plans and makes synthetic outputs without running task containers | No real task result, cancellation or ownership evidence | Planning/corpus checks only |
| Local Docker/Podman | Parallel container execution through the engine API | Reachable engine socket; usable images; explicit host inputs and writable paths operate under caller/engine permissions | Local checks and historical real-run evidence exist; current real Docker, rootless Podman, mixed UID and platform qualification remain separate gates |
| TES | Submit, poll, cancel and journal task ownership | Server must see staged inputs, store downloads and scratch at matching paths; portable direct-store staging is unfinished; offline execution capability is limited | Fake-server checks and historical Funnel evidence; current live-server/restart qualification outstanding |
| SLURM | Container batch tasks with Apptainer or Podman | Scheduler tools and shared store/scratch paths; backend-specific image, network and GPU limits | Simulated scheduler and historical cluster evidence; current real scheduler qualification outstanding |
| Kubernetes | Container Jobs through the Kubernetes API | Configured namespace and nodes with matching shared host paths; backend-specific network and GPU limits | Fake API checks; current live-cluster qualification outstanding |

Linux x86_64 is the initial locally tested build platform. That host evidence does not establish production support. Other host architectures and macOS/Windows have no qualification claimed here. Container platform compatibility must be checked for each image and executor; a host build alone does not establish it. See [executor configuration and shared paths](design/executors.md) and [storage configuration](design/storage.md).

Real backend acceptance must cover exact output bytes, missing/failed outputs, cancellation and timeout, cleanup after interruption, restart ownership, rootless/mixed UID access, resource limits, image platform selection and supported storage configurations. Ordinary green Rust tests and fake services do not replace these checks. Corpus gates must use complete pinned inputs and oracle results. Ignored container and corpus tests are external gates until explicitly invoked with their prerequisites.

## Installation and operation

Use the [root installation instructions](../README.md#build-and-install-from-source). `iunctus help` lists options; `iunctus check-machine` checks executor prerequisites and `iunctus check <workflow>` diagnoses supported planning constraints. These checks do not run tools or prove successful real results. A task without a workflow image requires `-default-image`; host-installed packages are not a fallback.

The config defaults to `~/.config/iunctus/config.toml`. The cache defaults to `$XDG_CACHE_HOME/iunctus`, else `~/.cache/iunctus`; `runs.sqlite` is stored there. Preserve the actual configured store, scratch and download paths when moving between machines or remote backends. The local run page binds to `127.0.0.1` by default; `-address` is an explicit choice to make its run data reachable elsewhere. See [runs and page options](design/runs.md).

Preview retention with `iunctus clean` before requesting deletion with `-remove`. Resolve retained remote ownership records before removing associated scratch. Do not manually erase unresolved attempt journals: they preserve information needed to determine whether submitted work is still running.

## Upgrade, backup and recovery

Before upgrading, stop admitting new work, finish or explicitly reconcile active remote work, and stop all local writers. Record the existing binary/source digest and configuration. Back up the run database together with its WAL state using a SQLite-consistent backup, or copy it after all writers have stopped and the WAL has been checkpointed. Also preserve the configured store, retained scratch/journals, workflow/input snapshots and referenced image digests. A database-only backup cannot restore missing output bytes.

Database opening applies forward schema migrations and refuses newer schema versions. There is no downgrade migration or cross-version rollback guarantee. Test an upgrade on copies first. Rollback requires the previous binary and matching pre-upgrade database/store/configuration copies; replacing only the executable can leave incompatible state. Reconcile remote ownership before resuming restored state so live jobs are not duplicated.

Resume and repeat are implemented, but exact historical computation depends on retained input bytes, images and configuration. Retention, corruption recovery, multi-host filesystem behavior, restoration and browser qualification remain explicit acceptance work. The [run design](design/runs.md) and [reproducibility design](design/reproducibility.md) describe the mechanisms; they do not certify a completed recovery exercise.
