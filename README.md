<!-- Build, installation and operating guidance for the iunctus workflow engine. -->
<!-- Copyright (c) 2026 The uwe Contributors -->
<!-- SPDX-License-Identifier: MIT or Apache-2.0 -->

# iunctus

iunctus is a pure Rust workflow executor for Nextflow, Snakemake, CWL and Argo Workflows. It reads all four languages natively, plans them into one shared execution graph, records every run and stores results by content digest in a central cache that works across languages. It does NOT require Python, Groovy, JavaScript or Java on the executor level and can run as a single static binary!

## Why?

Bioinformatics has many domain-specific workflow languages. Each has its own set of benefits and drawbacks. I was annoyed by the recurring problems I encountered over and over again. I was also annoyed that some of the cool features were hidden behind cloud offerings or were essentially paid features tied to API keys, etc.

Most have limited or inconsistent support for containers, Kubernetes, and so on, or they require a shared working directory. Almost none support multiple cloud storage providers simultaneously, such as several S3 providers at once. Caches are tied to one language or even to a single workflow, so the same tool with the same inputs will run again simply because it was called from a different workflow. So I decided to build this experiment: Can we unify all of that in a single Rust binary?

So far, the answer is a confident yes. A Snakemake rule and a Nextflow process that use the same container with the same inputs share one cache entry. You can move a run from your laptop to a cluster or the cloud without changing the workflow. Every run can also be repeated later with the same inputs, images, and code. And everything is container first, with no compromises. Everything MUST be run in a container, supporting the FAIR principles and making executions more reproducible.

## Features

**Workflow languages**

- **Nextflow** DSL2: processes, workflows, channels and operators, configs and profiles, `publishDir`, `storeDir` and `-entry`. Groovy is evaluated natively in Rust, no JVM needed.
- **Snakemake**: rules, wildcards, checkpoints, shadow rules, embedded Python, lifecycle hooks, yte and jinja2 templates, and `cwl:` rules, which run on the CWL runtime of iunctus. Python is evaluated natively in Rust.
- **CWL**: CommandLineTools and workflows with scatter, `when`, `valueFrom`, secondary files and JavaScript expressions (through the Boa engine, no Node.js).
- **Argo Workflows**: steps, DAGs, loops, conditions, artifacts, retries, timeouts, exit hooks, mutexes and semaphores. A suspended step waits until `iunctus resume <run> [<step>] [<name>=<value>]...`, which works like `argo resume` with `argo node set --output-parameter`. Sidecars share the network of their step and stop after it; daemon steps run until their readiness probe passes, give later steps their `ip`, and stop when their template ends or their `activeDeadlineSeconds` pass. With a rootless engine, or one in a VM, the local executor probes HTTP and TCP readiness through a port published on 127.0.0.1. Both run on the local executor and Kubernetes; SLURM and TES refuse them.
- Run workflows straight from GitHub, GitLab, Bitbucket or Gitea, pinned to a tag, branch or commit with `-r`.

**Execution**

- One shared task graph and task runner for every language.
- Executors for local **Docker** and **Podman**, **SLURM** (with Apptainer or Podman), **Kubernetes** and **GA4GH TES**.
- A simulator executor that plans a whole workflow and makes placeholder outputs, without running any tools.
- Conda environments are solved natively (rattler and resolvo) and turned into container images.

**Storage and caching**

- A content-addressed store: every result is kept once, by digest.
- One cache across all languages, with resume for interrupted runs. Like Nextflow's `-resume`, a resumed run also takes the results of its earlier runs for images without a registry digest, such as locally built tags.
- Storage providers for **S3**, **Google Cloud Storage**, **Azure Blob Storage**, **HTTPS**, **FTP** and local folders. Configure as many as you like at the same time, for example several S3 providers side by side.
- A remote store (for example on S3) with an optional local front store for speed.
- `iunctus clean` removes data no run still needs, and shows you what it would remove first.

**Runs and reproducibility**

- Every run is recorded in a local SQLite database: `iunctus runs` lists them.
- `iunctus changes <run>` shows which workflow files, inputs and images changed since a run, and `iunctus repeat <run>` runs it again with exactly what it used.
- `iunctus why` and `iunctus origin` explain why a task ran or where a result came from.
- A local web page for runs with `iunctus page`, or live during a run with `iunctus run -page`.
- RO-Crate export with `iunctus export`, plus timing, comparison and report commands.

