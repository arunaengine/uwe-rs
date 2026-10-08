<!-- Native Conda and pip locking, container builds and image reuse. -->
<!-- Copyright (c) 2026 The iunctus Contributors -->
<!-- SPDX-License-Identifier: MIT or Apache-2.0 -->

# Conda environments

Decision 12 says that a Conda environment is locked for the platform of the task and then built
into a container image. No Conda installation on the host takes part. This page says what
iunctus does today: offline locking, cached HTTPS acquisition, container builds and image reuse.

The code is in `conda-environment` and `executor`. `iunctus lock-environment` writes a lock;
the local workflow executor can also prepare and reuse images for Snakemake Conda rules.

## What works without network

- **Environment files.** iunctus reads the environment files of Snakemake rules and nf-core
  modules: `name`, `channels`, `dependencies` and the `pip:` list. Other keys, like `prefix`,
  are ignored. An error names the line of the file.
- **Match specs.** Both forms are read. Environment files write `samtools=1.17` (versions that
  start with 1.17), `samtools==1.17` (exactly 1.17), `bioconda::fastqc=0.12.1=hdfd78af_0` (a
  channel, a version and a build) and ranges like `python>=3.8,<3.9`. The dependencies of
  packages write `python >=3.8,<3.9.0a0`, `libzlib 1.2.13` (exactly this version) and
  `python_abi 3.8.* *_cp38` (a pattern of the build). Spaces around operators count for
  nothing, as in Conda. Bracket specs, including `numpy[version='>=1.20',build=py*]`, are
  parsed by `rattler_conda_types`. Channel, subdirectory and filename constraints are kept.
- **The order of versions.** Versions compare as conda's `VersionOrder` does: an epoch
  (`1!2.0`), components split at `.` and `_`, numbers as numbers, `dev` before any other text,
  `post` after any number, and a local part after `+`. So `1.1a1 < 1.1 = 1.1.0 < 1.1.0post1`.
- **The lock.** `rattler_solve` uses resolvo to choose compatible packages from channel
  metadata. The lock is JSON: the platform, channels, match specs, target virtual packages
  when supplied, and each package with its channel, file, address, SHA-256 and MD5. Its digest
  names the environment. Pip environments add a versioned `pypi` field with exact artifact
  versions, filenames, HTTPS URLs and SHA-256 hashes, including transitive dependencies.
  Conda-only locks omit this field and retain their existing bytes and digest.
- **The explicit list.** A lock can be written as conda's explicit list: `@EXPLICIT`, then the
  address of each package with its MD5. Conda and micromamba install such a list without
  solving again.

The local Snakemake corpus has 2053 environment files. With rattler parsing, 2037 read and
16 are rejected. Locking input acceptance increases from 1911 Conda-only environments to
2010 environments, including 99 supported pip environments. Another 27 pip environments use
unsupported requirement forms. This inventory checks inputs, not successful resolution or
container builds across the corpus. The remaining parser
rejections are malformed YAML, templates, unsupported extras, and namespace syntax such as
`bioconda:bismark`.

## The folder of repodata

The resolver reads the repodata of each channel and platform from the cache folder:

```
<cache>/conda/<channel>/<platform>/repodata.json
<cache>/conda/<channel>/noarch/repodata.json
```

`<cache>` is `$XDG_CACHE_HOME/iunctus`, else `~/.cache/iunctus`. Each file is the one that the
channel serves, like `https://conda.anaconda.org/conda-forge/linux-64/repodata.json`, or
`https://repo.anaconda.com/pkgs/main/linux-64/repodata.json` for Anaconda's own channels.

By default, or with `[conda] offline = false`, `rattler_repodata_gateway` fetches requested
package names and their dependency closure over HTTPS. It supports sharded and zstd repodata with fallback
to ordinary repodata, and keeps metadata under `<repodata>/gateway/`. Cloned acquisition
handles share a Conda HTTP client and serialize requests. Package downloads also use this
client, verify SHA-256 before atomic publication, and reuse verified cached bytes. Connect
and request deadlines are 30 and 300 seconds; an individual package is limited to 4 GiB.

With `offline = true`, prepared JSON files remain supported. If a prepared file
is missing, configured workflows and lock commands can use previously cached gateway
metadata, including stale metadata, without downloading. Packages must already be cached
for a cold build. The unconfigured lock command uses cached HTTPS acquisition unless the
network configuration is offline. Offline locking reports missing JSON files with their
addresses and cache paths.

Channel names work as in conda: `defaults` stands for `pkgs/main` and `pkgs/r`, and
`nodefaults` for none. An environment file without channels gets `conda-forge`, as Miniforge
has it. HTTPS channel URLs are supported; their local JSON folder uses the SHA-256 of the
normalized channel URL. Credentials, query strings and fragments in channel URLs are
unsupported. A match spec may name a subdirectory, like `conda-forge/linux-64::zlib`.

