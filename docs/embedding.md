<!-- Running uwe inside a service through its Rust API, with hosted policies and Kubernetes references. -->
<!-- Copyright (c) 2026 The uwe Contributors -->
<!-- SPDX-License-Identifier: MIT or Apache-2.0 -->

# Embedding uwe in a service

uwe is a library as well as a command. A service such as [Aruna](https://github.com/arunaengine) can start runs through the Rust API and keep its own control over users, quotas and data. This page assumes the [presets](presets.md) are set up.

## Starting a run

Set `run::Settings.preset`, `config_file`, the typed JSON `inputs` and a `hosted::Hosted` policy that the server creates. Then call `RunRequest::new(settings).run().await`.

`Catalogue::prepare` returns a guard for the temporary input file and the verified `task_runner::References`. Keep both alive for the whole submission.

## Pinning a repository revision

A `target` can name a remote Git repository, such as `nf-core/demo` or `https://github.com/nf-core/demo`. Set `run::Settings.revision` to a tag, branch or commit of it. This works like `uwe run -r`. A full commit pins the run: uwe reuses a cached checkout of that commit, or fetches that exact commit. Set `latest` to resolve a remembered branch again, like `-latest`.

uwe refuses a revision for a local target, and a revision or `latest` together with a preset. A preset names its own full commit.

```rust
let mut settings = uwe::run::Settings::new("nf-core/demo");
settings.revision = Some("0123456789abcdef0123456789abcdef01234567".into());
let run = uwe::RunRequest::new(settings).run().await?;
```

## Choosing the run id

Set `run::Settings.run_id` to give the run an id of the host, such as its own job id. Then the host knows the id before the run starts and can read the run database at once. The same id names the scratch folders, the run page, the telemetry and the record of the run.

The id has 1 to 64 ASCII letters, digits, `-` or `_`. It starts and ends with a letter or a digit. uwe refuses any other id with a usage failure. It also refuses an id that the run database already holds, before any task starts.

```rust
let mut settings = uwe::run::Settings::new("/srv/workflows/main.cwl");
settings.run_id = Some("01JBXQ5Z7C2W9M".into());
let run = uwe::RunRequest::new(settings).run().await?;
assert_eq!(run.id, "01JBXQ5Z7C2W9M");
```

## Default task time

Set `run::Settings.default_time` to a `std::time::Duration` to give a time limit to every task that sets none of its own. A task's own time always wins. Without it, uwe uses `task_minutes` of `[executor]` in the config file, and else no limit. A host that passes its own `setup` skips the config file, so it sets `default_time` itself. uwe refuses zero. A part of a second counts as a whole second.

The default reaches every executor like a task's own time, including the `staging_minutes` margin of TES, SLURM and Kubernetes. See [Default task time](remote-executors.md#default-task-time).

```rust
let mut settings = uwe::run::Settings::new("/srv/workflows/main.cwl");
settings.default_time = Some(std::time::Duration::from_secs(24 * 60 * 60));
let run = uwe::RunRequest::new(settings).run().await?;
```

## Steps that never use the cache

Set `run::Settings.no_cache_steps` to the names of steps whose tasks must not use the cache. Such a task looks up no stored result and keeps none, like a Nextflow process with `cache false`. So it runs on every run, also with `-resume`. Its outputs still reach the steps after it, and those steps can still come from the cache. On the command line this is `-no-cache-steps a,b`.

A name matches a step as the step graph names it, like the step of `Event::Task`: by its full name, or by the last part after `:` or `/`. So `FASTQC` matches the Nextflow process `NFCORE_DEMO:DEMO:FASTQC`, and `sort` matches the step `align/sort` of a CWL subworkflow. Snakemake rules and Argo templates have names of one part. Names are not patterns.

The setting only makes the cache stricter. A task whose workflow keeps it from the cache, like a CWL tool with `enableReuse: false`, stays so. Cache keys do not change, so the other steps still find the results of earlier runs. The setting does not change what a language plans: Snakemake still skips a job whose outputs are up to date, and `-forcerun` runs it again.

```rust
let mut settings = uwe::run::Settings::new("nf-core/demo");
settings.no_cache_steps = vec!["FASTQC".into()];
let run = uwe::RunRequest::new(settings).run().await?;
```

## Standard output

An embedded run writes nothing to the standard output of the host. What a Nextflow workflow prints, its banner too, goes to standard error. The CWL output object is not printed. Log messages still go through `tracing`.

The returned `Run` holds the CWL output object in `outputs`, as JSON, with the locations of the delivered files. For a Nextflow run it holds what the entry workflow emits, and for a [wir document](wir.md) the outputs of the entry graph. It is `None` for a simulated run and for the other languages. The run database does not keep the object, but it lists each delivered file with its target and its location in the store, `run_database::Database::deliveries(run_id)`, and it keeps the JSON Schema of the outputs.

Set `run::Settings.print` to `true` to write to standard output what `uwe run` writes there. `Settings::new` leaves it `false`. The command line sets it `true`.

## Following a run live

Set `run::Settings.listener` to a `run::Listener`, which is an `Arc<dyn Fn(run::Event) + Send + Sync>`. uwe calls it for every change of the run and its tasks:

- `Event::Started` comes once the run is in the run database. It carries the run id.
- `Event::Task` carries a `TaskEvent`: the task number, name and step, its `TaskState`, the attempt, times, whether the task reused a cached result, the exit code and an error message.
- `Event::Ended` is the last event. Its `error` is set when the run failed or stopped. Without strict provenance, a failure to record the run is only a warning, so `Ended` does not prove that the record is complete.

A task goes from `requested` to `started` to `finished`, `reused` or `failed`. A machine failure gives `retried`, then `started` again. Tasks that did not end when the run ended become `stopped`. The workflow's own output checks can still turn a `finished` attempt into `failed`.

Times are milliseconds since the Unix epoch, as in the run database. All event types are `Clone`, `Debug` and `serde::Serialize`.

uwe calls the listener on its own worker thread, while the engine waits. So the listener must return fast and must not block. Send the event on, for example into a Tokio unbounded channel:

```rust
let (sender, mut receiver) = tokio::sync::mpsc::unbounded_channel();
let mut settings = uwe::run::Settings::new("/srv/workflows/main.cwl");
settings.listener = Some(std::sync::Arc::new(move |event: uwe::run::Event| {
    let _ = sender.send(event);
}));
let running = tokio::spawn(uwe::RunRequest::new(settings).run());
while let Some(event) = receiver.recv().await {
    println!("{}", serde_json::to_string(&event)?);
}
let run = running.await??;
```

The channel closes when uwe drops the settings at the end of the run, so the loop ends then.

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
