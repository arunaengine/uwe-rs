<!-- What uwe is, what it runs, how to install it and where to read more. -->
<!-- Copyright (c) 2026 The uwe Contributors -->
<!-- SPDX-License-Identifier: MIT or Apache-2.0 -->

<img src="docs/uwe.svg" width="96" height="96" align="right" alt="The uwe icon: two task boxes joined into one amber result">

# uwe

uwe (Universal Workflow Executor) runs Nextflow, Snakemake, CWL and Argo workflows from one Rust binary. It reads all four languages itself, plans them into one task graph, runs every task in a container, and stores every result once, by its content digest. A Snakemake rule and a Nextflow process that run the same image on the same inputs share one cache entry.

The machine that runs uwe needs no Python, Groovy, JavaScript or Java. A static Linux build is a single file. The repository is called uwe-rs, the command is `uwe`.

```console
$ uwe run hello.smk
uwe 0.1.0: running hello.smk (Snakemake) on local, run 1a1a1d9fb32-186783
[done] greet (greeting=Hola)
[done] greet (greeting=Ciao)
[done] greet (greeting=Hello)
[done] greet (greeting=Bonjour)
[done] collect
executor >  local (6 tasks, 0 running, 4s)
[3] greet (greeting=Bonjour) | 4 of 4 ✔
[5] collect                  | 1 of 1 ✔
[6] all                      | 1 of 1 ✔

Done in 4s: 6 tasks, 6 ran, 0 from the cache, 0 failed
```

## Why

Bioinformatics has many workflow languages, and each has its own strengths and weaknesses. I kept running into the same problems with them. I was also annoyed that some of the useful features were only available in cloud offerings, or were paid features tied to API keys.

Most engines have limited or uneven support for containers and Kubernetes, or they need a shared working directory. Almost none can use several cloud storage providers at once, for example two different S3 services. Caches belong to one language or even to one workflow, so the same tool with the same inputs runs again just because a different workflow called it. So I started an experiment: can one Rust binary do all of that?

So far it can. A Snakemake rule and a Nextflow process with the same image and inputs share one cache entry. A run moves from a laptop to a cluster or the cloud without changes to the workflow. Every run can be repeated later with the same inputs, images and code. And every task runs in a container, without exceptions, which makes runs reproducible and supports the FAIR principles.

## What it does

### Languages

| Language | Supported |
| --- | --- |
| Nextflow DSL2 | Processes, workflows, channels and operators, configs and profiles, `publishDir`, `storeDir` and `-entry`. Groovy is evaluated in Rust, without a JVM. |
| Snakemake | Rules, wildcards, checkpoints, shadow rules, embedded Python, lifecycle hooks, yte and jinja2 templates, and `cwl:` rules, which run on the CWL runtime of uwe. Python is evaluated in Rust. |
| CWL | CommandLineTools and workflows with scatter, `when`, `valueFrom`, secondary files, and JavaScript expressions through the Boa engine, without Node.js. |
| Argo Workflows | Steps, DAGs, loops, conditions, artifacts, retries, timeouts, exit hooks, mutexes, semaphores, suspend and resume, sidecars and daemon steps. |

Each language is a native subset, not a wrapper around the original engine. `uwe check` tells you before a run whether a workflow uses something uwe does not support.

Workflows can come straight from GitHub, GitLab, Bitbucket, Gitea or any Git server over HTTPS, pinned to a tag, branch or commit with `-r`.

### Executors

- Local **Docker** and **Podman**.
- **SLURM**, with Apptainer, Singularity, Podman or Docker on the nodes.
- **Kubernetes** Jobs.
- **GA4GH TES** servers, such as Funnel.
- A **simulator** that plans a whole workflow and makes placeholder outputs, without running any tool.

Remote nodes can share a file system with uwe, or get their files through an object store, so no shared folder is needed. See [Remote executors and staging](docs/remote-executors.md).

Argo sidecars and daemon steps run on the local executor and on Kubernetes. SLURM and TES refuse them.

Conda environments are solved in Rust (rattler and resolvo) and turned into container images.

### Storage and cache

