<!-- How SLURM, Kubernetes, TES, AWS Batch and Google Batch jobs get their files, with or without shared folders. -->
<!-- Copyright (c) 2026 The uwe Contributors -->
<!-- SPDX-License-Identifier: MIT or Apache-2.0 -->

# Remote executors and staging

SLURM, Kubernetes, TES, AWS Batch and Google Batch take a `staging` setting in `[executor.slurm]`, `[executor.kubernetes]`, `[executor.tes]`, `[executor.aws-batch]` or `[executor.google-batch]` of the config file. It decides how a job gets its inputs and how its outputs come back. In every mode the task container runs the original image of the task, unchanged.

| Mode | Executors | What the nodes need |
| --- | --- | --- |
| `shared-path` (default) | SLURM, Kubernetes | The store and scratch folders at the same paths as on the machine that runs uwe |
| `object-store` | SLURM, Kubernetes, TES, AWS Batch, Google Batch | Network access to the staging root in an object store |
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

## AWS Batch

`-executor aws-batch` runs each attempt of a task as one AWS Batch job. uwe calls the AWS Batch and CloudWatch Logs APIs itself, signed with Signature Version 4, so it needs no AWS CLI. The jobs share no folders with this machine, so this executor always uses object-store staging with presigned requests.

```toml
[executor.aws-batch]
region = "eu-west-1"                        # else AWS_REGION or AWS_DEFAULT_REGION
queue = "my-queue"                          # for tasks that name no queue
staging = "object-store"
staging_root = "s3://bucket/uwe-staging"
helper_path = "/opt/uwe/uwe"                # the static uwe binary on the hosts of the compute environment
# helper_url = "https://example.org/uwe"    # or an address that the job downloads, with helper_sha256
# helper_file = "/home/me/uwe-x86_64"       # or a binary that uwe uploads to the staging root
job_role = "arn:aws:iam::123456789012:role/uwe-job"
# execution_role = "arn:aws:iam::123456789012:role/uwe-execution"   # needed on Fargate
# logs_group = "/uwe/batch"                 # else AWS's /aws/batch/job
# volumes = ["/scratch", "/refs:/refs:ro"]  # host folders that every job sees
# max_spot_attempts = 3                     # runs a job again when its spot instance went away
# share_identifier = "blue"                 # fair-share scheduling, with scheduling_priority
# terminate_unschedulable = true            # stops a job that no compute environment can run
# platform = "fargate"                      # else "ec2"
# poll_seconds = 10
# task_folder = "/tmp/uwe"                  # where a job makes the folder of its attempt
# profile = "lab"                           # a profile of the shared AWS files
```

The calls use AWS's default credential chain: the environment, the shared files (with `profile` or `AWS_PROFILE`), web identity, and container and instance roles. `AWS_ENDPOINT_URL_BATCH`, `AWS_ENDPOINT_URL_CLOUDWATCH_LOGS` and `AWS_ENDPOINT_URL` name other endpoints, as AWS's tools read them. The account needs `batch:DescribeJobDefinitions`, `RegisterJobDefinition`, `SubmitJob`, `DescribeJobs`, `TerminateJob` and `TagResource`, `logs:GetLogEvents`, and `iam:PassRole` for the job role.

A task runs like this:

1. uwe uploads the inputs and the wrapper of the task to the staging root and writes the plan.
2. uwe looks for an active job definition named `uwe-` and the image, whose parameter `uwe-digest` matches its settings, and registers one when there is none. It does this once per image and run. The definition holds the image, the job role, the log group, the volumes and, read-only, the folder of `helper_path`.
3. uwe submits the job with the queue, the command, the vCPUs, the memory in MiB and the GPUs of the task, its labels as tags, the spot retries, the fair share, and a time limit of the task's time plus `staging_minutes`.
4. The command of the job gets the helper, stages the inputs into `task_folder`, runs the wrapper of the task within its time, and sends the outputs back.
5. uwe asks about all running jobs together, up to 100 in one call, every `poll_seconds`. A cancelled task stops its job.
6. When a job fails before its task has an exit code, the error of the task shows the end of its CloudWatch log.

Limits:

- AWS Batch runs the entrypoint of the image around the command of the job, as it does for Nextflow. An image whose entrypoint does not run its arguments as a command cannot run tasks.
- The image needs `sh`. With `helper_url` or an uploaded binary it also needs `curl` or `wget`, and `sha256sum`. On EC2, `helper_path` avoids that: put the static binary on the AMI of the compute environment. Fargate has no host folders, so it needs the download.
- uwe registers its own job definitions and refuses `job-definition://` images. Containers never run privileged: uwe has no setting for it, and `containerOptions` refuses `--privileged`.
- A task without a queue fails before its job starts: set `queue`, or the queue of the task.
- `staging_access = "credentials"` is refused; the jobs use presigned requests.
- On Fargate, uwe rounds the vCPUs up to 1, 2, 4, 8 or 16 and moves the memory into the range that Fargate allows for them, with a warning. The disk of the task sets the ephemeral storage (21 to 200 GiB, default 50), and `linux/arm64` tasks run on ARM64. GPU tasks are refused.
- Tasks without network, tasks of several containers, tasks that run as a named user, and inputs at absolute paths are refused.
- A job that no compute environment of its queue can run waits with a warning, or fails with `terminate_unschedulable = true`.
- Array jobs, machine details of spot instances, and AWS Secrets Manager secrets are not supported.
- The tests run against a local fake of AWS Batch and CloudWatch Logs. Runs on a real AWS account are not verified yet.

