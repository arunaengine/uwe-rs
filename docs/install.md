<!-- Building and installing uwe on Linux, macOS and Windows, and connecting it to a container engine. -->
<!-- Copyright (c) 2026 The uwe Contributors -->
<!-- SPDX-License-Identifier: MIT or Apache-2.0 -->

# Installing uwe

uwe is built from source. You need a Rust toolchain and a container engine (Docker or Podman). Install Rust with [rustup](https://rustup.rs). The repository pins the Rust version in [rust-toolchain.toml](../rust-toolchain.toml), so rustup fetches the right one on the first build.

## Linux

Install Docker or Podman from your distribution and a C compiler (`build-essential` on Debian and Ubuntu, `base-devel` on Arch). Then build and install from a checkout:

```sh
cargo build --release --locked -p uwe
mkdir -p "$HOME/.local/bin"
install -m 755 target/release/uwe "$HOME/.local/bin/uwe"
```

Add `$HOME/.local/bin` to your `PATH` if it is not there yet.

For a fully static binary that runs on any x86_64 Linux, run `just release-binary`. It needs `musl-gcc` and writes `target/x86_64-unknown-linux-musl/release/uwe`. Remote executors that send uwe to the nodes need this static build.

With Podman, start its socket first: `systemctl --user enable --now podman.socket`. uwe tries Docker first and Podman second. Set `UWE_ENGINE=podman` to always use Podman.

## macOS

Install the Xcode command line tools (`xcode-select --install`) and either Docker Desktop or Podman. Then build as on Linux:

```sh
cargo build --release --locked -p uwe
mkdir -p "$HOME/.local/bin"
install -m 755 target/release/uwe "$HOME/.local/bin/uwe"
```

uwe talks to the engine through its socket. With Docker Desktop, either turn on "Allow the default Docker socket to be used" in the advanced settings, or point uwe at the socket in your home folder:

```sh
export DOCKER_HOST="unix://$HOME/.docker/run/docker.sock"
```

With Podman, start the machine and export its socket:

```sh
podman machine init && podman machine start
export DOCKER_HOST="unix://$(podman machine inspect --format '{{.ConnectionInfo.PodmanSocket.Path}}')"
```

## Windows

Use WSL 2. Install it with `wsl --install` in an administrator PowerShell, open the Ubuntu shell and follow the Linux steps there. For containers, either install Docker Desktop and turn on its WSL 2 integration for your distribution, or install Docker or Podman inside WSL.

Keep the checkout and your data inside the WSL file system, for example under `~/`, not under `/mnt/c`. It is much faster there, and container mounts avoid permission problems.

## Check the setup

```sh
uwe --version
uwe check-machine
```

`uwe check-machine` checks the container engine, the store and the executor setup, and says what is missing.
