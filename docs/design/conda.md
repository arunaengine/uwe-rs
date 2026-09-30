<!-- Offline Conda locking, container builds and image reuse. -->
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
  names the environment. Nonempty `pip:` requirements are refused before solving.
- **The explicit list.** A lock can be written as conda's explicit list: `@EXPLICIT`, then the
  address of each package with its MD5. Conda and micromamba install such a list without
  solving again.

The local Snakemake corpus has 2053 environment files. With rattler parsing, 2037 read and
16 are rejected. The five newly accepted files use version spellings supported by rattler,
including versions with hyphens and a comparison with a trailing wildcard. The remaining
rejections are malformed YAML, templates, unsupported extras, and namespace syntax such as
`bioconda:bismark`. Pip requirements are parsed for a clear lock-time refusal.

## The folder of repodata

The resolver reads the repodata of each channel and platform from the cache folder:

```
<cache>/conda/<channel>/<platform>/repodata.json
<cache>/conda/<channel>/noarch/repodata.json
```

`<cache>` is `$XDG_CACHE_HOME/iunctus`, else `~/.cache/iunctus`. Each file is the one that the
channel serves, like `https://conda.anaconda.org/conda-forge/linux-64/repodata.json`, or
`https://repo.anaconda.com/pkgs/main/linux-64/repodata.json` for Anaconda's own channels.

With `[conda] offline = false`, `rattler_repodata_gateway` fetches requested package names
and their dependency closure over HTTPS. It supports sharded and zstd repodata with fallback
to ordinary repodata, and keeps metadata under `<repodata>/gateway/`. Cloned acquisition
handles share one HTTP client and serialize requests. Package downloads also use this
client, verify SHA-256 before atomic publication, and reuse verified cached bytes. Connect
and request deadlines are 30 and 300 seconds; an individual package is limited to 4 GiB.

With `offline = true` (the default), prepared JSON files remain supported. If a prepared file
is missing, configured workflows and lock commands can use previously cached gateway
metadata, including stale metadata, without downloading. Packages must already be cached
for a cold build. The unconfigured lock command remains offline and reports missing JSON
files with their addresses and cache paths.

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
  configuration it reads only local JSON and infers no virtual packages.
- Pip requirements are refused in both lock and preparation paths. A separate Python
  dependency resolver and wheel installation path would be needed; they are outside this
  Conda implementation.

## Building and reusing images for local workflows

The local executor follows decision 12 and decision 26 (one binary, no host installers):

- The build goes through the Docker Engine API (bollard), which Podman serves too. iunctus
  sends a build context as a tar stream: the explicit list and a short recipe that starts from
  a pinned micromamba image and runs `micromamba create --offline --file <list>` inside the build.
- The image is named by the digest of the lock, the digest of the base image, the platform
  and the recipe. A second build of the same environment finds the image and builds nothing.
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
offline = false # true for prepared JSON or cached gateway metadata only
capabilities = "/srv/iunctus/conda/target.json"
repodata = "/srv/iunctus/conda/repodata"
packages = "/srv/iunctus/conda/packages"
```

Replace the base placeholder with a real pinned manifest. The base must already provide
Micromamba. The capability JSON has exactly `base`, `platform` and `virtual_packages`; each
package has `name`, `version` and `build`. Its base/platform must match the configuration.
These are trusted operator inputs obtained from the target container or deployment metadata.

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
- Pip requirements and Conda namespace syntax are refused. No host Conda, mamba or uv program
  is invoked. Micromamba remains inside the pinned container build base.
- Target capability discovery still relies on the existing operator-provided, target-bound
  JSON design. Classic builder support is required; BuildKit remains unsupported.
- Offline gateway queries require the needed metadata to have been cached previously.
  Missing metadata or packages fail clearly. Prepared JSON keeps the existing 256 MiB source
  limit; online queries avoid loading complete channels into the engine's package index.
- The ignored HTTPS smoke test locks and verifies tzdata and then reproduces the same lock
  and package bytes offline. It does not qualify a live container build or remote executor.
