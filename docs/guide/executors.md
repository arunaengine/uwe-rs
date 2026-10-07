<!-- Choosing executors, staging TES data and configuring registry, trace and page access. -->
<!-- Copyright (c) 2026 The iunctus Contributors -->
<!-- SPDX-License-Identifier: MIT or Apache-2.0 -->

# Executors

Select a backend with `-executor local|tes|slurm|kubernetes|simulator`.
`run` defaults to local containers. `[executor] default` can change that default.
Command-line options take precedence. Inspection commands normally plan with
the simulator. See [configuration](configuration.md) for all settings.

## Local Docker or Podman

```sh
iunctus check-machine -executor local
iunctus run main.nf -executor local
```

Tasks use the engine API and the images declared by the workflow.
Use `-default-image IMAGE` for tasks without an image.
`-max-pending N` limits concurrent pending tasks. Rule or process resources
determine each task's CPU and memory request.
See [installation](install.md) for socket selection.

## TES

TES needs a reachable GA4GH TES server and a path for inputs and outputs.
Choose shared-path staging when the iunctus host and TES workers see the store
and scratch folders at the same absolute paths:

```toml
[executor.tes]
server = "https://tes.example.org"
staging = "shared-path"
token = { environment = "TES_TOKEN" }
```

Shared-path is the default. Keep the local store and scratch on the shared
filesystem. A bearer token can also come from `IUNCTUS_TES_TOKEN`, which wins
over the file. For HTTP Basic authentication, replace `token` with both `user`
and `password` secrets. Bearer authentication takes precedence over Basic.

Choose object-store staging when workers do not share local paths:

```toml
[executor.tes]
server = "https://tes.example.org"
token = { environment = "TES_TOKEN" }
staging = "object-store"
staging_root = "s3://workflow-data/tes"
max_inputs = 512
max_outputs = 1024

[storage.staging]
type = "s3"
prefixes = ["s3://workflow-data/"]
endpoint = "https://s3.example.org"
region = "us-east-1"
path_style = true
access_key_id = { environment = "STAGING_ACCESS_KEY" }
secret_access_key = { environment = "STAGING_SECRET_KEY" }
```

iunctus uploads verified input files, submits S3 FILE locations and imports
verified task outputs into its content store. Directory results use a capture
step inside the task container. Workers must have their own access to the same
bucket and endpoint. iunctus storage credentials do not configure worker access.
The staging prefix must support reading and writing.

`idempotency_tag = "example.org/request-key"` opts into replay of an uncertain
submission. Use it only when the server deduplicates requests by that tag.
The built-in `iunctus-attempt` tag is not proof of server deduplication and cannot
be used as the configured key. Without that contract, an ambiguous submission
requires reconciliation. Funnel 0.11.14 does not provide this deduplication.

```sh
iunctus check-machine -executor tes
iunctus run main.nf -executor tes
```

TES refuses several task forms, including stdin, symbolic-link inputs, several
containers, offline tasks and GPUs. See [TES limits and qualification](../support.md#executors-and-platforms).
The service setup above was traced in code. No remote service was exercised for
this guide. Existing [executor notes](../design/executors.md#using-a-tes--s3-service)
describe TES and S3 deployment checks.

## SLURM

Start on a cluster node with `sbatch`, `squeue`, `sacct` and `scancel` on `PATH`.
Compute nodes must see the store, scratch and image folders at the same paths.
Apptainer is the default container runtime. Select another with
`-slurm-container singularity|podman|docker`.

```toml
[executor]
default = "slurm"

[cache]
folder = "/shared/iunctus/cache"

[executor.slurm]
container = "apptainer"
options = ["--account=lab", "--partition=batch"]

[executor.slurm.pull]
cpus = 2
memory_mb = 4096
minutes = 60
```

The `pull` settings size the job that acquires a SIF image.
`-slurm-options '--account=lab --partition=batch'` replaces configured options.
Run `iunctus check-machine -executor slurm` before submitting workflows.
Current live-cluster qualification remains separate from local checks.

## Kubernetes

Provide an explicit kubeconfig. iunctus never silently takes the default one.
Nodes need the same shared host paths for task staging and the store.
The backend creates Jobs, not Argo Workflow objects.

```toml
[executor.kubernetes]
kubeconfig = "/home/user/.kube/workflows.yaml"
namespace = "workflows"
```

`-kubeconfig FILE` and `-kubernetes-namespace NAME` override these values.
Without a namespace setting, the kubeconfig context's namespace is used.
Use `iunctus check-machine -executor kubernetes` before a run.
Live-cluster behavior was not exercised for this guide.

## Private registry login

Supply direct registry credentials in the iunctus config:

```toml
[registry."registry.example.org"]
username = { environment = "REGISTRY_USER" }
password = { environment = "REGISTRY_PASSWORD" }
```

Use `token` instead of the username/password pair for a bearer token.
iunctus first checks its registry settings. It then checks
`$DOCKER_CONFIG/config.json` (normally `~/.docker/config.json`),
`$XDG_RUNTIME_DIR/containers/auth.json` and `~/.config/containers/auth.json`.
Inline Docker/Podman auth and identity tokens are read.
External `credsStore` and `credHelpers` programs are not run.
Credentials needed by remote workers also need configuring at that backend.

## Telemetry

```toml
[telemetry]
endpoint = "http://localhost:4318"

[telemetry.headers]
Authorization = { environment = "OTLP_AUTHORIZATION" }
```

The header variable must contain the complete header value, such as `Bearer TOKEN`.
Runs send committed task spans in OTLP HTTP batches to `/v1/traces`.
Live export is best effort. Delivery failures are warnings, and pending spans
can be lost after the drain deadline. `iunctus telemetry RUN_ID` writes the
recorded trace to JSON lines and sends it to the configured endpoint.

## Run page

`iunctus page RUN_ID` listens on `127.0.0.1` and chooses a free port.
`-port 8080` selects a port. `-address IP` changes the bind address.
For a stable access token:

```toml
[page]
token = { environment = "IUNCTUS_PAGE_TOKEN" }
```

Tokens must be nonempty and contain only letters, digits, hyphens or underscores.
A bind beyond loopback requires a token. iunctus generates one if none is
configured and prints the URL containing it. Configured tokens also protect
loopback pages. Open the complete printed URL and keep it private.

Next: [Configuration reference](configuration.md) and [Storage reference](storage.md).