### Nextflow pipelines

When no `-executor` is given, the `process.executor` of a Nextflow pipeline picks the executor:

| `process.executor` | uwe executor |
| --- | --- |
| `awsbatch` | `aws-batch` |
| `google-batch` | `google-batch` |
| `k8s` | `kubernetes` |
| `slurm` | `slurm` |
| `tes` | `tes` |
| `local` | `local` |

Without `process.executor`, the run keeps `default` of `[executor]`, else `local`. uwe runs all tasks of a run with one executor. A pipeline whose `withName` or `withLabel` selectors name another executor is refused before any task starts, and so is an executor that uwe does not have. `-executor` overrides the pipeline.

The `aws` scope fills the settings that `[executor.aws-batch]` leaves unset: `aws.region`, `aws.accessKey` with `aws.secretKey`, `aws.profile`, and `aws.batch.jobRole`, `executionRole`, `logsGroup`, `volumes`, `maxSpotAttempts`, `shareIdentifier`, `schedulingPriority`, `terminateUnschedulableJobs` and `platformType`, and `executor.$awsbatch.pollInterval`. An `s3://` `workDir` becomes the staging root when none is set. uwe ignores `aws.batch.cliPath`, as its jobs need no AWS CLI.

The directives `queue`, `cpus`, `memory`, `accelerator`, `time`, `disk` (on Fargate) and `resourceLabels` go into the job. `containerOptions` passes environment variables, CPUs and GPUs, and refuses other flags.

### Snakemake, CWL and Argo

Their tasks use `queue` of `[executor.aws-batch]`. A Snakemake rule names another queue with `resources: queue="..."`. CPUs, memory, GPUs, time and disk come from the resources of the task, as on the other executors.

## Google Batch

`-executor google-batch` runs each attempt of a task as one Google Batch job. uwe calls the Google Batch and Cloud Logging REST APIs itself, with OAuth access tokens, so it needs no gcloud and no Google SDK. The jobs share no folders with this machine, so this executor always uses object-store staging with presigned requests.

```toml
[executor.google-batch]
project = "my-project"                      # else the project_id of the key file, or GOOGLE_CLOUD_PROJECT
location = "europe-west4"                   # else us-central1
staging = "object-store"
staging_root = "gs://bucket/uwe-staging"
helper_path = "/opt/uwe/uwe"                # the static uwe binary on the VM image
# helper_url = "https://example.org/uwe"    # or an address that a first runnable downloads, with helper_sha256
# helper_file = "/home/me/uwe-x86_64"       # or a binary that uwe uploads to the staging root
# helper_download_image = "mirror.example.org/alpine"   # the image that downloads it, else docker.io/library/alpine
# credential_file = "/home/me/key.json"     # a service account key or user credentials
# service_account = "jobs@my-project.iam.gserviceaccount.com"   # the identity of the VMs
# machine_type = "n2-standard-8"            # or "template://<name>" for an instance template; else Google picks
# accelerator_type = "nvidia-tesla-t4"      # for GPU tasks that name no type
# spot = true                               # or preemptible = true
# max_spot_attempts = 3                     # runs a task again when its spot VM went away
# boot_disk_image = "batch-debian"
# boot_disk_gb = 50                         # for tasks without a disk of their own
# cpu_platform = "Intel Ice Lake"
# install_gpu_drivers = true                # for instance templates with GPUs
# install_ops_agent = true
# allowed_locations = ["zones/europe-west4-a"]
# network = "projects/p/global/networks/n"  # with subnetwork, and private_address = true for no external IP
# network_tags = ["uwe"]
# logs_path = "gs://bucket/logs"            # else Cloud Logging
# poll_seconds = 10
# task_folder = "/tmp/uwe"                  # the folder of the VM where a job makes the folder of its attempt
# batch_endpoint = "https://batch.googleapis.com"       # other addresses, for tests
# logging_endpoint = "https://logging.googleapis.com"
```