- A content-addressed store keeps every result once, by its digest.
- One cache for all languages. Interrupted runs resume. As with `-resume` in Nextflow, a resumed run also reuses earlier results for images without a registry digest, such as locally built tags.
- Storage providers for **S3**, **Google Cloud Storage**, **Azure Blob Storage**, **HTTPS**, **FTP** and local folders. You can configure any number of them at once, for example two S3 services side by side.
- A remote store, for example on S3, with an optional local front store for speed.
- `uwe clean` removes data that no run needs anymore. It shows what it would remove before it removes anything.

### Runs and provenance

- Every run is recorded in a local SQLite database. `uwe runs` lists them.
- `uwe changes <run>` shows which workflow files, inputs and images changed since a run. `uwe repeat <run>` runs it again with exactly what it used.
- `uwe why` explains why a task ran. `uwe origin` tells where a result came from.
- `uwe page` opens a local web page with all runs. `uwe run -page` shows the run live while it runs.
- `uwe export` writes an RO-Crate. `uwe time`, `uwe compare` and `uwe report` show where the time went and how two runs differ.

### Checks before a run

- `uwe check <workflow>` finds everything that would stop a real run before any task starts: unsupported features, broken references, missing files and tasks without an image.
- `uwe check-machine` checks the container engine, the store and the executor setup.
- `uwe check-order <workflow>` finds steps whose results depend on which task finished first.
- `uwe inputs` and `uwe outputs` print the inputs and outputs of any workflow as JSON Schema. Every run checks its inputs against that schema.

## Install

You need a Rust toolchain from [rustup](https://rustup.rs) and Docker or Podman. On Linux:

```sh
cargo build --release --locked -p uwe
install -m 755 target/release/uwe "$HOME/.local/bin/uwe"
```

[Installing uwe](docs/install.md) covers macOS, Windows (WSL 2), Podman and the static Linux build.

## First run

From the root of the checkout:

```sh
uwe check-machine
uwe run demo/snakemake/hello.smk
uwe runs
```

The example uses a Debian image and writes greetings to `demo/snakemake/hello/`. The `demo/` folder has examples for every language:

```sh
uwe run demo/nextflow/main.nf
uwe run demo/cwl/hello_world.cwl demo/cwl/hello_world-job.json -default-image debian:stable-slim
uwe run demo/argo/hello-world.yaml
```

Pipelines also run straight from a repository:

```sh
uwe run nf-core/demo -r 1.2.0 -profile test,docker --outdir results
```

`uwe help` lists all commands, and `uwe run -help` lists all run options. uwe options take one hyphen (`-profile`), workflow parameters take two (`--outdir`).

## Documentation

| Page | Read it when |
| --- | --- |
| [Installing uwe](docs/install.md) | You set up uwe on Linux, macOS or Windows |
| [What uwe supports](docs/support.md) | You want to know which versions, executors and providers work |
| [Known pitfalls](docs/pitfalls.md) | A workflow behaves differently than on its original engine |
| [Remote executors and staging](docs/remote-executors.md) | You run on SLURM, Kubernetes or TES |
| [Workflow presets and installed references](docs/presets.md) | You offer fixed workflows and reference data to users |
| [Embedding uwe in a service](docs/embedding.md) | You start runs from your own Rust service |
| [Operating uwe](docs/operations.md) | You back up, restore or upgrade uwe for a team |
| [The demo](demo/README.md) | You want to try every language on a laptop or a cluster |
| [Style guide](STYLE.md) | You change the code |

## Status

uwe is young. It runs real pipelines such as nf-core/demo, locally and on SLURM, but each language is a subset of the original, so check a workflow with `uwe check` before you rely on it. [What uwe supports](docs/support.md) lists what is tested and how. Not every combination of executor, storage provider and container engine has been tested on live systems yet.

## License

uwe is licensed under either the [MIT license](LICENSE-MIT) or the [Apache License 2.0](LICENSE-APACHE), at your option. Bundled upstream examples keep their own notices.

## AI Disclaimer

A lot of this code was generated with the help of generative AI using the latest available models at the time (Opus 4.6-5.5, GPT-5.5 / 5.6-Sol / 6 Astra). The code has been manually audited and checked, but a few quirks still exist. It's still a proof of concept nonetheless and needs some additional work to be fully production ready.
