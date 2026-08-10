# Conda environments

Decision 12 says that a Conda environment is locked for the platform of the task and then built
into a container image. No Conda installation on the host takes part. This page says what
iunctus does today without network, what needs the network, and how images will be built.

The code is in the crate `conda-environment`. The command is `iunctus lock-environment`.

## What works without network

- **Environment files.** iunctus reads the environment files of Snakemake rules and nf-core
  modules: `name`, `channels`, `dependencies` and the `pip:` list. Other keys, like `prefix`,
  are ignored. An error names the line of the file.
- **Match specs.** Both forms are read. Environment files write `samtools=1.17` (versions that
  start with 1.17), `samtools==1.17` (exactly 1.17), `bioconda::fastqc=0.12.1=hdfd78af_0` (a
  channel, a version and a build) and ranges like `python>=3.8,<3.9`. The dependencies of
  packages write `python >=3.8,<3.9.0a0`, `libzlib 1.2.13` (exactly this version) and
  `python_abi 3.8.* *_cp38` (a pattern of the build). Spaces around operators count for
  nothing, as in conda. The form with brackets, like `numpy[version='>=1.20']`, is not read yet.
- **The order of versions.** Versions compare as conda's `VersionOrder` does: an epoch
  (`1!2.0`), components split at `.` and `_`, numbers as numbers, `dev` before any other text,
  `post` after any number, and a local part after `+`. So `1.1a1 < 1.1 = 1.1.0 < 1.1.0post1`.
- **The lock.** A small resolver chooses one package for each name from the repodata in a
  local folder (see below). The lock is JSON: the platform, the channels, the match specs, the
  `pip` packages, and each package with its channel, file, address, SHA-256 and MD5. Its
  digest names the environment.
- **The explicit list.** A lock can be written as conda's explicit list: `@EXPLICIT`, then the
  address of each package with its MD5. Conda and micromamba install such a list without
  solving again.

In the Snakemake test workflows, 2032 of 2053 environment files read. The others are broken
YAML (tabs, a list instead of a mapping), templates, or match specs that conda rejects too,
like `bioconda:bismark` with one colon or `dedup ==0.12.8-0`. Two use the form with brackets.

## The folder of repodata

The resolver reads the repodata of each channel and platform from the cache folder:

```
<cache>/conda/<channel>/<platform>/repodata.json
<cache>/conda/<channel>/noarch/repodata.json
```

`<cache>` is `$XDG_CACHE_HOME/iunctus`, else `~/.cache/iunctus`. Each file is the one that the
channel serves, like `https://conda.anaconda.org/conda-forge/linux-64/repodata.json`, or
`https://repo.anaconda.com/pkgs/main/linux-64/repodata.json` for Anaconda's own channels.

iunctus does not download repodata yet. When a file is missing, `iunctus lock-environment`
exits with 3 (the machine is not ready) and names each missing file with its address and the
place where it belongs. Downloading it needs the network; so does anything that uses the
packages themselves.

Channel names work as in conda: `defaults` stands for `pkgs/main` and `pkgs/r`, and
`nodefaults` for none. An environment file without channels gets `conda-forge`, as Miniforge
has it. A channel may be named with its platform, like `conda-forge/linux-64::zlib`.

## The resolver

The resolver is no full solver like conda's or libsolv. It is a search with going back:

1. It takes the specs of the environment file in their order.
2. For each spec it takes the newest package that fits, from the first channel that has the
   name at all (strict channel priority). Among equal versions, the higher build number wins,
   then the newer timestamp. A `.conda` file wins over a `.tar.bz2` file of the same package.
3. The package's dependencies join the end of the queue. A name that is chosen already must
   fit every later spec of that name.
4. What a package constrains (`constrains` in the repodata) limits the other packages, both
   ways.
5. When no package fits, the search goes back to the last choice that has another candidate.
6. It gives up after 200000 tries and says so, rather than search for long.

Virtual packages of a Linux container (`__unix`, `__linux`, `__glibc`, `__archspec`) count as
present. Others, like `__cuda`, count as missing, so a package that needs them does not fit.

A package that no channel has for a spec is named at once. When the packages exist but cannot
be installed together, the error lists the specs of the environment. Both exit with 1.

The resolver can choose other packages than conda would, when several answers fit. The lock
makes this harmless: the lock, not the environment file, decides what the image holds.

## `iunctus lock-environment`

```
iunctus lock-environment <conda environment file> [-platform <name>] [-output <file>]
```

- `-platform` is the conda platform, like `linux-64` or `linux-aarch64`. The default is Linux
  on the processor of this machine, as containers here run Linux.
- Without `-output`, the lock goes to standard output as JSON. With `-output`, it goes to that
  file; a name that ends in `.txt` gets the explicit list. The command then prints the number
  of packages and the digest of the environment.
- The lock does not pin the `pip` packages. A warning says so; pip installs them when the image
  is built.

## Building the image (not done yet)

The plan follows decision 12 and decision 26 (one binary, no external programs):

- The build goes through the Docker Engine API (bollard), which Podman serves too. iunctus
  sends a build context as a tar stream: the explicit list and a short recipe that starts from
  a pinned micromamba image and runs `micromamba install --file <list>` inside the build.
- The image is named by the digest of the lock, the digest of the base image, the platform
  and the recipe. A second build of the same environment finds the image and builds nothing.
- The digest of the built image goes into the cache key of the task, like the digest of any
  other image. The environment file alone does not.
- The build needs the network to fetch the packages, unless a folder of packages is mounted
  into it. A run without network can use images that were built before.
- Rules and processes with only `conda:` then get such an image in place of the default image.

## Limits

- No download of repodata or packages yet; see above.
- The form of match specs with brackets is not read.
- `pip` packages are not pinned.
- The resolver is a search with a limit, not a full solver. A large environment with many
  conflicts can need more tries than it allows.
