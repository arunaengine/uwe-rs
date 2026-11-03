<!-- Behavior of uwe that differs from the original engines or that often surprises users. -->
<!-- Copyright (c) 2026 The uwe Contributors -->
<!-- SPDX-License-Identifier: MIT or Apache-2.0 -->

# Known pitfalls

## General

- **Containers are required.** Every task runs in a container. uwe never falls back to tools installed on the host, so declare an image in the workflow or pass `-default-image IMAGE`.
- **Languages are native subsets.** uwe implements the languages in Rust instead of calling the original engines. A workflow that runs on Nextflow or Snakemake may still use a feature that uwe does not support yet. Run `uwe check <workflow>` first. Unsupported features give exit code 33 and a clear message.
- **Flags differ from the original engines.** uwe options use one hyphen (`-executor`, `-profile`). Workflow parameters use two (`--outdir`). Many Nextflow and Snakemake flags are not accepted. In Nextflow, `-c FILE` configures the workflow, not uwe. Use `-uwe-config FILE` for uwe.
- **Disk use grows.** The store, the cache and the engine images grow over time. Preview with `uwe clean`, then remove with `uwe clean -remove`.

## Nextflow

- **Work and resume layouts differ.** uwe has its own store and its own resume. Tools that read the `work/` folders or `.nextflow` logs of Nextflow will not find them.
- **A plain rerun runs everything again.** As in Nextflow, only `-resume` takes the results of earlier runs. `uwe why <run> <task>` tells you why a task ran.
- **Published links point into the store.** The default `publishDir` mode of Nextflow makes symbolic links to files in the store. `uwe clean` keeps every stored file that such a link of a recorded run still points to, so no published result breaks. The preview says how many files it keeps for that reason. To free that space, replace the links with copies, or remove them, first. Store files are read-only, so writing through a link fails instead of changing the cache. A damaged stored file is found when it is read, and the next run that makes it stores it again.

## Snakemake

**Jobs share the working directory.** With the local executor, and with SLURM on shared folders, Snakemake jobs run in place in the working directory, as in Snakemake. A file that a job writes without declaring it reaches the next job, and links stay links.

- **Stored results.** With `-resume`, a stored result answers a job when its command, image, environment, folder path and the content of its declared inputs are the same. uwe then puts the outputs back into the folder. Without `-resume`, a job takes a stored result only when nothing outside its description can matter: its image has a registry digest and it has no network access, for example with `-no-network`. A job that reads files it does not declare can get a stale result this way, just as Snakemake's own time checks can miss such a file. So declare what a job reads. The content of an `update()` output counts like a declared input.
- **Outputs outside the working directory**, by an absolute path or `..`, are written in place, and their job also gets the folders of those outputs.
- **Locks.** Like Snakemake, a run locks the files of its jobs in `.snakemake/locks`. A second run in the same folder that would write the same files stops with a `LockException`. `nolock: true` in a profile leaves the folder unlocked. Removing the lock files named in the error frees a lock that a killed run left behind.
- **Incomplete outputs.** An output that a killed run left incomplete stops the next run with an `IncompleteFilesException`, as in Snakemake. `rerun-incomplete: true` in a profile makes it again.
- **`envvars`.** The variables named by `envvars` reach the jobs when they start. Records, exports and cache keys hold only their names.
- **Other executors** give each job a folder with only its declared files.
- **Group jobs.** A local run forms no group jobs, as in Snakemake.
- **Python packages.** Embedded Python runs natively, but third-party packages run in container helpers. Arbitrary Python objects cannot cross that boundary.
- **Conda and pip.** Conda environments become container images for local runs and are published for remote executors. Editable, local or VCS `pip:` requirements are not supported.

## CWL

- **Images that tools build need a registry on Kubernetes and TES.** A `DockerRequirement` with `dockerFile`, `dockerLoad` or `dockerImport` builds the image with the local engine.
  - For Kubernetes, TES, and SLURM with Podman or Docker, set `registry = "host/team/cwl"` in a `[cwl]` section of the config file to a registry that the nodes can pull from. uwe pushes the image there, and the jobs pull it by its digest. Without a registry, such a requirement is refused as unsupported (exit code 33), and such a hint fails the tool, as in cwltool, instead of running it in the default image.
  - SLURM with Apptainer or Singularity needs no registry. uwe saves the image once as a docker-archive tar in `cwl-images` in its cache folder, named by its image ID. Tasks name the image by that ID, not by the file, so their results also serve other machines and the local executor, and `uwe changes` compares the ID. `uwe clean` removes archives that no kept run needs. On shared folders one job turns the archive into a SIF file in the images folder. With `staging = "object-store"` each job gets the archive through the staging root, and Apptainer runs it from there. With a registry, the jobs pull from the registry instead.
- **JavaScript runs in a helper container.** The first time a CWL expression needs it, uwe builds the image `uwe-cwl-js:<digest of the binary>` from the running binary, and removes the helper images of older binaries. The helper has no network and bounded memory. To use your own image, build it with `python3 scripts/cwl-js-image.py target/release/uwe --engine docker` (or `--engine podman`) and name it in `UWE_CWL_JS_IMAGE`. The helper runs a Linux binary for the architecture of the container engine. Where uwe itself is not such a binary, for example on macOS, set `UWE_CWL_JS_BINARY` to a static Linux build of uwe (`just release-binary`), and uwe builds the helper image from that. As in cwltool, code in `$(...)` or `${...}` only runs under a required `InlineJavascriptRequirement`.

## Sources and inputs

- **Repository sources.** GitHub, GitLab, Bitbucket and Gitea repositories are fetched through their APIs, without submodules. Any other server works through Git over HTTPS, with submodules, like `uwe run https://git.example.org/team/pipeline.git -r v1.0`. An address without `.git` counts as a repository only when the server answers as Git. Otherwise it is a file to download.
- **Repository credentials.** The `https` storage provider of the config file gives the user and password or token for each address, also for each submodule. An account is never sent to another host or over plain HTTP. A provider with an account must name its server in each prefix, like `https://git.example.org/`. uwe refuses a bare `https://`, because it would send the account to every server. SSH URLs and archive links are not supported.
- **Cached branches** stay on their remembered commit until you pass `-latest`.
- **Argo git artifacts** use the same Git client, also over HTTPS only. Links in a checkout that lead out of it become plain files that hold their target, as Git writes links without `core.symlinks`.
- **Links in inputs and Argo volumes.** A task never reads a file of this machine through a link that leads out of an input folder, an unpacked archive or a host path. Archives, input folders and `hostPath` folders with such links are refused. Archives unpack to at most 64 GiB and one million entries.
- **Argo `hostPath` writes.** Steps that write the same `hostPath` at the same time each write back only the files they made, changed or removed. So the changes of all of them stay, as on one shared folder.

## Remote executors

- **Files move in one of three ways.** By default SLURM and Kubernetes nodes must see the store and scratch folders at the same paths. With `staging = "object-store"` or `staging = "kubedal"` nothing is shared and the files go through an object store. See [Remote executors and staging](remote-executors.md).

## Cleaning up

- **Clean-up waits for running tasks.** `uwe clean -remove` waits up to five minutes while tasks prepare their inputs, and new tasks wait meanwhile. It also removes old downloads: copies of remote files, copies of the contents of a remote store, and parts of crashed downloads.
