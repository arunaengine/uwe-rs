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

## JSON key order

The JSON objects of uwe keep their keys in the order in which they were added, also in `Run.outputs`, the run database and the files that plugins write. An object read from a file keeps the order of that file. Where a format needs sorted keys, like a digest or the file of a tool that sorts its keys, uwe sorts them itself. Help texts list the inputs of a group by name.

This is the `preserve_order` feature of serde_json. The WDL front end needs it, and Cargo turns a feature on for every crate of a build, so the workspace declares it for all crates. A host that builds uwe into its program gets the same order for its own `serde_json` values.

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

## Secrets of a run

A config file names its secrets as `{ environment = "NAME" }`. On the command line uwe reads them from the environment of the process. A host that runs several workflows at once can give each run its own values instead, in `run::Settings.secrets`, a `run::SecretValues`. So the host does not need to change its environment while runs read it, and two runs can use different values for the same name.

`SecretValues::insert(name, value)` gives a value for the environment variable `name`. It refuses an empty name, a name with `=` or a NUL byte, and an empty value or one with a NUL byte. The error names the secret, never its value.

uwe takes a given value first, wherever a run reads one of these names:

- the `{ environment = "NAME" }` secrets of the config files that the run reads, like storage keys, the TES token, user and password, registry credentials, telemetry headers, the page token and the `[secrets]` section,
- `UWE_TES_TOKEN`,
- the repository tokens `NXF_GITHUB_ACCESS_TOKEN`, `GITHUB_TOKEN`, `GITLAB_TOKEN`, `BITBUCKET_TOKEN` and `GITEA_TOKEN`,
- what a Nextflow workflow reads from its environment: `env('NAME')`, `System.getenv('NAME')`, names of the environment in config files and scripts, and the tokens and settings that plugins read, like `TOWER_ACCESS_TOKEN` of nf-tower, `OPENAI_API_KEY` of nf-gpt, `ANTHROPIC_API_KEY` of nf-runinsights, the `LAMIN_` variables of nf-lamin, the `BIFFIX_` variables of nf-biffix, the variables of nf-llm-debugger and nf-fl86, `NXF_CLOUDCACHE_PATH`, and the variables that the BCO report of nf-prov records,
- what a Snakemake workflow reads from its environment: `os.environ` and the `envvars` of the workflow and its profile, also when a job gets their values as it starts.

A name without a given value is read from the environment, as on the command line. Call `allow_environment(false)` to stop that. Then such a secret fails the run with "the host gave no value for the secret NAME", and a token without a value is not sent. A workflow or plugin sees such a name as not set: `env('NAME')` gives `null`, nf-gpt stops with "Missing ChatGPT API key", and Snakemake stops on an `envvars` name that is not set.

The workflow can read every value that the host gives its run, and can write it to its outputs. So give a run only the values that its workflow may see.

```rust
let mut settings = uwe::run::Settings::new("/srv/workflows/main.cwl");
settings.config_file = Some("/srv/runs/01JBXQ5Z7C2W9M/uwe.toml".into());
settings.secrets.insert("LAB_TES_TOKEN", token)?;
settings.secrets.insert("LAB_S3_SECRET", secret_key)?;
settings.secrets.allow_environment(false);
let run = uwe::RunRequest::new(settings).run().await?;
```

The same settings work for `RunRequest::describe` and for a run on the simulator. uwe writes the given values to neither the run database, the recorded command line nor the RO-Crate. `Debug` of `SecretValues` shows only the names.

A host that passes its own `setup` made the secrets in it itself, so those do not take the given values. `UWE_TES_TOKEN`, the repository tokens and what workflows read still do.

These still come from the environment of the process, for every run alike:

- the default sources of storage credentials, like the `AWS_` variables and files and `IRODS_ENVIRONMENT_FILE`, sign-in to AWS CodeCommit repositories, and the credentials of cloud metadata services,
- `UWE_ENGINE` and the other settings of uwe itself,
- the user and home of the process: `HOME` and `USER`, as `workflow.homeDir`, `workflow.userName` and `System.getProperty` give them and as plugins like nf-quilt, nf-teams, nf-fairscape and nf-report write them, and `NXF_ASSETS` of nf-lamin,
- where the Nextflow secret store is: `NXF_HOME`, `HOME` and `NXF_SECRETS_FILE`,
- the terminal: `TERM` and `NXF_AGENT_MODE`, which nf-seqlab-progress reads.

## Tasks of a run whose process is gone

A TES run keeps a journal for each attempt in the scratch folder of the cache, `scratch/tes/<run>-<task>-<attempt>`. When the uwe process of the run stops, for example because the host restarts, its tasks can still run on the TES server. The next TES run on the same cache stops them. A host can also do it at once for one run, with the settings that the run used: the same `run_id`, `config_file`, `cache_folder`, `store` and `secrets`, or the same `setup`.

- `RunRequest::remote_tasks` reports the tasks of the run with what their servers say now. It changes nothing.
- `RunRequest::stop_tasks` cancels each task that has not ended, and waits until the server confirms that it ended. Then it removes the journal of the attempt, and the staged objects as `staging_cleanup` says. It gives up on one task after a minute, like the recovery of a run, and goes on with the next one.

Both give one `run::orphans::Orphan` for each attempt: its name, the id of its TES task, the state that the server last gave and an `Outcome`. The outcome is `Running` or `Ended` for a task, and `Canceled` for a task that `stop_tasks` canceled. `Unclear` says why the state is not known, `Kept` says why `stop_tasks` left a task as it was, and `Failed` holds the error of a stop that failed. A task that was kept or failed keeps its journal, so a later call tries again. A call for a run without journals gives an empty list and asks no server.

```rust
let mut settings = uwe::run::Settings::new("");
settings.run_id = Some("01JBXQ5Z7C2W9M".into());
settings.config_file = Some("/srv/runs/01JBXQ5Z7C2W9M/uwe.toml".into());
settings.cache_folder = Some("/srv/cache".into());
settings.secrets.insert("LAB_TES_TOKEN", token)?;
settings.secrets.allow_environment(false);
for task in uwe::RunRequest::new(settings).stop_tasks().await? {
    println!("{}: {:?}", task.attempt, task.outcome);
}
```

These calls touch only the tasks of this run:

- A journal belongs to the run only when it names the run. Journals from before uwe named the run in them are only reported, as `Unclear` or `Kept`, since the folder name of run `a` also fits a task of run `a-b`.
- A task is stopped only with a backend whose server and token are the ones that started it, as in the recovery of a run. A task of another server or token is kept.
- An attempt whose uwe process still holds the lock of its journal is kept.
- A second call finds no journal of the stopped tasks, so it does nothing.

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
