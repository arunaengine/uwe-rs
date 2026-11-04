<!-- What uwe is, what it runs, how to install it and where to read more. -->
<!-- Copyright (c) 2026 The uwe Contributors -->
<!-- SPDX-License-Identifier: MIT or Apache-2.0 -->

<p align="center">
  <img src="docs/uwe.svg" width="128" height="128" alt="The uwe icon: two task boxes joined into one amber result">
</p>

<h1 align="center">A <ins>U</ins>niversal <ins>W</ins>orkflow <ins>E</ins>xecutor</h1>

uwe (Universal Workflow Executor) runs [Nextflow](https://www.nextflow.io), [Snakemake](https://snakemake.github.io), [CWL](https://www.commonwl.org) and [Argo](https://argoproj.github.io/workflows/) workflows from a single Rust binary. It reads all four languages itself, plans them into one task graph, runs every task in a container, and stores every result once, by its content digest. A Snakemake rule and a Nextflow process that run the same command in the same image on the same inputs, without network access, share one cache entry.

The machine that runs uwe needs no Python, Groovy, JavaScript or Java. A static Linux build is a single file.

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

Bioinformatics and science in general has many workflow languages, and each has its own strengths and weaknesses. I kept running into the same problems with them and was always annoyed by their lack of compatibility. I was also annoyed that some of the useful features were only available in cloud offerings, or were paid features tied to API keys.

Most engines have limited or uneven support for containers and Kubernetes, or they need a shared working directory. Almost none can use several cloud storage providers at once, for example two different S3 services. Caches belong to one language or even to one workflow, so the same tool with the same inputs runs again just because a different workflow called it. So I started an experiment: can one Rust binary do all of that?

So far it looks like it can. A Snakemake rule and a Nextflow process that run the same command in the same image on the same inputs share one cache entry. A run moves from a laptop to a cluster or the cloud without changes to the workflow. Every run can be repeated later with its recorded inputs, images and code. And every task runs in a container, without exceptions, which makes runs reproducible and supports the FAIR principles.

## What it does

### Languages

| Language | Supported |
| --- | --- |
| [Nextflow](https://www.nextflow.io) DSL2 | Processes, workflows, channels and operators, configs and profiles, `publishDir`, `storeDir` and `-entry`. Groovy is evaluated in Rust, without a JVM. |
| [Snakemake](https://snakemake.github.io) | Rules, wildcards, checkpoints, shadow rules, embedded Python, lifecycle hooks, yte and jinja2 templates, and `cwl:` rules, which use the CWL runtime of uwe. Python is evaluated in Rust. |
| [CWL](https://www.commonwl.org) | CommandLineTools and workflows with scatter, `when`, `valueFrom`, secondary files, and JavaScript expressions through the Boa engine, without Node.js. |
| [Argo Workflows](https://argoproj.github.io/workflows/) | Steps, DAGs, loops, conditions, artifacts, retries, timeouts, exit hooks, mutexes, semaphores, suspend and resume, sidecars and daemon steps. |

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
- One cache for all languages. A task without network access (`-no-network`) whose image has a registry digest reuses results from any earlier run, of any workflow. uwe looks up the digest of an image tag itself. Other tasks reuse results only with `-resume`, as in Nextflow, which also covers images without a registry digest, such as locally built tags.
- Storage providers for **S3**, **Google Cloud Storage**, **Azure Blob Storage**, **HTTPS**, **FTP** and local folders. You can configure any number of them at once, for example two S3 services side by side.
- A remote store, for example on S3, with an optional local front store for speed.
- `uwe clean` removes data that no run needs anymore. It shows what it would remove before it removes anything.

### Runs and provenance

- Every run is recorded in a local SQLite database. `uwe runs` lists them.
- `uwe changes <run>` shows which workflow files, inputs and images changed since a run. `uwe repeat <run>` runs it again with its kept workflow files and inputs, and with the recorded image digests where it has them.
- `uwe why` explains why a task ran. `uwe origin` tells where a result came from.
- `uwe page` opens a local web page with all runs. `uwe run -page` shows the run live while it runs.
- `uwe export` writes an RO-Crate. `uwe time`, `uwe compare` and `uwe report` show where the time went and how two runs differ.

### Checks before a run

- `uwe check <workflow>` finds problems before any task starts: unsupported features, broken references, missing files and tasks without an image. It plans with made-up data, so it cannot see tasks that only real data creates, and it does not fetch remote inputs.
- `uwe check-machine` checks the container engine, the store and the executor setup.
- `uwe check-order <workflow>` finds steps whose results depend on which task finished first.
- `uwe inputs` and `uwe outputs` print the inputs and outputs of any workflow as JSON Schema. Every run checks its inputs against that schema.

## Install

You need a Rust toolchain from [rustup](https://rustup.rs) and Docker or Podman. On Linux:

```sh
cargo build --release --locked -p uwe
mkdir -p "$HOME/.local/bin"
install -m 755 target/release/uwe "$HOME/.local/bin/uwe"
```

Add `$HOME/.local/bin` to your `PATH` if it is not there yet.

[Installing uwe](docs/install.md) covers macOS, Windows (WSL 2), Podman and the static Linux build.

## First run

From the root of the checkout:

```sh
uwe check-machine
uwe run demo/snakemake/hello.smk
uwe runs
```

The example uses a Debian image and writes greetings to `demo/snakemake/hello/`. `uwe help` lists all commands, and `uwe run -help` lists all run options.

## Running workflows

`uwe run` takes a workflow file, a folder or a repository. It tells the language from the file, or from `-lang`. uwe options take one hyphen (`-profile`). Workflow parameters take two (`--outdir`).

### Nextflow

```sh
uwe run demo/nextflow/main.nf
uwe run demo/nextflow/main.nf -resume
uwe run nf-core/demo -r 1.2.0 -profile test,docker --outdir results
```

The second run takes all results from the cache. Parameters follow the workflow, like `--outdir results` or `--input samples.csv`. `-profile` picks profiles of `nextflow.config`, `-entry` picks a named workflow, and `-c FILE` adds a Nextflow config file.

### Snakemake

```sh
uwe run demo/snakemake/hello.smk
uwe run demo/snakemake/hello.smk hello/Hola.txt
uwe run demo/snakemake/hello.smk -forcerun collect
```

Targets, files or rules, follow the Snakefile. `--config name=value` sets config values. `-until`, `-omit-from`, `-forcerun` and `-forceall` work as in Snakemake. Every rule needs a `container:`, or a `-default-image` for those without one.

### CWL

```sh
uwe run demo/cwl/hello_world.cwl demo/cwl/hello_world-job.json -default-image debian:stable-slim
uwe run demo/cwl/hello_world.cwl --message "Hola mundo" -default-image debian:stable-slim
uwe run demo/cwl/scatter-workflow.cwl demo/cwl/scatter-job.yml -default-image debian:stable-slim
```

The job file, JSON or YAML, follows the CWL document. Inputs can also be given as `--name value`. A tool without a `DockerRequirement` runs in the `-default-image`. Outputs go to the current folder, or to `-output-dir FOLDER`.

### Argo Workflows

```sh
uwe run demo/argo/hello-world.yaml
uwe run demo/argo/dag-diamond.yaml
```

Workflow parameters are given as `--name value`, like `--message "hola mundo"`.

### Where tasks run

Tasks run in local Docker or Podman containers by default. Other executors take the same workflows:

```sh
uwe run main.nf -executor slurm
uwe run main.nf -executor kubernetes -kubeconfig ~/.kube/config
uwe run main.nf -executor tes -tes-server https://tes.example.org
uwe run main.nf -executor simulator
```

The simulator plans the whole run and makes placeholder outputs, without running a tool. [Remote executors and staging](docs/remote-executors.md) explains how SLURM, Kubernetes and TES jobs get their files.

### Before and after a run

```sh
uwe check demo/nextflow/main.nf
uwe inputs demo/cwl/hello_world.cwl -format text
uwe runs
uwe page
```

`uwe check` finds problems before any task starts. `uwe inputs` lists the inputs of a workflow. `uwe runs` lists recorded runs, and `uwe page` shows them in the browser.

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
| [Contributing](CONTRIBUTING.md) | You change the code |

## Status

uwe is still an experiment. It runs real pipelines such as some nf-core pipelines, locally and on SLURM, but each language is a subset of the original, so check a workflow with `uwe check` before you rely on it. [What uwe supports](docs/support.md) lists what is tested and how. Not every combination of executor, storage provider and container engine has been tested on live systems yet.

## License

uwe is licensed under either the [MIT license](LICENSE-MIT) or the [Apache License 2.0](LICENSE-APACHE), at your option. Bundled upstream examples keep their own notices.

## AI Disclaimer

A lot of this code was generated with the help of generative AI using the latest available models at the time (Opus 4.6-5.5, GPT-5.5 / 5.6-Sol / 6 Astra). The code has been manually audited and checked, but a few quirks still exist. It's still a proof of concept nonetheless and needs some additional work to be fully production ready.