The credentials come from the first of: `credential_file`, the file of `GOOGLE_APPLICATION_CREDENTIALS`, gcloud's application default credentials (`application_default_credentials.json` in `CLOUDSDK_CONFIG` or `~/.config/gcloud`), and the metadata server of a Google VM (`GCE_METADATA_HOST` names another). A service account key gets its token through a signed JWT at its `token_uri`; user credentials refresh theirs. Other kinds of credentials files, like workload identity federation, are refused, and so is a key of another project than `project`, as Nextflow does. The identity needs `roles/batch.jobsEditor` and `roles/iam.serviceAccountUser` on the job service account. That account needs `roles/batch.agentReporter` and `roles/logging.logWriter`, and both need access to the bucket. uwe signs the requests of the staging root with the service account key of its storage provider (`credential_file` in `[storage.<name>]`).

A task runs like this:

1. uwe uploads the inputs and the wrapper of the task to the staging root and writes the plan.
2. uwe creates the job `uwe-<attempt>-<milliseconds>` with one task: the CPUs, the memory in MiB and the disk of the task as its boot disk, a time limit of the task's time plus `staging_minutes`, its labels on the job and the VMs, and the VM settings above.
3. Without `helper_path`, a first runnable in `helper_download_image` downloads the helper into `task_folder` and checks its SHA-256. The task runnable runs the image of the task with `task_folder` mounted. It stages the inputs in, runs the wrapper of the task within its time, and sends the outputs back.
4. uwe asks about the job every `poll_seconds`. A cancelled task deletes its job, which stops it.
5. When a job fails before its task has an exit code, the error of the task shows the end of its Cloud Logging log.

Limits:

- Google Batch runs the entrypoint of the image around the commands of the job. An image whose entrypoint does not run its arguments as a command cannot run tasks.
- The image needs `sh`. The `task_folder` of the VM must allow running programs. With `helper_path` it must also be writable for the user of the image; the download runnable makes it writable for all.
- Google's exit codes from 50001 to 50006 (spot VM preempted, VM stopped reporting, VM rebooted, VM and task not answering, maximum run time, VM recreated) fail the attempt as a failure of the machine. 50005 with a time of the task counts as a task that ran too long (exit code 143). With `max_spot_attempts`, Google Batch runs the task again itself on exit code 50001.
- Containers never run privileged: uwe has no setting for it, `containerOptions` refuses `--privileged`, and Nextflow's `fusion.enabled = true` is refused.
- `staging_access = "credentials"` is refused; the jobs use presigned requests.
- Tasks without network, tasks of several containers, tasks that run as a named user, and inputs at absolute paths are refused. A GPU task needs a GPU type, from the task or `accelerator_type`.
- Array jobs, a machine type per process, scratch disks of a type, machine type patterns and Seqera's Cloud Info are not supported. The jobs stay in Google Batch after they end, as with Nextflow.
- The tests run against a local fake of Google Batch, its token endpoint and Cloud Logging, and `just emulator-test` stages a job through fake-gcs-server. Runs on a real Google Cloud project are not verified yet.

### Nextflow pipelines

`process.executor = 'google-batch'` picks this executor, as the table under AWS Batch shows. The `google` scope fills the settings that `[executor.google-batch]` leaves unset: `google.project`, `google.location`, and `google.batch.serviceAccountEmail`, `network`, `subnetwork`, `usePrivateAddress`, `spot`, `preemptible`, `maxSpotAttempts`, `autoRetryExitCodes`, `bootDiskImage`, `bootDiskSize`, `cpuPlatform`, `installGpuDrivers`, `installOpsAgent`, `allowedLocations`, `networkTags` and `logsPath`, and `executor.$google-batch.pollInterval`. A `gs://` `workDir` becomes the staging root when none is set. uwe ignores `google.batch.gcsfuseOptions`, `google.enableRequesterPaysBuckets` and `executor.$google-batch.jobName`, with a warning.

The directives `cpus`, `memory`, `accelerator`, `time`, `disk` and `resourceLabels` go into the job. Google Batch has no queues, so `queue` has no effect. uwe does not read the `machineType` directive; set `machine_type` instead.

### Snakemake, CWL and Argo

Their tasks use the settings of `[executor.google-batch]`. CPUs, memory, GPUs, time and disk come from the resources of the task, as on the other executors.

## Default task time

A task can set its own time limit: `time` in Nextflow, `runtime` in Snakemake, `ToolTimeLimit` in CWL and `activeDeadlineSeconds` in Argo. Give every other task a limit with `task_minutes` in `[executor]`, or with `-task-minutes` on the command line:

```toml
[executor]
default = "tes"
task_minutes = 1440
```

The limit applies only to tasks without a time of their own. A task's own time always wins. The value must be above zero. Every executor treats the default like a task's own time. So a SLURM job, Kubernetes pod or TES task also gets its `staging_minutes` on top. On TES, the time and margin count from the first time the server reports the task as `RUNNING` or `UNKNOWN`. Some servers report `UNKNOWN` for a running task, or never leave it. uwe then cancels a task that runs longer, and the attempt fails.

Tasks with a time do not run together in one container (see `-no-grouping`), so a default turns that grouping off.

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
