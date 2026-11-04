<!-- How to build, test and check a change to uwe before sending it. -->
<!-- Copyright (c) 2026 The uwe Contributors -->
<!-- SPDX-License-Identifier: MIT or Apache-2.0 -->

# Contributing

Thanks for helping with uwe. This page lists what a change needs before it is sent. The rules for names, folders, comments and file headers are in [STYLE.md](STYLE.md), which is the canonical policy.

## Set up

Install Rust with [rustup](https://rustup.rs), Docker or Podman, Python 3, and [just](https://github.com/casey/just). The pinned toolchain in [rust-toolchain.toml](rust-toolchain.toml) is fetched on the first build. Formatting and Clippy use the pinned nightly named in the [justfile](justfile):

```sh
rustup toolchain install nightly-2026-11-03 --component rustfmt,clippy
```

## Check a change

```sh
just style   # names, folders, comments and headers from STYLE.md, without a build
just lint    # formatting and Clippy, warnings are errors
just test    # all workspace tests and doctests
```

`just test` first builds uwe and its CWL JavaScript helper image, which the CWL tests need. Building the image needs Docker or Podman. Tests that need outside services are ignored by default. The recipes that run them, like `just s3-test`, `just tes-test` and `just kind-staging-test`, are described in the justfile.

Run demos from a copy outside the checkout, so their outputs do not end up in the repository.

## Commits

- Keep each commit small and about one thing.
- Use one subject line that starts with `feat:`, `fix:`, `chore:`, `refactor:` or `test:`.
- Add a regression test with each fix.
- Update the README or the pages under [docs/](docs/) when behavior that users see changes.
