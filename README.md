<!-- Build, installation and operating guidance for the iunctus workflow engine. -->
<!-- Copyright (c) 2026 The iunctus Contributors -->
<!-- SPDX-License-Identifier: MIT or Apache-2.0 -->

# iunctus

iunctus is a pure Rust workflow executor for Nextflow, Snakemake, CWL and Argo Workflows. It reads all four languages natively, plans them into one shared execution graph, records every run and stores results by content digest in a central cache that works across languages. It does NOT require Python, Groovy, JavaScript or Java on the executor level and can run as a single static binary!

## Why?

Bioinformatics has many domain-specific workflow languages. Each has its own set of benefits and drawbacks. I was annoyed by the recurring problems I encountered over and over again. I was also annoyed that some of the cool features were hidden behind cloud offerings or were essentially paid features tied to API keys, etc.

Most have limited or inconsistent support for containers, Kubernetes, and so on, or they require a shared working directory. Almost none support multiple cloud storage providers simultaneously, such as several S3 providers at once. Caches are tied to one language or even to a single workflow, so the same tool with the same inputs will run again simply because it was called from a different workflow. So I decided to build this experiment: Can we unify all of that in a single Rust binary?

So far, the answer is a confident yes. A Snakemake rule and a Nextflow process that use the same container with the same inputs share one cache entry. You can move a run from your laptop to a cluster or the cloud without changing the workflow. Every run can also be repeated later with the same inputs, images, and code. And everything is container first, with no compromises. Everything MUST be run in a container, supporting the FAIR principles and making executions more reproducible.

## Features

**Workflow languages**

- **Nextflow** DSL2: processes, workflows, channels and operators, configs and profiles, `publishDir`, `storeDir` and `-entry`. Groovy is evaluated natively in Rust, no JVM needed.
- **Snakemake**: rules, wildcards, checkpoints, shadow rules, embedded Python and lifecycle hooks. Python is evaluated natively in Rust.
- **CWL**: CommandLineTools and workflows with scatter, `when`, `valueFrom`, secondary files and JavaScript expressions (through the Boa engine, no Node.js).
- **Argo Workflows**: steps, DAGs, loops, conditions, artifacts, retries, timeouts, exit hooks, mutexes and semaphores.
- Run workflows straight from GitHub, GitLab, Bitbucket or Gitea, pinned to a tag, branch or commit with `-r`.

**Execution**

- One shared task graph and task runner for every language.
- Executors for local **Docker** and **Podman**, **SLURM** (with Apptainer or Podman), **Kubernetes** and **GA4GH TES**.
- A simulator executor that plans a whole workflow and makes placeholder outputs, without running any tools.
- Conda environments are solved natively (rattler and resolvo) and turned into container images.

**Storage and caching**

- A content-addressed store: every result is kept once, by digest.
- One cache across all languages, with resume for interrupted runs.
- Storage providers for **S3**, **Google Cloud Storage**, **Azure Blob Storage**, **HTTPS**, **FTP** and local folders. Configure as many as you like at the same time, for example several S3 providers side by side.
- A remote store (for example on S3) with an optional local front store for speed.
- `iunctus clean` removes data no run still needs, and shows you what it would remove first.

**Runs and reproducibility**

- Every run is recorded in a local SQLite database: `iunctus runs` lists them.
- `iunctus changes <run>` shows which workflow files, inputs and images changed since a run, and `iunctus repeat <run>` runs it again with exactly what it used.
- `iunctus why` and `iunctus origin` explain why a task ran or where a result came from.
- A local web page for runs with `iunctus page`, or live during a run with `iunctus run -page`.
- RO-Crate export with `iunctus export`, plus timing, comparison and report commands.

**Checks**

- `iunctus check <workflow>` finds everything that would stop a real run (unsupported features, broken references, missing files, tasks without an image) before any task starts.
- `iunctus check-machine` checks the container engine, the store and the executor setup.
- `iunctus check-order <workflow>` finds steps whose results depend on which task finished first.
- `iunctus inputs` and `iunctus outputs` print the inputs and outputs of any workflow as JSON Schema, and every run checks its inputs against it.

## Getting started

