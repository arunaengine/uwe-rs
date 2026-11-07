<!-- How SLURM, HTCondor, PBS, Kubernetes, TES, AWS Batch, Google Batch, Azure Batch and Nomad jobs get their files, with or without shared folders. -->
<!-- Copyright (c) 2026 The uwe Contributors -->
<!-- SPDX-License-Identifier: MIT or Apache-2.0 -->

# Remote executors and staging

SLURM, HTCondor, PBS, Kubernetes, TES, AWS Batch, Google Batch, Azure Batch and Nomad take a `staging` setting in `[executor.slurm]`, `[executor.htcondor]`, `[executor.pbs]`, `[executor.kubernetes]`, `[executor.tes]`, `[executor.aws-batch]`, `[executor.google-batch]`, `[executor.azure-batch]` or `[executor.nomad]` of the config file. It decides how a job gets its inputs and how its outputs come back. In every mode the task container runs the original image of the task, unchanged.

| Mode | Executors | What the nodes need |
| --- | --- | --- |
| `shared-path` (default) | SLURM, HTCondor, PBS, Kubernetes, Nomad | The store and scratch folders at the same paths as on the machine that runs uwe |
| `object-store` | SLURM, HTCondor, PBS, Kubernetes, TES, AWS Batch, Google Batch, Azure Batch, Nomad, Flyte, Snowflake | Network access to the staging root in an object store |
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
| `azurebatch` | `azure-batch` |
| `flyte` | `flyte` |
| `google-batch` | `google-batch` |
| `k8s` | `kubernetes` |
| `condor`, `ospool` | `htcondor` (see [HTCondor](#htcondor)) |
| `nomad` | `nomad` |
| `pbspro` | `pbs` (see [PBS](#pbs)) |
| `slurm` | `slurm` |
| `slurm_no_memory` | `slurm`, with `memory_per_cpu_gb` (see [SLURM memory per CPU](#slurm-memory-per-cpu)) |
| `snowflake` | `snowflake` |
| `tencentcloud-omics` | `tes`, with `platform = "tencent-omics"` |
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

## Azure Batch

`-executor azure-batch` runs each attempt of a task as one Azure Batch task. uwe calls the Azure Batch REST API itself, with the Shared Key of the account or an Entra ID token, so it needs no Azure CLI and no Azure SDK. The tasks share no folders with this machine, so this executor always uses object-store staging with presigned requests. The staging root is usually an Azure Blob container that a `[storage.<name>]` table of type `azure-blob-storage` with the account key serves; each request then carries a service SAS for one blob.

```toml
[executor.azure-batch]
account = "mylab"                           # else AZURE_BATCH_ACCOUNT_NAME, or the first part of endpoint
location = "westeurope"                     # with account: https://mylab.westeurope.batch.azure.com
# endpoint = "https://mylab.westeurope.batch.azure.com"
account_key = { environment = "AZURE_BATCH_ACCOUNT_KEY" }   # the Shared Key
staging = "object-store"
staging_root = "az://container/uwe-staging"
pool = "small"                              # the pool of tasks that name no queue
# helper_path = "/opt/uwe/uwe"              # the static uwe binary on the nodes
# helper_url = "https://example.org/uwe"    # or an address that Batch downloads, with helper_sha256
# helper_file = "/home/me/uwe-x86_64"       # or a binary that uwe uploads; else the running binary
# tenant_id = "<tenant>"                    # a service principal, with client_id and client_secret
# managed_identity = "system"               # or the client id of a user-assigned identity
# allow_pool_creation = true                # creates a named pool that does not exist
# auto_pools = true                         # tasks without a pool run on a pool of [executor.azure-batch.pools.auto]
# delete_jobs = true                        # at the end of the run
# delete_pools = true                       # the pools that uwe created, at the end of the run
# delete_tasks = true                       # all ended tasks; false keeps them; unset deletes the successful ones
# terminate_jobs = false                    # else the jobs end once their tasks ended, at the end of the run
# job_max_hours = 720
# job_quota_retries = 3                     # waits job_quota_seconds (120) while the account has its most active jobs
# registry_server = "lab.azurecr.io"        # a login for the pools that uwe creates, with registry_user and registry_password
# poll_seconds = 10
# task_folder = "/tmp/uwe"                  # where a task makes the folder of its attempt
# api_version = "2024-07-01.20.0"
# authority_endpoint = "https://login.microsoftonline.com"   # other addresses, for tests
# identity_endpoint = "http://169.254.169.254/metadata/identity/oauth2/token"

[executor.azure-batch.pools.small]
vm_size = "Standard_E4d_v5"                 # else Standard_D4a_v4
vm_cpus = 4                                 # the cores of vm_size; needed to create a pool of another size
# vm_memory_gb = 32                         # lets the memory of a task count toward its slots
# vm_count = 1                              # nodes; max_vm_count when auto scaling, else 3 x vm_count
# auto_scale = true                         # with scale_minutes (5) and scale_formula
# low_priority = true                       # spot nodes
# schedule_policy = "pack"                  # or "spread"
# run_as = "batchuser"                      # a user account of the pool
# publisher = "microsoft-dsvm"              # the image, with offer and sku; or vm_image_id
# virtual_network = "/subscriptions/<id>/resourceGroups/<group>/providers/Microsoft.Network/virtualNetworks/<net>/subnets/<subnet>"
# start_script = "echo ready"               # runs once on each node, as a user without rights
```

The credentials come from the first of: a managed identity (`managed_identity`, `AZURE_MANAGED_IDENTITY_USER` with a client id, or `AZURE_MANAGED_IDENTITY_SYSTEM=true`), whose token comes from the instance metadata service; a service principal (`tenant_id`, `client_id` and `client_secret`, or `AZURE_TENANT_ID`, `AZURE_CLIENT_ID` and `AZURE_CLIENT_SECRET`), whose token comes from Entra ID with the client credentials grant; and the Shared Key (`account_key`, or `AZURE_BATCH_ACCOUNT_KEY`), which signs each request. Tokens have the scope `https://batch.core.windows.net/.default`. uwe never logs keys, tokens or SAS URLs.

A task runs like this:

1. uwe finds the pool: the queue of the task, else `pool`, else with `auto_pools` the pool `uwe-pool-<digest>-<vm size>` of the `auto` settings and the labels of the task. An existing pool must be active, and its slots per node must equal `vm_cpus` when that is set. A missing pool is created with `allow_pool_creation` (auto pools always): the first verified Linux image of the account with the publisher, offer and node agent of the settings, Docker-compatible containers, the registry login, one task slot per core, and auto scaling or a fixed number of dedicated or low priority nodes.
2. uwe creates one job per pool and run, `uwe-<random>-<pool>`, with a time limit of `job_max_hours`.
3. uwe uploads the inputs and the wrapper of the task to the staging root and writes the plan.
4. uwe adds the task `uwe-<attempt>`: the image of the task with `--cpu-shares` and `--memory`, the plan and the helper as resource files by their SAS URLs (Batch downloads them into the working folder before the task starts), its slots, a time limit of the task's time plus `staging_minutes`, and a task user without rights (or `run_as` of the pool). The command line runs `sh`, which checks the SHA-256 of a downloaded helper, stages the inputs in, runs the wrapper of the task within its time, and sends the outputs back. The script is in the environment variable `UWE_SCRIPT`, as a command line holds no newlines.
5. uwe asks about the task every `poll_seconds`. While it waits for a node, uwe looks at its pool once a minute: a pool of fixed size with resize errors and no nodes fails the task, and an auto scaling pool whose errors are all `AllocationTimedout` or `AllocationFailed` gets a warning and the task waits. A cancelled task is terminated.
6. The error of a failed task shows the end of its `stdout.txt` and `stderr.txt`. Then uwe deletes the task as `delete_tasks` says.
7. At the end of the run, each job ends once its tasks ended (`terminate_jobs`), and uwe deletes the jobs and the pools it created when asked. A pool that an active job of another run uses stays. uwe does not wait for the deletions.

Limits:

- A task takes as many slots as its CPUs, or its share of the memory of a node when `vm_memory_gb` is set, at most the slots of a node.
- Containers never run privileged: `privileged` and `start_privileged` of a pool are refused, and so are Nextflow's `privileged` pools and `fusion.enabled = true`. Tasks and start scripts run as users without admin rights.
- The container keeps the entrypoint of its image around the command line. The image needs `sh`, and `sha256sum` when the helper is downloaded (`helper_url` or the uploaded binary). With `helper_path`, the container sees the folder of the helper read-only.
- Real Azure Batch downloads resource files over HTTP or HTTPS, so the staging root must be in an object store, like Azure Blob Storage or S3.
- `delete_pools` deletes only the pools that uwe created in the run and that no active job of another run uses; Nextflow deletes every pool it used.
- `staging_access = "credentials"` is refused; the tasks use presigned requests.
- Tasks without network, tasks of several containers, tasks that run as a named user, and inputs at absolute paths are refused.
- uwe ships no table of VM sizes: VM size patterns and the automatic choice of a VM size for auto pools are not supported, and creating a pool of another size than `Standard_D4a_v4` needs `vm_cpus`. Azure Files shares, output files of Batch, low priority nodes per task and Fusion are not supported either.
- The tests run against a local fake of Azure Batch and the Entra ID token endpoints, and `just emulator-test` stages a task through Azurite. Runs on a real Azure account are not verified yet.

### Nextflow pipelines

`process.executor = 'azurebatch'` picks this executor, as the table under AWS Batch shows. The `azure` scope fills the settings that `[executor.azure-batch]` leaves unset: `azure.batch.accountName`, `accountKey`, `endpoint`, `location`, `autoPoolMode`, `allowPoolCreation`, `deleteJobsOnCompletion`, `deletePoolsOnCompletion`, `deleteTasksOnCompletion`, `terminateJobsOnCompletion`, `jobMaxWallClockTime`, `maxJobQuotaRetries`, `jobQuotaRetryDelay` and the pools of `azure.batch.pools` (`vmType`, `vmCount`, `maxVmCount`, `autoScale`, `scaleFormula`, `scaleInterval`, `lowPriority`, `schedulePolicy`, `runAs`, `publisher`, `offer`, `sku`, `virtualMachineImageId`, `allowUnverifiedImages`, `virtualNetwork` and `startTask.script`), `azure.registry`, `azure.activeDirectory`, `azure.managedIdentity`, and `executor.$azurebatch.pollInterval`. An `az://` `workDir` becomes the staging root when none is set; its storage provider comes from a `[storage.<name>]` table, as uwe does not read `azure.storage`. uwe ignores `azure.batch.poolIdentityClientId`, `copyToolInstallMode`, `maxParallelTransfers`, `azure.azcopy`, `azure.storage.fileShares` and `azure.retryPolicy`, with a warning.

The directive `queue` names the pool. `cpus`, `memory`, `time` and `resourceLabels` (auto pools only) go into the task and its pool. uwe does not read the `machineType` directive, and `containerOptions` does not reach the task.

### Snakemake, CWL and Argo

Their tasks use `pool` of `[executor.azure-batch]`, or auto pools. A Snakemake rule names another pool with `resources: queue="..."`. CPUs, memory and time come from the resources of the task, as on the other executors.

## Nomad

`-executor nomad` runs each attempt of a task as one HashiCorp Nomad batch job with one task, through the Nomad HTTP API with the ACL token in `X-Nomad-Token`. uwe needs no `nomad` program. The task runs with the `docker` driver, or with `podman`.

```toml
[executor.nomad]
address = "https://nomad.example.org:4646"  # else NOMAD_ADDR, else http://127.0.0.1:4646
token = { environment = "NOMAD_TOKEN" }     # else NOMAD_TOKEN
namespace = "lab"                           # else NOMAD_NAMESPACE
# region = "eu"                             # else NOMAD_REGION
# datacenters = ["dc1"]
# node_pool = "highmem"
# driver = "podman"                         # else "docker"
# network_mode = "bridge"
# cpu_mode = "cpu"                          # MHz, 1000 per CPU; else "cores"
# device = "nvidia/gpu"                     # the device of GPUs that name no device of their own
# priority = 80                             # 1 to 100
# meta = { owner = "team-x" }
# constraints = [{ attribute = "${attr.cpu.arch}", operator = "=", value = "amd64" }]
# affinities = [{ attribute = "${node.class}", value = "fast", weight = 50 }]
# spreads = [{ attribute = "${node.datacenter}", weight = 100, targets = { dc1 = 70, dc2 = 30 } }]
# volumes = [{ type = "host", name = "scratch", path = "/shared", read_only = false }]   # or type = "csi"
# restart_attempts = 0                      # restarts on the same node
# reschedule_attempts = 1                   # placements on another node
# cleanup = "on-success"                    # purges successful jobs; or "always", "never"
# fail_unplaced = true                      # fails a job at once when no node fits it
# placement_minutes = 30                    # else how long a job may wait for a node
# poll_seconds = 1
```

With `staging = "shared-path"` (the default) the Nomad clients see the store and scratch folders at the same paths as this machine. A folder on one of `volumes` (a host or CSI volume mounted at the same path) needs nothing more; every other folder of the task is a bind mount of the driver, which needs `volumes.enabled = true` in the docker plugin of the clients. The container runs the wrapper of the task as the user and group that own the scratch folder.

With `staging = "object-store"` the jobs get their files through the staging root with presigned requests, as on the other remote executors. Nomad downloads the helper before the task starts, as an `artifact` with its SHA-256 checksum, so the image needs no download tool: from `helper_url` with `helper_sha256`, or the binary of `helper_file` (else the running binary) that uwe uploads to the staging root. With `helper_path` the job mounts the folder of the binary on the hosts read-only. The task copies the helper into `task_folder` (default `/tmp/uwe`), stages the inputs, runs the wrapper and sends the outputs back.

A task runs like this:

1. uwe registers the job `uwe-<attempt>` with `POST /v1/jobs`: the image and command of the task, its CPUs as cores (or MHz), its memory in MiB (1024 when it names none) also as `MemoryMaxMB`, its GPUs as devices, the datacenters, node pool, priority, `Meta`, constraints, affinities, spreads and volumes of the settings and of the task, and the restart and reschedule policies.
2. uwe reads the allocations of the job every `poll_seconds`. Without an allocation, it reads the latest evaluation: with `fail_unplaced`, a job that every node was filtered out for fails at once; a job that waits only for busy nodes waits on. A job without a node after `placement_minutes` fails.
3. A job ends when its task is `dead` or its allocation `complete`, `failed` or `lost`. The exit code that the wrapper recorded wins; else the exit code of the task's last `Terminated` event. A task that Nomad stopped for lack of memory gets a note. A job that could not run its task shows the end of its standard error, read through the client file system API.
4. uwe stops a job whose allocation failed, so that Nomad does not place it again after the attempt ended, and purges the job as `cleanup` says. A cancelled task stops its job.

Limits:

- Containers never run privileged: uwe sends `privileged = false` and `no-new-privileges`, and refuses `nomad.jobs.privileged = true` and `privileged` in `nomadOptions`. nf-nomad starts privileged containers by default.
- Nomad has no time limit for batch jobs, so the wrapper stops the task at its time, and uwe stops a job that runs longer than its time and `staging_minutes`.
- Tasks of several containers, tasks that run as a named user, and, with object-store staging, tasks without network and inputs at absolute paths are refused. With shared paths a task without network gets `network_mode = "none"`.
- Nextflow's secrets provider of Nomad Variables, the `slurm` and `pbs` drivers of nf-nomad and constraint closures are not supported.
- The tests run against a local fake of the Nomad HTTP API. Runs on a real Nomad cluster are not verified yet. Nomad documents the durations of restart and reschedule policies in nanoseconds, which uwe sends; nf-nomad sends milliseconds.

### Nextflow pipelines

`process.executor = 'nomad'` picks this executor. The `nomad` scope of nf-nomad fills the settings that `[executor.nomad]` leaves unset: `nomad.client.address`, `token` and `pollInterval`, and `nomad.jobs.datacenters`, `region`, `namespace`, `nodePool`, `driver`, `networkMode`, `cpuMode`, `acceleratorDeviceName`, `meta`, `constraints`, `affinities` and `spreads` (as lists of maps), `volumes`, `restartAttempts`, `rescheduleAttempts`, `cleanup` or `deleteOnCompletion`, `failOnPlacementFailure` and `placementFailureTimeout`. uwe ignores `nomad.jobs.secrets`, `dockerVolume` and `nomad.debug`, with a warning.

The `nomadOptions` of a process (from the process config) add `datacenters`, `nodePool`, `priority` (`critical` 100, `high` 80, `normal` 50, `low` 30, `min` 1, or a number), `meta`, `constraints`, `affinity`, `spread`, `volumes`, `shutdownDelay`, `failures.restart` and `failures.reschedule`, `resources.memoryMax`, `resources.cpu` or `resources.cores`, `resources.device` and `driver`. Other keys are refused before the job starts.

nf-nomad-s5cmd is not needed: with `nomad.s5cmd.enabled` and its distributed work dir (`nomad.s5cmd.workDir.enabled`, `bucket` and `prefix`), uwe stages through `s3://<bucket>/<prefix>` with its helper and serves that bucket with the S3 settings of `nomad.s5cmd.s3` (`endpoint`, `region`, `usePathStyle`, `accessKeyId`, `secretAccessKey`). The other `nomad.s5cmd` settings and the s5cmd host volume are unused, with one warning.

### Snakemake, CWL and Argo

Their tasks use the settings of `[executor.nomad]`. CPUs, memory, GPUs and time come from the resources of the task, as on the other executors.

## HTCondor

`-executor htcondor` runs each attempt of a task as one HTCondor job of the vanilla universe. uwe runs on an access point and drives the HTCondor tools: `condor_submit -terse` submits a job, `condor_q` and, for a job that left the queue, `condor_history` tell its state, and `condor_rm` removes it. With the SLURM and PBS tools, these are the only programs of a cluster that uwe starts.

```toml
[executor.htcondor]
# container = "apptainer"                   # or "singularity", "podman", "docker" on the execute nodes
# options = ['+ProjectName = "lab"']        # more lines of every submit description
# staging = "object-store"                  # with staging_root and helper_path, helper_url or helper_file
# node_folder = "/local/scratch"            # with object-store staging; else $TMPDIR, else /tmp
```

The submit description of an attempt, `job.condor` in its control folder, names the job script as `executable`, HTCondor's event log `condor.log`, one log `condor.out` for both streams, `request_cpus` with `machine_count = 1` for more than one CPU, `request_memory` in MiB, `request_disk` in KiB, `request_gpus`, and for a task with a time `periodic_remove` after that time and `staging_minutes`. Each job carries `+uwe_attempt = "<attempt>"`. The cluster options of the task come next, then `options`, which come last and so win.

With `staging = "shared-path"` (the default) the execute nodes see the store and scratch folders at the same paths, and the jobs say `should_transfer_files = IF_NEEDED`. With `staging = "object-store"` HTCondor's file transfer brings the job script to the node (`should_transfer_files = YES`), and the job stages the files of its task through the staging root with the uwe helper, as on SLURM. The helper comes from `helper_path` on the nodes, from `helper_url` with `helper_sha256`, or from `helper_file` (else the running binary), which uwe uploads.

A held job fails its attempt with HTCondor's hold reason, after uwe removed it. A cancelled task removes its job. The web proxy of the config file reaches the jobs through `environment`.

Limits:

- `staging_access = "credentials"` is refused: the jobs get no secrets.
- Tasks of several containers, daemons, image archives and image IDs are refused. Apptainer pulls the image in each job through its own cache; there are no pull jobs as on SLURM.
- uwe does not recover the jobs of a run that ended abruptly; they run on until their time.
- The tests run against fake HTCondor tools. Runs on a real pool, held jobs, file transfer and OSPool access points are not verified.

### Nextflow pipelines

`process.executor = 'condor'` and nf-ospool's `'ospool'` pick this executor. The cluster options of a process become lines of the submit description: a text is split at `;` and newlines, a list gives one line per item, as nf-ospool does. uwe keeps the submit description in the folder of the attempt and stages files itself, so it ignores `executor.$ospool.submitFileDir`, `sharedFilesystem`, `pathMappings`, `autoStageDirectories`, `stageBinDir`, `unstageOutputs` and `getenv`, with a warning for each. The `queue` directive is not used.

### Snakemake, CWL and Argo

Their tasks use the `options` of `[executor.htcondor]`. CPUs, memory, disk, GPUs and time come from the resources of the task.

## PBS

`-executor pbs` runs each attempt of a task as one job of PBS Professional or OpenPBS. uwe runs on a login node and drives `qsub`, `qstat -x -f -F json` and `qdel`. Torque, whose `qstat` gives no JSON, is not supported.

```toml
[executor.pbs]
# container = "apptainer"                   # or "singularity", "podman", "docker" on the nodes
# options = ["-A lab"]                      # more options of qsub, on its command line, so they win
# project = "ab12"                          # -P, as NCI Gadi needs it
# storage = "gdata/ab12+scratch/ab12"       # -l storage=..., as Gadi needs it
# disk_resource = "jobfs"                   # the resource that gets the disk of a task, like Gadi's jobfs
# staging = "object-store"                  # as for HTCondor
```

The job script starts with `#PBS` lines: the cluster options of the task first, then the name `uwe-<attempt>`, the log `pbs.log` in the control folder with `-j oe`, `-q` for the queue of the task, `-P` of `project`, `-l ncpus`, `-l mem` in MiB, `-l walltime` of the time of the task and `staging_minutes`, `-l ngpus`, the disk of the task as `disk_resource` and `-l storage`. A line for something that the cluster options or `options` set is left out, and a `select` statement leaves out `ncpus`, `mem` and `ngpus`. With object-store staging the job log goes to `/dev/null`, and the job sends its own log back.

A held job fails its attempt with the comment of PBS, after uwe removed it. A job that `qstat` no longer knows, on a server without job history, counts as ended, and the record of its container decides. The web proxy of the config file reaches the jobs through `qsub -v`. When the job log ends with a resource summary, as on NCI Gadi, uwe adds that summary to the end of the standard error of the task. The limits of HTCondor apply.

### Nextflow pipelines

`process.executor = 'pbspro'` picks this executor. The cluster options of a process become one `#PBS` line, as Nextflow writes them, like `-P ab12 -l storage=gdata/ab12`. The `queue` directive gives `-q`.

nf-gadi writes its usage report from the resource summaries: one CSV row or JSON entry for each task that ended or came from the cache, with the summary of its `gadi-pbs` job, in `gadi.output` with `gadi.format`, as the plugin settles them. A value that the summary lacks is an empty cell, where the plugin writes `null`. `gadi.publishTo` takes a local path; an address or a closure is not copied, with a warning. The plugin's start and end lines are not printed. The layout of a real Gadi summary was not checked; the tests use the labels that nf-gadi reads.

### Snakemake, CWL and Argo

Their tasks use the settings of `[executor.pbs]`, and the `queue` of a task. CPUs, memory, disk, GPUs and time come from the resources of the task.

## Flyte

`-executor flyte` runs each attempt of a task as one Flyte v2 run, whose root action runs a raw container: the image of the task with uwe's job command as its arguments. uwe calls Flyte's Connect API with JSON messages (`CreateRun`, `GetRunDetails` and `AbortRun` of `flyteidl2.workflow.RunService`, and `TailLogs` of `RunLogsService`), so it needs no `flyte` or `flytectl` program and no protobuf code. It works with open source Flyte 2 and with Union.

```toml
[executor.flyte]
endpoint = "tenant.hosted.unionai.cloud"    # else _U_EP_OVERRIDE, FLYTE_ADMIN_ENDPOINT, or the API key
project = "flows"                           # else FLYTE_INTERNAL_EXECUTION_PROJECT
domain = "development"                      # else FLYTE_INTERNAL_EXECUTION_DOMAIN
staging = "object-store"
staging_root = "s3://bucket/uwe"            # Flyte and the containers read it
# org = "lab"                               # else _U_ORG_NAME, or the API key
# api_key = { environment = "MY_FLYTE_KEY" }   # else _UNION_EAGER_API_KEY, EAGER_API_KEY or FLYTE_API_KEY
# queue = "gpu"                             # sent as the cluster of each run
# helper_url = "s3://bucket/tools/uwe"      # an object that Flyte downloads, with helper_sha256
# helper_path = "/opt/uwe/uwe"              # or the static uwe binary inside the task images
# helper_file = "/home/me/uwe-x86_64"       # or a binary that uwe uploads; else the running binary
# poll_seconds = 5
# task_folder = "/tmp/uwe"                  # where a run makes the folder of its attempt
```

The API key is the base64 of `endpoint:client_id:client_secret:org`, as Flyte and Union give it out. uwe asks the `AuthMetadataService` of the server for the token endpoint and the header name (like `flyte-authorization`), gets an access token with the OAuth 2.0 client-credentials grant, keeps it until a minute before its end, and gets a new one once when Flyte refuses it. Without an API key uwe sends no token, for a local Flyte without sign-in. Missing `endpoint`, `org`, `project` or `domain` stop the run before any task starts.

A task runs like this:

1. uwe uploads the inputs, the plan and, without `helper_path` and `helper_url`, the uwe binary to the staging root.
2. uwe creates the run `uwe-<24 hex digits>`, unique for each attempt, with an inline task `uwe.<attempt>` of type `raw-container`: the image, the job command as arguments (the entrypoint of the image stays), CPUs, memory, disk (as ephemeral storage) and GPUs as requests and equal limits, no retries and no Flyte caching (uwe does both), and a timeout of the time of the task and `staging_minutes`. The helper is a Blob input, which Flyte downloads before the container starts, so the image needs no download tool, only `sh`, `cp` and `sha256sum`. The job command checks the SHA-256 of the helper, stages the inputs, runs the wrapper and sends the outputs back.
3. uwe reads the run with `GetRunDetails` every `poll_seconds`, until its root action ended.
4. The exit code that the wrapper recorded wins. A run that ended without it, for example for an image that cannot be pulled, is a failure of the machine, with the message of Flyte and the end of the log from `TailLogs`.
5. A cancelled task aborts its run.

Limits:

- Object-store staging only. With an uploaded helper, Flyte reads it from the staging root, so Flyte needs read access there; the containers reach it with presigned requests.
- uwe makes one Flyte run for each attempt. nf-flyte's child actions of a Nextflow head that runs inside Flyte (`runName`, `parentAction`, `runOutputBase`) need input files in protobuf form and are not supported, nor its `authCommand`.
- Containers run unprivileged, as the user of their image. Tasks of several containers, tasks that run as a named user, tasks without network and inputs at absolute paths are refused.
- The tests run against a local fake of the Connect API and its token endpoint. Runs on a real Flyte or Union are not verified yet, nor that Flyte takes `cluster` as the queue of a run.

### Nextflow pipelines

`process.executor = 'flyte'` picks this executor. The `flyte` scope of nf-flyte fills `endpoint`, `org`, `project`, `domain`, `queue` and `pollInterval` when `[executor.flyte]` leaves them unset, and an `s3://`, `gs://` or `az://` `workDir` becomes the staging root when none is set. uwe ignores `runName`, `parentAction`, `runOutputBase`, `heartbeatTimeout`, `rootImage`, `cache` and `authCommand`, with a warning.

### Snakemake, CWL and Argo

Their tasks use the settings of `[executor.flyte]`. The image, CPUs, memory, disk, GPUs and time come from the task. A raw container needs an image: a task without one gets `-default-image`.

## Snowflake

`-executor snowflake` runs each attempt of a task as one Snowpark Container Services job service, which uwe starts with `EXECUTE JOB SERVICE` through the Snowflake SQL API (`/api/v2/statements`). uwe needs no JDBC driver and no `snowsql` or `snow` program.

```toml
[executor.snowflake]
account = "myorg-lab"                       # else a connection, or SNOWFLAKE_ACCOUNT
user = "UWE"
private_key_file = "/home/me/.snowflake/uwe.p8"   # an unencrypted PKCS #8 or PKCS #1 RSA key
database = "LAB"                            # where the job services go
schema = "JOBS"
compute_pool = "TASKS"
external_access_integrations = ["STAGING"]  # lets the jobs reach the staging root
helper_image = "/lab/images/repo/uwe:0.1.0" # an image of an image repository with the static uwe binary at /uwe
staging = "object-store"
staging_root = "s3://bucket/uwe"
# token = { environment = "MY_SNOWFLAKE_TOKEN" }   # a programmatic access token, instead of the key
# connection_name = "lab"                   # a connection of connections.toml; else SNOWFLAKE_DEFAULT_CONNECTION_NAME, else default
# role = "RUNNER"
# warehouse = "XS"
# registry_mappings = "docker.io:/lab/images/repo,quay.io:/lab/images/quay"
# helper_path = "/opt/uwe/uwe"              # or the binary inside the task images
# helper_url = "https://example.org/uwe"    # or an address that the job downloads, with helper_sha256
# helper_file = "/home/me/uwe-x86_64"       # or a binary that uwe uploads; else the running binary
# host = "myorg-lab.snowflakecomputing.com" # else <account>.snowflakecomputing.com
# poll_seconds = 5
# task_folder = "/tmp/uwe"
```

An account administrator sets up once: an image repository with the task images and the helper image, a compute pool, and an external access integration that allows the host of the staging root, for example:

```sql
CREATE NETWORK RULE staging_rule MODE = EGRESS TYPE = HOST_PORT VALUE_LIST = ('bucket.s3.eu-central-1.amazonaws.com');
CREATE EXTERNAL ACCESS INTEGRATION staging ALLOWED_NETWORK_RULES = (staging_rule) ENABLED = TRUE;
GRANT USAGE ON INTEGRATION staging TO ROLE runner;
```

uwe signs in with a key-pair JWT (`iss` is `<ACCOUNT>.<USER>.SHA256:<fingerprint of the public key>`, `sub` is `<ACCOUNT>.<USER>`, valid for 59 minutes, made for each call), with a programmatic access token, or inside Snowpark Container Services with the token of `/snowflake/session/token`. Settings that `[executor.snowflake]` and the Nextflow config leave unset come from the connection of the drivers' `connections.toml` (in `~/.snowflake` when that folder exists, else in `$SNOWFLAKE_HOME`, else in `~/.config/snowflake`: `account`, `user`, `role`, `warehouse`, `database`, `schema`, `host`, `private_key_file` or `private_key_path`, and `token`), then from `SNOWFLAKE_ACCOUNT`, `SNOWFLAKE_HOST`, `SNOWFLAKE_DATABASE`, `SNOWFLAKE_SCHEMA` and `SNOWFLAKE_WAREHOUSE`.

A task runs like this:

1. uwe uploads the inputs and the plan to the staging root.
2. uwe submits `EXECUTE JOB SERVICE IN COMPUTE POOL <pool> NAME = UWE_<24 hex digits> COMMENT = 'uwe attempt: <attempt>' EXTERNAL_ACCESS_INTEGRATIONS = (...) FROM SPECIFICATION $$...$$` asynchronously, with a request id that makes a retried submission run once, and a statement timeout of the time of the task and `staging_minutes`. The specification (JSON, which Snowflake reads as YAML) has the container `main`: the image after the registry mappings, the job command as `args` (the entrypoint of the image stays), CPUs and memory as requests, and GPUs as `nvidia.com/gpu` requests and limits. Job services have no init containers, so with `helper_image` a second container `helper` copies the binary into a local volume that both mount (`/uwe __stage install`), and the job command waits until it is there. With `helper_url`, or the binary that uwe uploads to the staging root, the job downloads the helper with `curl` or `wget` and checks its SHA-256; with `helper_path` it runs the binary of the image.
3. uwe asks about the statement every `poll_seconds`: 202 while the job runs, 200 when it ended, 422 when it failed.
4. The exit code that the wrapper recorded wins, as Snowflake reports none. A job that ended without it is a failure of the machine, with the message of Snowflake and the end of the log of `main` from `SYSTEM$GET_SERVICE_LOGS`.
5. uwe drops the job service with `DROP SERVICE IF EXISTS`. A cancelled task cancels its statement, which stops the job, and drops it.

Limits:

- Object-store staging only, through the external access integrations. nf-snowflake's `snowflake://` stage file system (a work dir on an internal stage), its trace upload to the stage and its cache store on the stage are not supported: the SQL API cannot move files to or from a stage.
- Images must be in a Snowflake image repository. `registry_mappings` replaces the first part of an image name, as nf-snowflake does: `docker.io:/lab/images/repo` turns `docker.io/library/ubuntu:24.04` into `/lab/images/repo/library/ubuntu:24.04`. An image without a registry part stays as it is. uwe pins public images to their digest before it maps them, so the copy in the repository must keep the digest.
- Encrypted private keys and passwords are not supported.
- Containers run unprivileged, as the user of their image. Tasks of several containers, tasks that run as a named user, tasks without network and inputs at absolute paths are refused.
- The tests run against a local fake of the SQL API. Runs on a real Snowflake account are not verified yet, nor that every account edition takes `EXECUTE JOB SERVICE` through the SQL API.

### Nextflow pipelines

`process.executor = 'snowflake'` picks this executor. The `snowflake` scope of nf-snowflake fills `computePool` (the variable `computePool` wins, as in the plugin), `registryMappings` and `connectionName` when `[executor.snowflake]` leaves them unset. A `snowflake://` `workDir` gets a warning, and an `s3://`, `gs://` or `az://` `workDir` becomes the staging root when none is set.

### Snakemake, CWL and Argo

Their tasks use the settings of `[executor.snowflake]`. The image, CPUs, memory, GPUs and time come from the task.

## JARVICE

`-executor jarvice` runs each attempt of a task as one JARVICE batch job (`/jarvice/batch`) through the JARVICE API, with the user name and API key of a JARVICE account. JARVICE runs a job script in the image of the task on one node of the machine type.

```toml
[executor.jarvice]
api_url = "https://cloud.nimbix.net/api"   # the default, the API of the Nimbix cloud
username = "lab"
api_key = { environment = "JARVICE_API_KEY" }
machine_type = "n1"                        # for tasks that name no machine type
vault = "persistent"                       # the vault that the jobs mount
# vault_readonly = false
# vault_force = false
# vault_path = "/data"                     # where the jobs see the vault
# poll_seconds = 10
# staging = "object-store"                 # with staging_root and helper_path, helper_url or helper_file
# task_folder = "/tmp/uwe"                 # with object-store staging: the folder of the attempts in the job
```

With `staging = "shared-path"` (the default) uwe runs in a JARVICE job itself, with its cache on the vault, as nf-jarvice needs Nextflow there: the store and scratch folders must be below `vault_path`, where the jobs see them at the same paths. The job script changes to the work folder of the task and runs its wrapper. A folder of the task outside the vault is refused before the job starts.

With `staging = "object-store"` uwe can run anywhere: the job script fetches the helper (from `helper_url` with `helper_sha256`, or the binary of `helper_file`, else the running binary, which uwe uploads; the image needs `curl` or `wget` and `sha256sum`), or runs it from `helper_path` on the vault, stages the inputs through the staging root, runs the wrapper and sends the outputs back. No vault is needed then.

A task runs like this:

1. uwe submits the job with `POST /jarvice/batch`: the image of the task (an image of Docker Hub without a namespace gets `library/`, as JARVICE asks), the job script in base64, the machine type with one node, the vault, the user, and the label `uwe-<attempt>`.
2. uwe reads `/jarvice/status` every `poll_seconds` until the job is `COMPLETED`, `COMPLETED WITH ERROR`, `CANCELED` or `TERMINATED`.
3. The exit code that the wrapper recorded is the exit code of the task. A job that ended without it shows the end of its output (`/jarvice/output`, without the header of the platform).
4. A cancelled task, or a job that runs longer than its time and `staging_minutes`, ends with `/jarvice/shutdown`, else `/jarvice/terminate`.

The machine type of a task is its `machineType` option, else its queue, else `machine_type`. CPUs and memory do not pick a machine type: JARVICE sizes jobs by machine type only.

Limits:

- The API key goes in the body of a submission and in the query of the other calls, as the JARVICE API asks, so a web proxy may log it. uwe never logs those addresses.
- Tasks of several containers and tasks that run as a named user are refused; with object-store staging also tasks without network and inputs at absolute paths. JARVICE cannot cut a job off the network.
- A job runs on one node; more nodes and a time limit of JARVICE's own are not sent.
- The tests run against a local fake of the API, after the fake of the nf-jarvice spec. Runs on a real JARVICE system are not verified, nor whether JARVICE takes the job script in base64, as nf-jarvice sends it, or as plain text, as the API documentation shows.

### Nextflow pipelines

`process.executor = 'jarvice'` picks this executor. nf-jarvice's settings of the process config fill those that `[executor.jarvice]` leaves unset: `process.apiUrl`, `machineType`, `vault` (`name`, `readonly`, `force`) and `user` (`username`, `apikey`). The `machineType` directive of a process picks the machine type of its tasks. uwe does not export the `NXF_*` variables of its own environment into the jobs, as nf-jarvice does, and prints no banner.

### Snakemake, CWL and Argo

Their tasks use the settings of `[executor.jarvice]`; the queue of a task picks its machine type.

## wr

`-executor wr` runs each attempt of a task as one job of a wr manager (the workflow runner of the Wellcome Sanger Institute), through the manager's REST API over HTTPS. wr runs the job on the machine of the manager, on LSF or on OpenStack instances that it starts. The job script is that of the SLURM jobs: it runs the task container with the container program of the nodes.

```toml
[executor.wr]
# deployment = "production"                # or "development": the folder and port of the manager
# endpoint = "https://localhost:5022"      # else https://localhost at wr's port for the user of uwe
# token_file = "/home/lab/.wr_production/client.token"   # the default
# ca_file = "/home/lab/.wr_production/ca.pem"            # the default
# poll_seconds = 1
# container = "apptainer"                  # or "singularity", "podman", "docker" on the nodes
# staging = "object-store"                 # with staging_root and helper_path, helper_url or helper_file
# node_folder = "/local/scratch"           # with object-store staging; else $TMPDIR, else /tmp
```

uwe sends the token of `token_file` as a bearer token and trusts only the CA certificate of `ca_file`, which wr made, and checks the host name of the certificate. The manager is reached without the web proxy. The default port is wr's: 1021 + 4 x the user id + 1, 2 more for `development`.

With `staging = "shared-path"` (the default) the nodes see the store and scratch folders at the same paths: the job runs `/bin/bash <job script>` in the attempt folder (`cwd_matters`), with its log in the control folder. With `staging = "object-store"` the job carries its job script, runs in a fresh folder that wr removes after the job (`on_exit` cleanup), and stages the files of its task through the staging root with the uwe helper, as on SLURM.

Each job gets the CPUs of its task, its memory in MiB, its time and `staging_minutes` in seconds and its disk in GB, with `override = 2` so that wr uses these values and not what it learned of the group, `retries = 0` (uwe retries itself), `rerun = true`, and `[uwe] <run>-<step>` as `rep_grp` and `req_grp`. uwe reads the states of its jobs in one call. A cancelled job, or one that uwe gives up on, is stopped (`state=running`) and then deleted (`state=deletable`). A job whose task failed stays in wr's queue as `buried`, where `wr status` shows it.

Limits:

- `staging_access = "credentials"` is refused: the jobs get no secrets. The web proxy of the config file does not reach the jobs.
- wr's `with_docker` and `with_singularity` job options and its S3 mounts are not used: uwe runs the container and stages the files itself.
- Tasks of several containers, daemons, image archives and image IDs are refused, as on HTCondor and PBS.
- The tests run against a local fake of the REST API over HTTPS with a throwaway CA. Runs with a real wr manager, LSF and OpenStack are not verified.

### Nextflow pipelines

`process.executor = 'wr'` picks this executor. nf-wr's `executor.$wr` scope (also read as `executor.wr`) fills the settings that `[executor.wr]` leaves unset: `deployment`, `endpoint`, `tokenpath` and `cacertpath`. uwe stages files itself, so it ignores `disableRemoteBinDir` with a warning. Unlike nf-wr, uwe checks the host name of the manager's certificate.

### Snakemake, CWL and Argo

Their tasks use the settings of `[executor.wr]`. CPUs, memory, disk and time come from the resources of the task.

## Kubernetes pod options

`pod` in `[executor.kubernetes]` gives every Job pod options in the form of Nextflow's `pod` directive, `service_account` its service account, and `context` the context of the kubeconfig:

```toml
[executor.kubernetes]
context = "lab"
service_account = "runner"
pod = [
    { nodeSelector = "disktype=ssd" },
    { toleration = { key = "gpu", operator = "Exists", effect = "NoSchedule" } },
    { env = "TOKEN", secret = "creds/token" },
    { volumeClaim = "refs", mountPath = "/refs", readOnly = true },
    { label = "team", value = "genomics" },
]
```

The options are `env` with `value`, `secret`, `config` or `fieldPath`; volumes of `secret`, `config`, `volumeClaim`, `emptyDir`, `hostPath` or `csi` with `mountPath` (and `subPath`, `readOnly`); `label` and `annotation` with `value`; `nodeSelector`, `affinity`, `toleration`; `imagePullPolicy`, `imagePullSecret`; `priorityClassName`, `schedulerName`, `runtimeClassName`; `automountServiceAccountToken`; `runAsUser` and `securityContext`; and `ttlSecondsAfterFinished`. The `pod` directive of a Nextflow process adds its options after these. Resource labels become Job labels, cleaned as Nextflow cleans them. A task's disk becomes `ephemeral-storage`, and the `type` of Nextflow's `accelerator` names the GPU resource (`amd` gives `amd.com/gpu`). uwe's own labels, its architecture selector and the security of its containers stay: `privileged`, and a `runAsUser` of 0 or another root `securityContext`, are refused with a clear message. A requested `runAsUser` or `runAsGroup` goes to every container of the pod, so the task runs as that user instead of the owner of its folder; the user then needs write access to the folder.

With `process.executor = 'k8s'` the `k8s` scope fills these settings: `k8s.namespace`, `context`, `serviceAccount`, `pod`, `imagePullPolicy`, `runAsUser`, `securityContext`, and with shared paths `storageClaimName` with `storageMountPath` as the shared volume. uwe still needs `-kubeconfig`, `kubeconfig` in `[executor.kubernetes]` or `in_cluster`; it does not read `KUBECONFIG` or `~/.kube/config`. It always runs Jobs, also with `k8s.computeResourceType = 'Pod'`, and ignores `k8s.client`, `storageSubPath` and `autoMountHostPaths` with a warning. Containers always get a CPU limit, whatever `k8s.cpuLimits` says.

## Common Workflow Scheduler on Kubernetes

`[executor.kubernetes.cws]` lets a running Common Workflow Scheduler (CWS) place the pods of the Kubernetes executor. uwe registers the run with CWS, adds the vertex of each process to its graph, and registers each task by the name of its pod once the Job made the pod, which waits for CWS meanwhile. A Job without a pod after `unschedulable_minutes` fails. The pods name CWS as their `schedulerName`. uwe does not start CWS, which runs as root by default; the cluster's administrators run it.

```toml
[executor.kubernetes.cws]
address = "http://cws.lab:8080"            # the CWS REST API, without /v1
# strategy = "FIFO"                        # the scheduling strategy, like "rank_max-fair"
# cost_function = "<name>"
# scheduler_name = "workflow-scheduler"    # the SCHEDULER_NAME of the CWS instance
```

The calls are `POST /v1/scheduler/<run>` with `dns`, `namespace`, `strategy` and `costFunction`, `POST /v1/scheduler/<run>/DAG/vertices` once for each process, and `POST /v1/scheduler/<run>/task/<n>` with the pod name as `runName`, the process, the CPUs, the memory in bytes, the work folder, the attempt as `repetition` and the bytes of the inputs as `inputSize`. The registration of the run is tried again for 150 seconds while CWS refuses connections.

With `process.executor = 'k8s'` nf-cws's `cws` scope fills these settings: `cws.dns`, `strategy` and `costFunction`. uwe tells CWS of each task by itself and ignores `cws.batchSize`, `memoryPredictor`, `minMemory`, `maxMemory`, `maxCopyTasksPerNode` and `maxWaitingCopyTasksPerNode`, and the on-demand scheduler pod of `k8s.scheduler`, with a warning.

Limits:

- The graph has no edges: the Kubernetes executor sees the tasks, not the data between them. Strategies that rank by the graph see each process alone.
- The file inputs of a task are not listed (`fileInputs` is empty), and there are no batches, memory prediction or WOW (node-local work folders with copy daemons).
- The tests run against a local fake of CWS next to the fake Kubernetes API. Runs with a real CWS are not verified.

## SLURM memory per CPU

Some clusters give each CPU a fixed amount of memory and refuse `--mem`. With `memory_per_cpu_gb` in `[executor.slurm]`, the jobs ask for no memory and instead for enough CPUs to cover the memory of the task, at that many GB (of 2^30 bytes) per CPU: a task of 2 CPUs and 10 GB gets `--cpus-per-task=3` at 4 GB per CPU. The image pull jobs do the same. The task itself keeps its CPUs and its memory limit in the container.

```toml
[executor.slurm]
memory_per_cpu_gb = 4
```

Nextflow's `process.executor = 'slurm_no_memory'` (nf-slurm-no-memory) picks the SLURM executor with `memory_per_cpu_gb` from `executor.$slurm_no_memory.memoryPerCore`, 4 when it is unset, and the account of `executor.$slurm_no_memory.account`.

## TES authentication and endpoints

Besides a bearer token (`token`, or `UWE_TES_TOKEN`) and HTTP Basic (`user`, `password`), `[executor.tes]` takes an API key, which goes in a query parameter or a header, and a timeout for each request:

```toml
[executor.tes]
api_key = { environment = "TES_API_KEY" }
api_key_name = "key"                        # the name of the parameter or header
api_key_mode = "header"                     # else "query"
timeout_seconds = 30                        # else 120
```

uwe adds the API path `/ga4gh/tes/v1` to the address of the server, unless it ends with that path or with `/v1`.

With `process.executor = 'tes'` the `tes` scope of nf-ga4gh fills these settings: `tes.endpoint` with `/v1` added, as nf-ga4gh adds it, `oauthToken`, `basicUsername` and `basicPassword`, `apiKey`, `apiKeyParamName` and `apiKeyParamMode`, `timeout` and `tags`. uwe does not read `tes.pollInterval`. TES counts memory and disk in GB of 10^9 bytes, as the TES specification says, and so does uwe; nf-ga4gh sends GiB, so the same `memory` directive asks for about 7% more there.

With `platform = "tencent-omics"` (or Nextflow's `process.executor = 'tencentcloud-omics'` of nf-tencentcloud, whose `tencentcloud.omics.endpoint` and `accessToken` name the server), tasks get the tags `runUuid` (from the environment variable `RUN_ID`), `WorkingDir` and `DestDir`, spot VMs from the resource label `preemptible = 'true'`, and the backend parameters `gpuType` and `gpuCount` from the resource labels of those names or from the GPUs of the task. Which addresses of inputs and outputs the platform accepts is not verified. `tencentcloud.metadata.enabled` writes the run and its task events to `tencentcloud.metadata.file` (default `metadata.json` in the launch directory).

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