## The resolver

`rattler_solve` uses the resolvo SAT solver with flexible channel priority. Higher-priority
channels are preferred, with lower-priority candidates available when required for a valid
solution. Dependencies, optional constraints, versions and builds use rattler types. There
is no 200000-attempt search limit. A small variant-flag adapter preserves channel,
subdirectory and filename qualifiers that the upstream matcher otherwise omits in package
constraints. It returns the original records after solving.

Virtual packages describe the container target. `lock_with_virtual` and configured
preparation use explicit target capabilities; an omitted package is absent. The unconfigured
lock command has no virtual packages and cannot satisfy dependencies on them. It keeps
unknown target compatibility in its lock metadata. Host detection from
`rattler_virtual_packages` is deliberately not used to infer container capabilities.

A spec with no candidate is reported as missing; incompatible requirements produce a
conflict. A valid resolvo solution can differ from a previous greedy solution. The exact
package URLs and digests in the resulting lock define the image contents.

## Pip dependencies

Named [PEP 508 requirements](https://packaging.python.org/en/latest/specifications/dependency-specifiers/)
support version constraints, extras and target markers. `pep440_rs` and `pep508_rs` parse
versions and requirements. A bounded native backtracking resolver follows the complete
dependency closure from [PyPI's JSON Simple API](https://docs.pypi.org/api/index-api/).
It fails explicitly after 10000 attempts or 256 projects. The Conda solve adds `pip` when
needed and locks Python before pip resolution. No host Python, pip or uv program runs.

The resolver uses the exact locked standard CPython 3 version and Linux target architecture.
It accepts compatible pure Python, CPython and stable ABI wheels. Manylinux and musllinux
tags require suitable `__glibc` or `__musl` capabilities from the target, never the host.
Wheels are preferred for a release. Prereleases need an explicit prerelease constraint;
yanked artifacts are excluded. Unsupported wheel tags fail when no compatible artifact exists.

Dependency metadata comes from the selected artifact. Hash-verified metadata sidecars are
used when available; otherwise native ZIP reading extracts metadata from the verified wheel.
Source archives are read natively without running their code. Static metadata must have
Metadata-Version 2.2 or later, with no dynamic dependency, Python, extra, name or version fields.
Source builds require locked Conda `setuptools` and `wheel`; declared build-system requirements
must also be satisfied by locked Conda packages. Any compiler needed by the package must be
in that environment. Dynamic source metadata and local build backend paths are refused.

The native path keeps the host's one-binary boundary and a small dependency addition.
[uv's resolver](https://github.com/astral-sh/uv/blob/main/crates/uv-resolver/Cargo.toml)
is an internal crate with a large uv workspace dependency graph.
[RIP's resolver API](https://docs.rs/rattler_installs_packages/0.10.0/rattler_installs_packages/resolve/fn.resolve.html)
requires a wheel builder that can create Python environments and execute build code.
Neither host builder is needed here: source installation happens only in the image build.

Pip metadata is cached at `<repodata>/pypi/<SHA-256 of URL>`. Online locking refreshes project
indexes and reuses immutable artifact metadata; offline locking uses only cached metadata.
Individual locking responses are limited to 64 MiB, extracted metadata to 8 MiB, and source
archive reads to 256 MiB. Downloads are serial with the same 30/300 second connection/request
deadlines as Conda acquisition. Cold-build artifact bytes share the existing SHA-256 package
cache and are checked again while the tar context is written.

Editable installs, local paths and archives, direct URLs, VCS requirements, index options,
extra indexes and kernel release/version markers are refused with exit code 33. Requirements
are validated before metadata access, including requirements whose markers are inactive.

## `iunctus lock-environment`

```
iunctus lock-environment <conda environment file> [-platform <name>] [-output <file>]
    [-iunctus-config <file>]
```

- `-platform` is the conda platform, like `linux-64` or `linux-aarch64`. The default is Linux
  on the processor of this machine when no Conda target is configured. With a `[conda]`
  configuration, the platform comes from that target and an explicit override must match it.
- Without `-output`, the lock goes to standard output as JSON. With `-output`, it goes to that
  file; a name that ends in `.txt` gets the explicit list. The command then prints the number
  of packages and the digest of the environment.
- A configured lock uses the exact target capabilities and acquisition policy below. Without
  configuration it uses cached HTTPS acquisition, or local JSON when offline, and infers
  no virtual packages.
- Pip locks require JSON output. A `.txt` explicit list cannot preserve pip pins and is
  refused with exit code 33 for an environment containing pip requirements.

## Building and reusing images for local workflows

The local executor follows decision 12 and decision 26 (one binary, no host installers):

- The build goes through the Docker Engine API (bollard), which Podman serves too. iunctus
  sends a build context as a tar stream: the explicit list and a short recipe that starts from
  a pinned micromamba image and runs `micromamba create --offline --file <list>` inside the build.
- For pip environments, exact URLs are downloaded and hash-verified natively into the context.
  After Conda installation, container Python runs `pip --isolated install --no-index --no-deps
  --no-build-isolation --require-hashes` on those cached files. Source wheels are built inside
  the image using its locked build tools. Build networking stays disabled.
- The image is named by the digest of the lock, the digest of the base image, the platform
  and the recipe, including all pip artifact pins. A second build of the same environment
  finds the image and builds nothing.
- The inspected immutable Docker configuration ID goes into the task cache key. It is kept
  separate from registry manifest and exported archive digests.
- Completed-image receipts and verified transport archives use the deepest shared content
  store. A compatible local image can be registered; a missing local image can be restored.
- Packages are checked only for a cold build. A later workflow can reuse the image after
  package-cache eviction. Current environment YAML and repodata remain necessary to resolve
  the exact preparation identity. Online metadata uses the gateway cache policy.
- Local Snakemake rules with only `conda:` get the prepared image before task cache lookup.
  Included rules resolve the YAML from the declaring rule's folder. Preparation is cancellable;
  path-only historical image pins and frozen Conda repeats cannot bypass current preparation.

Configure the local path explicitly in the existing iunctus TOML file:

```toml
[conda]
base = "registry/micromamba@sha256:<selected-platform-manifest>"
platform = "linux/amd64"
installer = "micromamba"
offline = false # default; true requires cached metadata and package bytes
capabilities = "/srv/iunctus/conda/target.json"
repodata = "/srv/iunctus/conda/repodata"
packages = "/srv/iunctus/conda/packages"
```

Replace the base placeholder with a real pinned manifest. The base must already provide
Micromamba. The capability JSON has exactly `base`, `platform` and `virtual_packages`; each
package has `name`, `version` and `build`. Its base/platform must match the configuration.
These are trusted operator inputs obtained from the target container or deployment metadata.

Online HTTPS acquisition is the default when networking is allowed. Setting
`[conda].offline = true` or `[network].default = "none"` keeps acquisition offline. Build commands always install cached, SHA-256 verified
packages with build networking disabled. An offline cold-cache error explains
which cache is missing and how to populate it during a network-enabled run or
copy prepared metadata and hash-named package files from another machine.

Prepared JSON lives at `<repodata>/<channel>/<conda-platform>/repodata.json` and the channel's
`noarch/repodata.json`. Online metadata uses `<repodata>/gateway/`. Package files live at
`<packages>/<SHA-256 hex>` and are acquired only before a cold image build. Source reads and
contexts have finite byte limits. The cold path inspects the exact base manifest and platform
before building, with network disabled, one CPU and 1 GiB memory. Hot reuse does not require
the base or package files. Classic builder availability is required; BuildKit support remains open.

The focused native fixture `real_conda_rule_reuses_its_image_after_package_cache_eviction`
accepts `IUNCTUS_CONDA_TEST_FIXTURE` containing `target.json`, `repodata/` and `packages/`.
It installs a pinned tzdata package, executes two fresh native Snakemake workflows with task
cache reuse disabled, deletes its own copied package cache between runs, and checks the same
completed-image receipt and native output.

## Limits

- Workflow preparation supports local YAML and the local executor only. Remote executors,
  remote environment sources and named host environments are unsupported.
- Unsupported pip forms above and Conda namespace syntax are refused. No host Conda, mamba,
  pip or uv program is invoked. Micromamba and pip remain inside the container image build.
- Target capability discovery still relies on the existing operator-provided, target-bound
  JSON design. Classic builder support is required; BuildKit remains unsupported.
- Offline gateway queries require the needed metadata to have been cached previously.
  Missing metadata or packages fail clearly. Prepared JSON keeps the existing 256 MiB source
  limit; online queries avoid loading complete channels into the engine's package index.
- The ignored HTTPS acquisition test locks and verifies tzdata and then reproduces the same lock
  and package bytes offline. It does not qualify a live container build or remote executor.
- The local fake-index tests cover dependency closure, extras, backtracking, target markers
  and tags, hashes, input refusals, static source tools, offline reuse and lock compatibility.
  `CARGO_INCREMENTAL=0 nice -n 19 cargo test -p conda-environment --test pip_build -j 8 --locked
  -- --ignored --nocapture` locks and builds Python plus `requests==2.32.3` with local Docker.
  It checks offline relocking and the installed import, disables build networking, limits the
  build to one CPU and 1 GiB memory, and removes its images. This test passed on 2026-10-08
  with Python 3.12.15, 27 Conda packages and five pip packages; output included
  `Successfully built 7e5354c689ad` and `requests=2.32.3`.
