<!-- Build, installation and operating guidance for the iunctus workflow engine. -->
<!-- Copyright (c) 2026 The iunctus Contributors -->
<!-- SPDX-License-Identifier: MIT or Apache-2.0 -->

# iunctus

iunctus is a Rust workflow engine for subsets of Nextflow, Snakemake, CWL and Argo. It plans one shared execution graph, records runs and stores results by content digest. The default executor runs tasks in local Docker/Podman containers. TES, SLURM and Kubernetes backends are also available; `-executor simulator` explicitly selects synthetic outputs without task containers.

Aruna integration uses the generic TES executor with object-store staging through S3 (`staging = "object-store"`); it was checked against Funnel and MinIO, see [executors](docs/guide/executors.md).

This is a development release. Implemented features and passing local checks do not establish production readiness or full language parity. Read the [support matrix and operator notes](docs/support.md) before selecting a backend.

## Build and install from source

Start in a checkout of the revision you intend to use. Install Rust through your usual toolchain setup and provide native build tools, including a C compiler. [rust-toolchain.toml](rust-toolchain.toml) pins Rust 1.97.1; [Cargo.lock](Cargo.lock) pins dependencies.

```sh
cargo build --release --locked -p iunctus -j 2
mkdir -p "$HOME/.local/bin"
install -m 755 target/release/iunctus "$HOME/.local/bin/iunctus"
"$HOME/.local/bin/iunctus" help
"$HOME/.local/bin/iunctus" check-machine
```

Add `$HOME/.local/bin` to your `PATH` if needed. Record the source commit and the binary's SHA-256 digest with your installation. Compilation may download the pinned toolchain and dependencies. There is no published binary qualification claimed here.

## First run

```sh
iunctus --version
iunctus check-machine
iunctus run demo/snakemake/hello.smk
iunctus runs
```

Run these commands from the checkout after installing the binary. Docker or Podman
must expose its engine socket. The example uses a Debian image and writes greetings
under `demo/snakemake/hello/`. Use `-executor simulator` in a separate copy for
made-up outputs without task containers.

Follow the [user guide](docs/guide/first-run.md) for a local Nextflow example,
repository runs, resume and repeat. See [installation](docs/guide/install.md),
[executor setup](docs/guide/executors.md) and the
[configuration reference](docs/guide/configuration.md).

See [commands and configuration](docs/README.md), [executor setup](docs/design/executors.md), [support and recovery](docs/support.md) and the [roadmap](docs/roadmap.md). The project license expression is `MIT OR Apache-2.0`; bundled upstream examples retain their own notices.