You need a container engine (Docker or Podman) and a Rust toolchain to build iunctus. Install Rust through [rustup](https://rustup.rs). [rust-toolchain.toml](rust-toolchain.toml) pins the Rust version, so rustup downloads the right one on the first build.

### Linux

Install Docker or Podman from your distribution, plus a C compiler (for example `build-essential` on Debian and Ubuntu, `base-devel` on Arch). Then build and install from a checkout of this repository:

```sh
cargo build --release --locked -p iunctus
mkdir -p "$HOME/.local/bin"
install -m 755 target/release/iunctus "$HOME/.local/bin/iunctus"
```

Add `$HOME/.local/bin` to your `PATH` if needed. For a fully static binary that runs on any x86_64 Linux, use `just release-binary` (needs `musl-gcc`).

If you use Podman, start its socket with `systemctl --user enable --now podman.socket`. iunctus looks for Docker first and Podman second; set `IUNCTUS_ENGINE=podman` to pick Podman.

### macOS

Install the Xcode command line tools (`xcode-select --install`) and either Docker Desktop or Podman. Then build from a checkout, like on Linux:

```sh
cargo build --release --locked -p iunctus
mkdir -p "$HOME/.local/bin"
install -m 755 target/release/iunctus "$HOME/.local/bin/iunctus"
```

iunctus talks to the engine through its socket. With Docker Desktop, either enable "Allow the default Docker socket to be used" in the advanced settings, or point iunctus at the socket in your home folder:

```sh
export DOCKER_HOST="unix://$HOME/.docker/run/docker.sock"
```

With Podman, start the machine and export its socket:

```sh
podman machine init && podman machine start
export DOCKER_HOST="unix://$(podman machine inspect --format '{{.ConnectionInfo.PodmanSocket.Path}}')"
```

### Windows

Use WSL 2. Install it with `wsl --install` in an administrator PowerShell, open the Ubuntu shell and follow the Linux steps from there. For containers, either install Docker Desktop and enable the WSL 2 integration for your distribution, or install Docker or Podman directly inside WSL.

Keep the checkout and your data inside the WSL file system (for example under `~/`), not under `/mnt/c`. It is much faster and avoids permission problems with container mounts.

### First run

```sh
iunctus --version
iunctus check-machine
iunctus run demo/snakemake/hello.smk
iunctus runs
```

Run these from the checkout. The example uses a Debian image and writes greetings under `demo/snakemake/hello/`. The `demo/` folder has more examples for every language; try `iunctus run demo/nextflow/main.nf`, `iunctus run demo/cwl/hello_world.cwl demo/cwl/hello_world-job.json -default-image debian:stable-slim` or `iunctus run demo/argo/hello-world.yaml`.

You can also run a pipeline straight from a repository:

```sh
iunctus run nf-core/demo -r 1.2.0 -profile test,docker --outdir results
```

`iunctus help` lists all commands and `iunctus run -help` lists all run options.

## Known pitfalls

- **Containers are required.** Every task runs in a container. Host-installed tools are never used as a fallback, so declare an image in the workflow or pass `-default-image IMAGE`.
- **Languages are native subsets.** iunctus implements the languages in Rust instead of calling the original engines. A workflow that runs on Nextflow or Snakemake can still use a feature iunctus does not support yet. Run `iunctus check <workflow>` first, unsupported features give exit code 33 with a clear message.
- **Flags differ from the original engines.** iunctus options use one hyphen (`-executor`, `-profile`), workflow parameters use two (`--outdir`). Many Nextflow and Snakemake flags are not accepted. Nextflow `-c FILE` configures the workflow, not iunctus, use `-iunctus-config FILE` for that.
- **Nextflow work and resume layouts differ.** iunctus has its own store and resume, so tools that read Nextflow `work/` folders or `.nextflow` logs will not find them.
- **Python packages in Snakemake.** Embedded Python runs natively, but third-party packages run in container helpers, and arbitrary Python objects cannot cross that boundary.
- **Conda and pip.** Conda environments become container images for local runs and are published for remote executors. Editable, local or VCS `pip:` requirements are not supported.
- **CWL JavaScript needs a helper image.** Build it once from your binary with `python3 scripts/cwl-js-image.py target/release/iunctus --engine docker` (or `--engine podman`).
- **Repository sources.** SSH URLs, arbitrary Git servers, submodules and archive links are not supported. Cached branches stay on their remembered commit until you pass `-latest`.
- **Remote executors need shared paths.** SLURM and Kubernetes nodes must see the store and scratch folders at the same paths. TES can use an S3 bucket instead.
- **Disk use grows.** The store, cache and engine images grow over time. Published results can link to data in the store, so preview with `iunctus clean` before removing anything with `-remove`.

## License

The project license is `MIT OR Apache-2.0`. Bundled upstream examples keep their own notices.

## AI Disclaimer

A lot of this code was generated with the help of generative AI using the latest available models at the time (Opus 4.6-5.5, GPT-5.5 / 5.6-Sol / 6 Astra). The code has been manually audited and checked, but a few quirks still exist. It's still a proof of concept nonetheless and needs some additional work to be fully production ready.
