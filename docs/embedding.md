<!-- Running uwe inside a service through its Rust API, with hosted policies and Kubernetes references. -->
<!-- Copyright (c) 2026 The uwe Contributors -->
<!-- SPDX-License-Identifier: MIT or Apache-2.0 -->

# Embedding uwe in a service

uwe is a library as well as a command. A service such as [Aruna](https://github.com/arunaengine) can start runs through the Rust API and keep its own control over users, quotas and data. This page assumes the [presets](presets.md) are set up.

## Starting a run

Set `run::Settings.preset`, `config_file`, the typed JSON `inputs` and a `hosted::Hosted` policy that the server creates. Then call `RunRequest::new(settings).run().await`.

`Catalogue::prepare` returns a guard for the temporary input file and the verified `task_runner::References`. Keep both alive for the whole submission.

## Choosing the run id

Set `run::Settings.run_id` to give the run an id of the host, such as its own job id. Then the host knows the id before the run starts and can read the run database at once. The same id names the scratch folders, the run page, the telemetry and the record of the run.

The id has 1 to 64 ASCII letters, digits, `-` or `_`. It starts and ends with a letter or a digit. uwe refuses any other id with a usage failure. It also refuses an id that the run database already holds, before any task starts.

```rust
let mut settings = uwe::run::Settings::new("/srv/workflows/main.cwl");
settings.run_id = Some("01JBXQ5Z7C2W9M".into());
let run = uwe::RunRequest::new(settings).run().await?;
assert_eq!(run.id, "01JBXQ5Z7C2W9M");
```

## The hosted policy

The policy limits which presets are allowed and which cache, database and output paths a tenant gets. It also limits Argo host roots and volume mappings. Its authorization callback sees the merged inputs before planning, and the normalized inputs before execution.

Hosted runs always use strict admission and strict provenance. Local execution in a hosted run needs explicit CPU and memory capacity. The service itself remains responsible for authentication, total quotas, file system access and network policy.

## Kubernetes

Pass `Setup.cluster` with `kubernetes_executor::Cluster::new(client, namespace, server)`, or set `in_cluster = true` in `[executor.kubernetes]`.

Map installed references through named `[executor.kubernetes.references.NAME]` tables. Each has a `path` and either a `claim`, or an `nfs_server` and `nfs_path`. Only declared reference paths are mounted, and always read-only. Local and mapped Kubernetes references avoid copying the whole installation. Remote SLURM and TES staging still checks the bytes of the source.

## Sharing between requests

Share one `argo_runtime::SharedLocks` coordinator between related hosted policies, so that Argo mutexes and semaphores hold across requests. Requests that overlap must use equivalent registry settings. Use separate worker processes for different registry identities.

## Operator commands

- `uwe run -strict-provenance` makes a failure to keep provenance fatal.
- `uwe clean -front` only evicts the local front of a layered store.
- `uwe reconcile RUN -reason TEXT` records an operator decision about a run. It works only once the local owner of the run has provably stopped. Foreign or uncertain owners must be reconciled on their backend first.