**Checks**

- `iunctus check <workflow>` finds everything that would stop a real run (unsupported features, broken references, missing files, tasks without an image) before any task starts.
- `iunctus check-machine` checks the container engine, the store and the executor setup.
- `iunctus check-order <workflow>` finds steps whose results depend on which task finished first.
- `iunctus inputs` and `iunctus outputs` print the inputs and outputs of any workflow as JSON Schema, and every run checks its inputs against it.

## Getting started

You need a container engine (Docker or Podman) and a Rust toolchain to build iunctus. Install Rust through [rustup](https://rustup.rs). [rust-toolchain.toml](rust-toolchain.toml) pins the Rust version, so rustup downloads the right one on the first build.

### Linux

Install Docker or Podman from your distribution, plus a C compiler (for example `build-essential` on Debian and Ubuntu, `base-devel` on Arch). Then build and install from a checkout of this repository:

```sh
cargo build --release --locked -p iunctus
mkdir -p "$HOME/.local/bin"
install -m 755 target/release/iunctus "$HOME/.local/bin/iunctus"
```

Add `$HOME/.local/bin` to your `PATH` if needed. For a fully static binary that runs on any x86_64 Linux, use `just release-binary` (needs `musl-gcc`).

If you use Podman, start its socket with `systemctl --user enable --now podman.socket`. iunctus looks for Docker first and Podman second; set `IUNCTUS_ENGINE=podman` to pick Podman.

### macOS

Install the Xcode command line tools (`xcode-select --install`) and either Docker Desktop or Podman. Then build from a checkout, like on Linux:

```sh
cargo build --release --locked -p iunctus
mkdir -p "$HOME/.local/bin"
install -m 755 target/release/iunctus "$HOME/.local/bin/iunctus"
```

iunctus talks to the engine through its socket. With Docker Desktop, either enable "Allow the default Docker socket to be used" in the advanced settings, or point iunctus at the socket in your home folder:

```sh
export DOCKER_HOST="unix://$HOME/.docker/run/docker.sock"
```

With Podman, start the machine and export its socket:

```sh
podman machine init && podman machine start
export DOCKER_HOST="unix://$(podman machine inspect --format '{{.ConnectionInfo.PodmanSocket.Path}}')"
```

### Windows

Use WSL 2. Install it with `wsl --install` in an administrator PowerShell, open the Ubuntu shell and follow the Linux steps from there. For containers, either install Docker Desktop and enable the WSL 2 integration for your distribution, or install Docker or Podman directly inside WSL.

Keep the checkout and your data inside the WSL file system (for example under `~/`), not under `/mnt/c`. It is much faster and avoids permission problems with container mounts.

### First run

```sh
iunctus --version
iunctus check-machine
iunctus run demo/snakemake/hello.smk
iunctus runs
```

Run these from the checkout. The example uses a Debian image and writes greetings under `demo/snakemake/hello/`. The `demo/` folder has more examples for every language; try `iunctus run demo/nextflow/main.nf`, `iunctus run demo/cwl/hello_world.cwl demo/cwl/hello_world-job.json -default-image debian:stable-slim` or `iunctus run demo/argo/hello-world.yaml`.

You can also run a pipeline straight from a repository:

```sh
iunctus run nf-core/demo -r 1.2.0 -profile test,docker --outdir results
```

`iunctus help` lists all commands and `iunctus run -help` lists all run options.

## Remote executors without shared folders

SLURM and Kubernetes take a `staging` setting in `[executor.slurm]` or `[executor.kubernetes]` of the config file. In every mode the task container runs the original image of the task, unchanged.

**`staging = "shared-path"`** (the default): the nodes see the store and scratch folders at the same paths as the machine of iunctus, for example on a shared file system. On Kubernetes these are `hostPath` volumes, or one shared volume that holds the cache of iunctus:

```toml
[executor.kubernetes.shared_volume]
claim = "iunctus-cache"   # a ReadWriteMany claim; or nfs_server and nfs_path of an NFS export
path = "/mnt/iunctus"     # where iunctus and the pods see the volume
```

A ReadWriteMany folder that all pods share is an antipattern on Kubernetes: it is slow, it fails as one piece, and it does not scale. Prefer `object-store` or `kubedal`.

A kubeconfig of a managed cluster may name an `exec` credential plugin, like `aws`, `gke-gcloud-auth-plugin` or `kubelogin`. iunctus runs it to get a token. These plugins come from your kubeconfig, and they are the only external programs that iunctus starts for Kubernetes.

**`staging = "object-store"`**: nothing is shared. iunctus uploads the inputs to the staging root and writes a plan of presigned requests. On the node, the static iunctus binary downloads the inputs, checks their SHA-256, and uploads the outputs after the task. The nodes need no credentials. The staging root is an address of a configured storage provider that signs requests: S3, Google Cloud Storage with the key file of a service account (`credential_file`), or Azure Blob Storage with the account key (`key`). For Azure, iunctus makes a service SAS for each request, which reads or writes one blob until `presign_hours` have passed. A `sas_token` alone cannot sign requests, as each request would carry all of its rights; give the account key, or use `staging_access = "credentials"`.

By default iunctus sends its own binary with the jobs. It uploads its running binary once to `cas/` in the staging root, by its SHA-256, and each job downloads it from there through a presigned request and checks its SHA-256. This works only when iunctus itself is a statically linked Linux binary for the machine of the nodes, like the one of `just release-binary`. Otherwise, for example on macOS or with a dynamically linked build, iunctus stops before the first job and says so. Then `helper_file` names a static binary on this machine to upload instead, for example one for arm64 nodes.

```toml
[executor.slurm]
staging = "object-store"
staging_root = "s3://bucket/iunctus-staging"
# helper_file = "/home/me/iunctus-x86_64"    # instead of the running binary
node_folder = "/local/scratch"               # the default is $TMPDIR, else /tmp
```

A SLURM job makes a new private folder in the node folder. It gets the binary there with `curl` or `wget` and checks it, stages the inputs into the `data` folder in it, runs the container with binds to the `data` folder only, and sends the outputs back. The task cannot change the binary, the plan or the log of the job, which run or count after the task. Apptainer pulls the image on each node. iunctus cannot see the machine of the nodes, so it must match the binary: the job checks `uname -m` and fails with a clear message when it differs. On every exit, also after a failure or a cancellation, the job removes its folder. Before that it sends its log back, with the helper or else with `curl` or `wget`, and iunctus shows the end of the log in the error of the task. A log that cannot go back stays beside the folder on the node, as `iunctus-<attempt>.<random>.log`. A job that cannot even make its folder sends the reason alone. Instead of the uploaded binary, a job may get it from a path on the nodes, or from an address of your own:

```toml
[executor.slurm]
helper_url = "https://example.org/iunctus"   # the static binary of `just release-binary`
helper_sha256 = "<its SHA-256, 64 hex digits>"
# helper_path = "/opt/iunctus/bin/iunctus"   # instead, when the nodes have it already
```

On Kubernetes the task image stays as it is, so the binary comes in through init containers. The first one runs Alpine, which downloads the binary into an `emptyDir` volume and checks it. The second one runs the binary in the same Alpine image and stages the inputs into an `emptyDir` volume. This happens before the task container starts, because the kubelet would make each missing `subPath` folder of the task container as root, and then the task could not write there. The task container sees the binary in the `emptyDir` volume and sends the outputs back after the task. The Job asks for nodes of the machine of the binary with the `kubernetes.io/arch` node selector. The Alpine image is pinned by its digest; `helper_download_image` names another image with a shell, `wget` and `sha256sum`, for example from a registry mirror. Alpine is used rather than busybox because the `wget` of busybox does not check the certificate of the server, and the presigned address of the binary should not go to a server that pretends to be the staging root. A mirror image should check certificates too.

```toml
[executor.kubernetes]
staging = "object-store"
staging_root = "s3://bucket/iunctus-staging"
# helper_file = "/home/me/iunctus-arm64"                 # instead of the running binary
# helper_download_image = "mirror.example.org/alpine"    # instead of docker.io/library/alpine
```

Instead, a helper image may hold the binary:

```toml
[executor.kubernetes]
helper_image = "registry.example.org/iunctus-helper:0.1.0"   # built with `just helper-image`
helper_mount = "image-volume"                                # or "init-container"
```

The helper image holds only the static binary, at `/iunctus`. An init container of that image stages the inputs. The task container sees the binary as a read-only image volume. Image volumes need Kubernetes 1.31 or later with the `ImageVolume` feature gate on, and a container runtime that supports them. On other clusters, `helper_mount = "init-container"` copies the binary into an `emptyDir` volume instead.

With `staging_access = "credentials"`, the nodes reach the staging root with credentials of their own instead of presigned requests. The plan then names the objects, and the helper sends each output in parts, so a file may be larger than 5 GiB. The helper reads the storage providers of the staging root from the environment variable `IUNCTUS_STAGING_STORAGE`, as `[storage.<name>]` tables like those of the config file:

```toml
[executor.slurm]
staging_access = "credentials"
staging_secret_file = "/home/me/.config/iunctus/staging.toml"   # only its owner may read it

[executor.kubernetes]
staging_access = "credentials"
staging_secret = "iunctus-staging"   # a Secret in the namespace of the Jobs
```

A SLURM job gets the text of the file in its environment, never in its job script. A Kubernetes pod gets the keys of the Secret as environment variables, and the Job only names the Secret:

```sh
kubectl create secret generic iunctus-staging --from-file=IUNCTUS_STAGING_STORAGE=staging.toml
```

On Kubernetes the task container holds these credentials too, as it sends the outputs back, and SLURM administrators can see the environment of jobs. Give the nodes credentials that reach only the staging root.

Limits of object-store staging:

- A SLURM job, Kubernetes pod or TES task may run `staging_minutes` longer than the time of its task (default 30, or 5 with `shared-path`), for image pulls, staging and the start of its container. The task itself still stops at its own time. On TES only a task that runs through the shell of its wrapper can do that; a task that runs its command without a shell stops at the end of the margin. When a SLURM partition allows no more than the time of the task, so that the job would wait for ever as `PartitionTimeLimit` or `sbatch` refuses it, iunctus warns and submits it once more without the margin; a task that asks for more than its partition allows fails with a clear message.
- Presigned requests work for `presign_hours` (default 168, the most that S3 allows). This must cover the time that a task waits in the queue and runs. Requests signed with temporary credentials end with those credentials.
- With presigned requests, each output file goes up in one request of at most 5 GiB, the S3 limit for one upload. A larger file fails the task with a clear message. With credentials it goes up in parts of 8 MiB, and S3 takes at most 10,000 parts, so about 78 GiB.
- A task gives back at most `max_outputs` files (default 1024).
- With presigned requests, anyone who can read the job script or the Job sees the address of its plan, which grants access to the files of that task until it expires.
- Tasks of several containers need `shared-path`.
- The uploaded binary is downloaded through a presigned request also with `staging_access = "credentials"`, as the job has no helper yet to read the staging root. The staging root must sign requests then too.

**`staging = "kubedal"`** (Kubernetes only): the [kubedal](https://github.com/arunaengine/kubedal) CSI driver places the inputs from a file list before the pod starts and uploads the outputs when the pod ends. The pod gets no iunctus binary.

```toml
[executor.kubernetes]
staging = "kubedal"
staging_root = "s3://bucket/iunctus-staging"
kubedal_data_node = "iunctus-staging"       # a DataNode whose bucket and root are the staging root
kubedal_namespace = "data"                  # the default is the namespace of the Jobs
kubedal_driver = "kubedal.arunaengine.org"  # the default
kubedal_upload_minutes = 60                 # how long kubedal may upload after a pod (default)
```

Install [kubedal](https://github.com/arunaengine/kubedal) with its Helm chart:

```sh
helm install kubedal oci://ghcr.io/arunaengine/charts/kubedal --namespace kube-system
```

Make a DataNode that may write, with its credentials Secret, whose `config` names the bucket and the `root` of the staging root. Put it in the namespace of the Jobs, or give their service account the right to `get` it. iunctus waits until Kubernetes removed the pod, which happens only once kubedal uploaded the outputs, and then checks that every output file that the task listed is there. The pod gets the group of the user who runs iunctus as `fsGroup`, so that the task can write to the volume.

On a busy cluster the tools and the API answer late or say no for a while. `query_seconds` in `[executor.slurm]` sets how long one call of `squeue`, `sacct` or `scancel` may take (default 60); a queue whose answers fail is asked less and less often, and the end of a job may take four such calls before iunctus counts it as unclear. On Kubernetes a Job whose pod the namespace quota does not allow yet waits, as in a queue. A pod that finds no node fails after `unschedulable_minutes` in `[executor.kubernetes]` (default 60) with the reason of the scheduler, unless the cluster autoscaler adds nodes for it, which starts the time again. A Job that has no pod after `start_grace_seconds` (default 120, between 10 and 3600) is checked: iunctus reads why from the events of the namespace, so the account of the kubeconfig needs the right to `list` events. Without that right iunctus logs one warning, and a Job without a pod waits up to `unschedulable_minutes`, as for quota, before it fails.

Both modes keep inputs under `cas/` and the results of attempts under `attempts/` in the staging root. With `staging_cleanup = true`, iunctus removes the results and the plan of every attempt once its job surely ended: after a success, a failure or a cancellation, once it has collected what it needs. For a job that may still run, iunctus keeps the journal of the attempt, and the recovery at the next run removes them. iunctus tries to collect results a few times; results that it still cannot collect stay in the staging root, and a warning names their prefix. TES with `staging = "object-store"` takes `staging_cleanup` in `[executor.tes]` too. It never removes the inputs, as other tasks may use them; a lifecycle rule of the bucket can.

## Known pitfalls

- **Containers are required.** Every task runs in a container. Host-installed tools are never used as a fallback, so declare an image in the workflow or pass `-default-image IMAGE`.
- **Languages are native subsets.** iunctus implements the languages in Rust instead of calling the original engines. A workflow that runs on Nextflow or Snakemake can still use a feature iunctus does not support yet. Run `iunctus check <workflow>` first, unsupported features give exit code 33 with a clear message.
- **Flags differ from the original engines.** iunctus options use one hyphen (`-executor`, `-profile`), workflow parameters use two (`--outdir`). Many Nextflow and Snakemake flags are not accepted. Nextflow `-c FILE` configures the workflow, not iunctus, use `-iunctus-config FILE` for that.
- **Nextflow work and resume layouts differ.** iunctus has its own store and resume, so tools that read Nextflow `work/` folders or `.nextflow` logs will not find them.
- **Snakemake jobs share the working directory.** With the local executor, and with SLURM on shared folders, Snakemake jobs run in place in the working directory, as in Snakemake: a file that a job writes without declaring it reaches the next job, and links stay links. A stored result answers such a job when its command, image, environment, the path of the folder and the content of its declared inputs are the same; iunctus then puts the outputs back into the folder. A job that reads files it does not declare can get a stale result this way, as Snakemake's own time checks can miss such a file, so declare what a job reads. The content of an `update()` output counts like a declared input. Outputs outside the working directory, by an absolute path or `..`, are written in place, and their job gets the folders of those outputs as well. Like Snakemake, a run locks the files of its jobs in `.snakemake/locks`, so a second run in the same folder that would write the same files stops with a `LockException`; `nolock: true` in a profile leaves the folder unlocked, and removing the lock files that the error names frees a lock that a killed run left. An output that a killed run left incomplete stops the next run with an `IncompleteFilesException`, as in Snakemake; `rerun-incomplete: true` in a profile makes it again. The variables that `envvars` names reach the jobs as they start: records, exports and cache keys hold only their names. Other executors give each job a folder with its declared files only. A local run forms no group jobs, as Snakemake does locally.
- **Python packages in Snakemake.** Embedded Python runs natively, but third-party packages run in container helpers, and arbitrary Python objects cannot cross that boundary.
- **Conda and pip.** Conda environments become container images for local runs and are published for remote executors. Editable, local or VCS `pip:` requirements are not supported.
- **CWL images that tools build need a registry on Kubernetes and TES.** A `DockerRequirement` with `dockerFile`, `dockerLoad` or `dockerImport` makes the image with the local engine. For Kubernetes, TES, and SLURM with Podman or Docker, set `registry = "host/team/cwl"` in a `[cwl]` section of the config file to a registry that the nodes can pull from: iunctus pushes the image there, and the jobs pull it by its digest. Without it, such a requirement is refused as unsupported (exit code 33), and such a hint fails the tool, as in cwltool, instead of running it in the default image. SLURM with Apptainer or Singularity needs no registry: iunctus saves the image once as a docker-archive tar in `cwl-images` of its cache folder, named by its image ID. The tasks name the image by that ID, not by the file, so their results serve other machines and the local executor too, and `iunctus changes` compares the ID. `iunctus clean` removes archives that no kept run needs. On shared folders one job turns it into a SIF file in the images folder; with `staging = "object-store"` each job gets the archive through the staging root and Apptainer runs it from there. With a registry, the jobs pull from the registry instead.
- **CWL JavaScript runs in a helper container.** iunctus makes its image `iunctus-cwl-js:<digest of the binary>` from the running binary the first time a CWL expression needs it, and removes the helper images of older binaries. The helper has no network and bounded memory. To use an image of your own, build it with `python3 scripts/cwl-js-image.py target/release/iunctus --engine docker` (or `--engine podman`) and name it in `IUNCTUS_CWL_JS_IMAGE`. The helper runs a Linux binary for the architecture of the container engine. Where iunctus itself is not one, for example on macOS, set `IUNCTUS_CWL_JS_BINARY` to a static Linux binary of iunctus (`just release-binary`), and iunctus makes the helper image from that. As in cwltool, code in `$(...)` or `${...}` runs only under a required `InlineJavascriptRequirement`.
- **Repository sources.** GitHub, GitLab, Bitbucket and Gitea repositories come through their APIs, without submodules. Any other server works through Git over HTTPS, with submodules, like `iunctus run https://git.example.org/team/pipeline.git -r v1.0`; an address without `.git` is a repository only when the server answers as Git, otherwise it is a file to download. The `https` storage provider of the config file for each address, also of each submodule, gives the user and password or token; an account never goes to another host or over plain HTTP. A provider with an account must name its server in each prefix, like `https://git.example.org/`; a bare `https://` would send the account to every server, so iunctus refuses it. SSH URLs and archive links are not supported. Cached branches stay on their remembered commit until you pass `-latest`. Argo git artifacts use the same Git client, also over HTTPS only. Links in a checkout that lead out of it become plain files that hold their target, as Git writes links without `core.symlinks`.
- **Links in inputs and Argo volumes.** Tasks never read a file of this machine through a link that leads out of an input folder, an unpacked archive or a host path: archives with such links are refused, and so are input folders and `hostPath` folders. Archives unpack to at most 64 GiB and one million entries. Steps that write the same `hostPath` at the same time each write back only the files they made, changed or removed, so that the changes of all of them stay, as on one shared folder.
- **Remote executors move files in one of three ways.** By default SLURM and Kubernetes nodes must see the store and scratch folders at the same paths. With `staging = "object-store"` (SLURM and Kubernetes) or `staging = "kubedal"` (Kubernetes) nothing is shared and the files go through an object store, see [Remote executors without shared folders](#remote-executors-without-shared-folders). TES can use an S3 bucket too.
- **Disk use grows.** The store, cache and engine images grow over time. Preview with `iunctus clean` before removing anything with `-remove`.
- **Published links point into the store.** Nextflow's default `publishDir` makes symbolic links to files in the store. `iunctus clean` keeps every stored file that such a link of a recorded run still points to, so no published result breaks, and the preview says how many files it keeps for that. To free that space, replace the links with copies (or remove them) first. Store files are read-only, so a write through a link fails instead of changing the cache; a damaged stored file is found when it is read and stored again by the next run that makes it.
- **Clean-up waits for running tasks.** `iunctus clean -remove` waits up to five minutes while tasks prepare their inputs, and new tasks wait meanwhile. It removes old downloads too: copies of remote files, copies of contents of a remote store, and parts of crashed downloads.

## License

The project license is `MIT OR Apache-2.0`. Bundled upstream examples keep their own notices.

## AI Disclaimer

A lot of this code was generated with the help of generative AI using the latest available models at the time (Opus 4.6-5.5, GPT-5.5 / 5.6-Sol / 6 Astra). The code has been manually audited and checked, but a few quirks still exist. It's still a proof of concept nonetheless and needs some additional work to be fully production ready.
