<!-- How SLURM, Kubernetes and TES jobs get their files, with or without a shared file system. -->
<!-- Copyright (c) 2026 The uwe Contributors -->
<!-- SPDX-License-Identifier: MIT or Apache-2.0 -->

# Remote executors and staging

SLURM, Kubernetes and TES take a `staging` setting in `[executor.slurm]`, `[executor.kubernetes]` or `[executor.tes]` of the config file. It decides how a job gets its inputs and how its outputs come back. In every mode the task container runs the original image of the task, unchanged.

| Mode | Executors | What the nodes need |
| --- | --- | --- |
| `shared-path` (default) | SLURM, Kubernetes | The store and scratch folders at the same paths as on the machine that runs uwe |
| `object-store` | SLURM, Kubernetes, TES | Network access to the staging root in an object store |
| `kubedal` | Kubernetes | The [kubedal](https://github.com/arunaengine/kubedal) CSI driver |

## Shared paths

With `staging = "shared-path"`, the nodes see the store and scratch folders at the same paths as the machine that runs uwe, for example on a shared file system. On Kubernetes these are `hostPath` volumes, or one shared volume that holds the uwe cache:

```toml
[executor.kubernetes.shared_volume]
claim = "uwe-cache"       # a ReadWriteMany claim; or nfs_server and nfs_path of an NFS export
path = "/mnt/uwe"         # where uwe and the pods see the volume
```

Use `object-store` or `kubedal` when uwe and the nodes do not share a file system.

The kubeconfig of a managed cluster may name an `exec` credential plugin, like `aws`, `gke-gcloud-auth-plugin` or `kubelogin`. uwe runs it to get a token. These plugins come from your kubeconfig, and they are the only external programs that uwe starts for Kubernetes.

## Object store

With `staging = "object-store"`, nothing is shared. uwe uploads the inputs to the staging root and writes a plan of presigned requests. On the node, the static uwe binary downloads the inputs, checks their SHA-256, and uploads the outputs after the task. The nodes need no credentials.

This helper-based staging applies to SLURM and Kubernetes. TES takes an `s3://` staging root and passes the S3 addresses of inputs and outputs to the TES server, which needs access to that storage.

uwe asks TES servers for memory and disk in gigabytes of 10^9 bytes, as the TES specification defines `ram_gb` and `disk_gb`.

The staging root is an address on a configured storage provider that can sign requests:

- S3.
- Google Cloud Storage, with the key file of a service account (`credential_file`).
- Azure Blob Storage, with the account key (`key`). uwe makes a service SAS for each request, which reads or writes one blob until `presign_hours` have passed. A `sas_token` alone cannot sign requests, because each request would carry all of its rights. Give the account key, or use `staging_access = "credentials"`.

### The helper binary

By default uwe sends its own binary along with the jobs. It uploads the running binary once to `cas/` in the staging root, named by its SHA-256. Each job downloads it from there through a presigned request and checks the SHA-256.

This only works when uwe itself is a statically linked Linux binary for the machine type of the nodes, like the one from `just release-binary`. Otherwise, for example on macOS or with a dynamically linked build, uwe stops before the first job and says so. In that case `helper_file` names a static binary on this machine to upload instead, for example one built for arm64 nodes.

```toml
[executor.slurm]
staging = "object-store"
staging_root = "s3://bucket/uwe-staging"
# helper_file = "/home/me/uwe-x86_64"        # instead of the running binary
node_folder = "/local/scratch"               # the default is $TMPDIR, else /tmp
```

### On SLURM

A SLURM job makes a new private folder in the node folder. It fetches the binary there with `curl` or `wget` and checks it. It stages the inputs into the `data` folder inside, runs the container with a bind of the `data` folder only, and sends the outputs back. The binary, the plan and the job log stay outside the folder that the task can write. Apptainer pulls the image on each node.

uwe cannot see the machine type of the nodes, so the binary must match it. The job checks `uname -m` and fails with a clear message when it differs.

On every exit, also after a failure or a cancellation, the job removes its folder. Before that it sends its log back, with the helper or else with `curl` or `wget`, and uwe shows the end of the log in the error of the task. A log that cannot be sent back stays next to the folder on the node, as `uwe-<attempt>.<random>.log`. A job that cannot even make its folder sends back only the reason.

Instead of the uploaded binary, a job can take one from a path on the nodes, or from your own address:

```toml
[executor.slurm]
helper_url = "https://example.org/uwe"       # the static binary from `just release-binary`
helper_sha256 = "<its SHA-256, 64 hex digits>"
# helper_path = "/opt/uwe/bin/uwe"           # instead, when the nodes have it already
```

### On Kubernetes

On Kubernetes the task image stays as it is, so the binary comes in through init containers:

1. The first init container runs Alpine. It downloads the binary into an `emptyDir` volume and checks it.
2. The second runs the binary in the same Alpine image and stages the inputs into an `emptyDir` volume.

Staging happens before the task container starts because the kubelet would create each missing `subPath` folder of the task container as root, and the task could then not write there. The task container sees the binary in the `emptyDir` volume and sends the outputs back after the task. The Job asks for nodes of the binary's architecture with the `kubernetes.io/arch` node selector.

The Alpine image is pinned by its digest. `helper_download_image` names another image with a shell, `wget` and `sha256sum`, for example from a registry mirror. uwe uses Alpine rather than busybox because the busybox `wget` does not check the server certificate, and the presigned address of the binary should not go to a server that only pretends to be the staging root. A mirror image should check certificates too.

```toml
[executor.kubernetes]
staging = "object-store"
staging_root = "s3://bucket/uwe-staging"
# helper_file = "/home/me/uwe-arm64"                     # instead of the running binary
# helper_download_image = "mirror.example.org/alpine"    # instead of docker.io/library/alpine
```

A helper image can hold the binary instead:

```toml
[executor.kubernetes]
helper_image = "registry.example.org/uwe-helper:0.1.0"       # built with `just helper-image`
helper_mount = "image-volume"                                # or "init-container"
```

The helper image holds only the static binary, at `/uwe`. An init container of that image stages the inputs. The task container sees the binary as a read-only image volume. Image volumes need Kubernetes 1.31 or later with the `ImageVolume` feature gate on, and a container runtime that supports them. On other clusters, `helper_mount = "init-container"` copies the binary into an `emptyDir` volume instead.

### Credentials instead of presigned requests

With `staging_access = "credentials"`, the nodes reach the staging root with their own credentials. The plan then names the objects, and the helper uploads each output in parts, so a file may be larger than 5 GiB. The helper reads the storage providers of the staging root from the environment variable `UWE_STAGING_STORAGE`, written as `[storage.<name>]` tables like in the config file:

```toml
[executor.slurm]
staging_access = "credentials"
staging_secret_file = "/home/me/.config/uwe/staging.toml"       # only its owner may read it

[executor.kubernetes]
staging_access = "credentials"
staging_secret = "uwe-staging"       # a Secret in the namespace of the Jobs
```

A SLURM job gets the text of the file in its environment, never in its job script. A Kubernetes pod gets the keys of the Secret as environment variables, and the Job only names the Secret:

```sh
kubectl create secret generic uwe-staging --from-file=UWE_STAGING_STORAGE=staging.toml
```

On Kubernetes the task container holds these credentials too, because it sends the outputs back. SLURM administrators can see the environment of jobs. Give the nodes credentials that reach only the staging root.

### Limits

- A SLURM job, Kubernetes pod or TES task may run `staging_minutes` longer than its task (default 30, or 5 with `shared-path`), for image pulls, staging and the start of its container. The task itself still stops at its own time limit. On TES only a task that runs through the shell of its wrapper can use this margin. A task that runs its command without a shell stops at the end of the margin.
- When a SLURM partition allows no more than the time of the task, the job would wait forever as `PartitionTimeLimit`, or `sbatch` would refuse it. uwe then warns and submits it once more without the margin. A task that asks for more time than its partition allows fails with a clear message.
- Presigned requests are valid for `presign_hours` (default 168, the most that S3 allows). This must cover the time a task waits in the queue and runs. Requests signed with temporary credentials end when those credentials end.
- With presigned requests, each output file goes up in one request of at most 5 GiB, the S3 limit for one upload. A larger file fails the task with a clear message. With credentials it goes up in parts of 8 MiB. S3 takes at most 10,000 parts, so about 78 GiB.
- A task returns at most `max_outputs` files (default 1024).
- With presigned requests, anyone who can read the job script or the Job sees the address of its plan. That address grants access to the files of the task until it expires.
- Tasks with several containers need `shared-path`.
- The uploaded binary is always downloaded through a presigned request, also with `staging_access = "credentials"`, because the job has no helper yet to read the staging root. So the staging root must be able to sign requests in that case too.

## kubedal

With `staging = "kubedal"` (Kubernetes only), the [kubedal](https://github.com/arunaengine/kubedal) CSI driver places the inputs from a file list before the pod starts, and uploads the outputs when the pod ends. The pod gets no uwe binary.

```toml
[executor.kubernetes]
staging = "kubedal"
staging_root = "s3://bucket/uwe-staging"
kubedal_data_node = "uwe-staging"           # a DataNode whose bucket and root are the staging root
kubedal_namespace = "data"                  # the default is the namespace of the Jobs
kubedal_driver = "kubedal.arunaengine.org"  # the default
kubedal_upload_minutes = 60                 # how long kubedal may upload after a pod (default)
```

Install kubedal with its Helm chart:

```sh
helm install kubedal oci://ghcr.io/arunaengine/charts/kubedal --namespace kube-system
```

Create a DataNode that may write, with its credentials Secret, whose `config` names the bucket and the `root` of the staging root. Put it in the namespace of the Jobs, or give their service account the right to `get` it.

uwe waits until Kubernetes has removed the pod, which only happens after kubedal uploaded the outputs. Then it checks that every output file the task listed is there. The pod gets the group of the user who runs uwe as `fsGroup`, so that the task can write to the volume.

## Busy clusters

On a busy cluster the tools and the API can answer late, or refuse for a while.

- `query_seconds` in `[executor.slurm]` sets how long one call of `squeue`, `sacct` or `scancel` may take (default 60). After a query fails, uwe waits longer before it asks SLURM again. The end of a job may take four such calls before uwe treats it as unclear.
- On Kubernetes, a Job whose pod the namespace quota does not allow yet waits, as in a queue.
- A pod that finds no node fails after `unschedulable_minutes` in `[executor.kubernetes]` (default 60) with the reason from the scheduler. If the cluster autoscaler adds nodes for it, the time starts again.
- A Job that has no pod after `start_grace_seconds` (default 120, between 10 and 3600) is checked. uwe reads the reason from the events of the namespace, so the kubeconfig account needs the right to `list` events. Without that right uwe logs one warning, and a Job without a pod waits up to `unschedulable_minutes`, as for quota, before it fails.

## Cleaning the staging root

Both remote modes keep inputs under `cas/` and the results of attempts under `attempts/` in the staging root.

With `staging_cleanup = true`, uwe removes the results and the plan of each attempt once its job has surely ended: after a success, a failure or a cancellation, and after it collected what it needs. For a job that may still be running, uwe keeps the journal of the attempt, and recovery on the next run removes them. uwe tries several times to collect results. Results it still cannot collect stay in the staging root, and a warning names their prefix.

TES with `staging = "object-store"` takes `staging_cleanup` in `[executor.tes]` as well.

uwe never removes the inputs under `cas/`, because other tasks may still use them. A lifecycle rule on the bucket can.

## TES tags

`tags` in `[executor.tes]` adds the same tags to every TES task. It is a table of texts. Some servers read tags as settings, for example to give a task network access or to name a group:

```toml
[executor.tes]
tags = { "aruna-engine.org/network" = "open", "aruna-engine.org/group" = "lab" }
```

A `[executor.tes.sites.<name>]` table takes its own `tags`. Like every other key of a site, it does not inherit from `[executor.tes]`. A site without `tags` adds none.

uwe sets `uwe-attempt`, `uwe-submission` and the `idempotency_tag` itself. A tag with one of these names, or with an empty name, is a config error. Values may be empty.

## Read-only TES inputs

Some TES servers stage input files as read-only and owned by another user, and run the task as a user without rights to change them. uwe makes executable inputs and the helper executable with `chmod +x`. When `chmod` fails, uwe copies the file next to itself, makes the copy executable and moves it over the original. This needs a folder the task user may write to. If the copy fails too, the task stops with an error, as before. When `chmod` works, nothing changes.
