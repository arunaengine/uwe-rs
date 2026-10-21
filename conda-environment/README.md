<!-- Source contracts retained while shortening source comments and introductions. -->
<!-- Copyright (c) 2026 The uwe Contributors -->
<!-- SPDX-License-Identifier: MIT or Apache-2.0 -->

# Source contracts

## src/lock.rs

The lock of an environment: the packages that it installs for a platform, each with its
channel, file, address and digest, so that the same environment is built again anywhere.
Its digest names the environment, as decision 12 asks: an image built from it is reused by
the lock, the base image, the platform and the recipe.

## tests/test_workflows.rs

Reads every conda environment file of the Snakemake test workflows, the files in their
`envs` folders, and checks their accepted and rejected outcomes. Run it with
`cargo test -p conda-environment --test test_workflows -- --ignored --nocapture`; the
folder is `SNAKEMAKE_TEST_WORKFLOWS`, else `~/.cache/uwe/test-workflows/snakemake`.

## src/image.rs:187

Writes a deterministic tar from the SHA-256 package cache of `crate::fetch_packages`.
Package bytes are checked while copied. Output may be partial on any error: consumers
must stage the context and submit it to the container API only after this succeeds.
This method never invokes an installer or builds an image on the driver host.

## src/lib.rs:105

Locks an environment for a platform with the repodata in `folder`.
No virtual packages are inferred. Dependencies on target capabilities cannot be satisfied
unless they are supplied through `lock_with_virtual`. Target compatibility remains unknown.
Use `lock_with_virtual` to check authoritative target capabilities instead.

