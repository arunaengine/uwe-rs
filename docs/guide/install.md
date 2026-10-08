<!-- Building the beta and checking the local container engine. -->
<!-- Copyright (c) 2026 The iunctus Contributors -->
<!-- SPDX-License-Identifier: MIT or Apache-2.0 -->

# Install from source

iunctus is one native Rust binary. It interprets supported Nextflow, Snakemake,
CWL and Argo workflows. Tasks run in containers. You do not need the original
workflow engines, Java or host Python for the examples in this guide.

Start in a checkout of the beta revision you want to use. Install rustup and
native build tools, including a C compiler. The repository's
[toolchain file](../../rust-toolchain.toml) pins Rust **1.97.1**.
[Cargo.lock](../../Cargo.lock) pins Rust dependencies.

```sh
CARGO_INCREMENTAL=0 nice cargo build --locked -p iunctus -j 8
mkdir -p "$HOME/.local/bin"
install -m 755 target/debug/iunctus "$HOME/.local/bin/iunctus"
export PATH="$HOME/.local/bin:$PATH"
iunctus --version
iunctus check-machine
```

The binary prints `iunctus 0.1.0`. The version stays 0.1.0 during the beta;
the changelog lists the changes that are not yet released.
Building may download the toolchain and dependencies.

For local runs, start Docker or enable Podman's API socket:

```sh
systemctl --user enable --now podman.socket
IUNCTUS_ENGINE=podman iunctus check-machine
```

iunctus searches Docker sockets before Podman sockets by default.
`IUNCTUS_ENGINE` can select `docker`, `podman` or an absolute socket path.
Your user must have access to that socket.

`check-machine` reports the engine, store space, run database and executor setup.
An unused remote executor marked `not set up` does not block a local run.
Exit code 0 ends with `This machine is ready for real runs.`
This command checks real-run prerequisites even with `-executor simulator`.
You can run a simple simulation without an engine despite that readiness check.

Use `iunctus run -help` for the current options.
Engine options normally use one hyphen. Workflow parameters use two.
Read the [support boundaries](../support.md) before running a new workflow.

Next: [First run](first-run.md). See also [Troubleshooting](troubleshooting.md).
