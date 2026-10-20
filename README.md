<!-- Build, installation and operating guidance for the iunctus workflow engine. -->
<!-- Copyright (c) 2026 The iunctus Contributors -->
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
- **Snakemake**: rules, wildcards, checkpoints, shadow rules, embedded Python and lifecycle hooks. Python is evaluated natively in Rust.
- **CWL**: CommandLineTools and workflows with scatter, `when`, `valueFrom`, secondary files and JavaScript expressions (through the Boa engine, no Node.js).
- **Argo Workflows**: steps, DAGs, loops, conditions, artifacts, retries, timeouts, exit hooks, mutexes and semaphores.
- Run workflows straight from GitHub, GitLab, Bitbucket or Gitea, pinned to a tag, branch or commit with `-r`.

**Execution**

- One shared task graph and task runner for every language.
- Executors for local **Docker** and **Podman**, **SLURM** (with Apptainer or Podman), **Kubernetes** and **GA4GH TES**.
- A simulator executor that plans a whole workflow and makes placeholder outputs, without running any tools.
- Conda environments are solved natively (rattler and resolvo) and turned into container images.

**Storage and caching**

- A content-addressed store: every result is kept once, by digest.
- One cache across all languages, with resume for interrupted runs.
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

A SLURM job makes a new private folder in the node folder. It gets the binary there with `curl` or `wget` and checks it, stages the inputs into the `data` folder in it, runs the container with binds to the `data` folder only, and sends the outputs back. The task cannot change the binary, the plan or the log of the job, which run or count after the task. Apptainer pulls the image on each node. iunctus cannot see the machine of the nodes, so it must match the binary: the job checks `uname -m` and fails with a clear message when it differs. Instead of the uploaded binary, a job may get it from a path on the nodes, or from an address of your own:

```toml
[executor.slurm]
helper_url = "https://example.org/iunctus"   # the static binary of `just release-binary`
helper_sha256 = "<its SHA-256, 64 hex digits>"
# helper_path = "/opt/iunctus/bin/iunctus"   # instead, when the nodes have it already
```

On Kubernetes the task image stays as it is, so the binary comes in through init containers. The first one runs busybox, which downloads the binary into an `emptyDir` volume and checks it. The second one runs the binary in the same busybox image and stages the inputs into an `emptyDir` volume. This happens before the task container starts, because the kubelet would make each missing `subPath` folder of the task container as root, and then the task could not write there. The task container sees the binary in the `emptyDir` volume and sends the outputs back after the task. The Job asks for nodes of the machine of the binary with the `kubernetes.io/arch` node selector. The busybox image is pinned by its digest; `helper_download_image` names another image with a shell, `wget` and `sha256sum`, for example from a registry mirror.

```toml
[executor.kubernetes]
staging = "object-store"
staging_root = "s3://bucket/iunctus-staging"
# helper_file = "/home/me/iunctus-arm64"                 # instead of the running binary
# helper_download_image = "mirror.example.org/busybox"   # instead of docker.io/library/busybox
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

Both modes keep inputs under `cas/` and the results of attempts under `attempts/` in the staging root. With `staging_cleanup = true`, iunctus removes the results and the plan of an attempt once it has collected them. It never removes the inputs, as other tasks may use them; a lifecycle rule of the bucket can.

## Known pitfalls

- **Containers are required.** Every task runs in a container. Host-installed tools are never used as a fallback, so declare an image in the workflow or pass `-default-image IMAGE`.
- **Languages are native subsets.** iunctus implements the languages in Rust instead of calling the original engines. A workflow that runs on Nextflow or Snakemake can still use a feature iunctus does not support yet. Run `iunctus check <workflow>` first, unsupported features give exit code 33 with a clear message.
- **Flags differ from the original engines.** iunctus options use one hyphen (`-executor`, `-profile`), workflow parameters use two (`--outdir`). Many Nextflow and Snakemake flags are not accepted. Nextflow `-c FILE` configures the workflow, not iunctus, use `-iunctus-config FILE` for that.
- **Nextflow work and resume layouts differ.** iunctus has its own store and resume, so tools that read Nextflow `work/` folders or `.nextflow` logs will not find them.
- **Python packages in Snakemake.** Embedded Python runs natively, but third-party packages run in container helpers, and arbitrary Python objects cannot cross that boundary.
- **Conda and pip.** Conda environments become container images for local runs and are published for remote executors. Editable, local or VCS `pip:` requirements are not supported.
- **CWL JavaScript needs a helper image.** Build it once from your binary with `python3 scripts/cwl-js-image.py target/release/iunctus --engine docker` (or `--engine podman`).
- **Repository sources.** SSH URLs, arbitrary Git servers, submodules and archive links are not supported. Cached branches stay on their remembered commit until you pass `-latest`.
- **Remote executors move files in one of three ways.** By default SLURM and Kubernetes nodes must see the store and scratch folders at the same paths. With `staging = "object-store"` (SLURM and Kubernetes) or `staging = "kubedal"` (Kubernetes) nothing is shared and the files go through an object store, see [Remote executors without shared folders](#remote-executors-without-shared-folders). TES can use an S3 bucket too.
- **Disk use grows.** The store, cache and engine images grow over time. Published results can link to data in the store, so preview with `iunctus clean` before removing anything with `-remove`.

## License

The project license is `MIT OR Apache-2.0`. Bundled upstream examples keep their own notices.

## AI Disclaimer

A lot of this code was generated with the help of generative AI using the latest available models at the time (Opus 4.6-5.5, GPT-5.5 / 5.6-Sol / 6 Astra). The code has been manually audited and checked, but a few quirks still exist. It's still a proof of concept nonetheless and needs some additional work to be fully production ready.
