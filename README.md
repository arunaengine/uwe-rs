<!-- Build, installation and operating guidance for the iunctus workflow engine. -->
<!-- Copyright (c) 2026 The iunctus Contributors -->
<!-- SPDX-License-Identifier: MIT or Apache-2.0 -->

# iunctus

iunctus is a Rust workflow engine for subsets of Nextflow, Snakemake, CWL and Argo. It plans one shared execution graph, records runs and stores results by content digest. The default executor simulates tasks; real tasks run in containers through local Docker/Podman, TES, SLURM or Kubernetes backends.

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
iunctus check main.nf
iunctus run main.nf                       # simulation with synthetic outputs
iunctus run main.nf -executor local       # real container execution
```

For real local runs, Docker or Podman must expose its engine socket. A workflow task without an image needs `-default-image <image>`. Conda currently parses and locks environments; the Conda-to-image builder is unfinished. Task and package execution stays in containers, including dependencies that the workflow would normally find on its host.

Remote executors need their configuration and shared-path prerequisites. The run page defaults to `127.0.0.1`; choosing another address makes run information available to clients that can reach it. This application follows a trusted local workflow model and runs under the caller's configured filesystem and executor permissions.

See [commands and configuration](docs/README.md), [executor setup](docs/design/executors.md), [support and recovery](docs/support.md) and the [roadmap](docs/roadmap.md). The project license expression is `MIT OR Apache-2.0`; bundled upstream examples retain their own notices.
