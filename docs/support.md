<!-- Which language versions, executors, staging modes and storage providers this revision of uwe supports. -->
<!-- Copyright (c) 2026 The uwe Contributors -->
<!-- SPDX-License-Identifier: MIT or Apache-2.0 -->

# What uwe supports

This page describes uwe 0.1.0. `uwe --version` prints the version you run. Where a row says "refused", uwe stops with exit code 33 and a message before any task starts. It never runs such a workflow with different behavior.

## Languages

| Language | Follows | Not supported |
| --- | --- | --- |
| Nextflow | DSL2 as in Nextflow 26.04 | DSL1 scripts, some directives and options (refused by `uwe check`), Nextflow's `work/` and `.nextflow` layouts |
| Snakemake | Snakemake 9.27 | Group jobs on a local run, editable, local or VCS `pip:` requirements, Python objects that cross into container helpers |
| CWL | v1.0, v1.1 and v1.2 | Direct in-place updates of a Directory, and conflicting views of one parent folder (both refused) |
| Argo Workflows | `argoproj.io/v1alpha1`, as in Argo 4 (older fields are converted as by `argo convert`) | Sidecars and daemon steps on SLURM and TES |

Each language is a subset that uwe implements in Rust. The exact limits are in [Known pitfalls](pitfalls.md). `uwe check <workflow>` answers the question for one workflow.

## Executors

| Executor | Container runtime | Staging modes |
| --- | --- | --- |
| `local` | Docker or Podman | Local folders |
| `slurm` | Apptainer (default), Singularity, Podman or Docker on the nodes | `shared-path`, `object-store` |
| `kubernetes` | The container runtime of the cluster | `shared-path`, `object-store`, `kubedal` |
| `tes` | The runtime of the TES server, for example Funnel | Server-side, or `object-store` |
| `aws-batch` | The compute environments of an AWS Batch job queue, on EC2 or Fargate | `object-store` |
| `google-batch` | The VMs of Google Batch in one project and location | `object-store` |
| `simulator` | None, outputs are placeholders | None |

Object-store staging on SLURM, Kubernetes, AWS Batch and Google Batch needs a static Linux build of uwe for the machine type of the nodes. Tasks with several containers need `shared-path`. See [Remote executors and staging](remote-executors.md).

## Storage providers

Inputs can come from S3 (and S3-compatible services), Google Cloud Storage, Azure Blob Storage, HTTPS, FTP and local folders. Outputs and the store need a provider that can be written: S3, Google Cloud Storage, Azure Blob Storage or a local folder. Any number of providers can be configured at once. Repositories come from GitHub, GitLab, Bitbucket and Gitea through their APIs, and from any Git server over HTTPS. SSH Git URLs are not supported.

`drs://<host>/<object id>` addresses read GA4GH DRS objects. uwe takes the access method of the type `access_protocol` names, else the first of type `s3`, then `gs`, else the first one, and downloads its HTTPS address. A `[storage.<name>]` table of type `drs` sets `authentication` (`gen3`, `bearer` or `none`), `credential` (a Gen3 API key or a bearer token), `token_path`, `access_protocol` and `plain_http` (servers, as `host:port`, that serve plain HTTP). Without one, uwe signs in with the Gen3 API key of the environment variable `GEN3_API_KEY`, or reads without a token. DRS objects cannot be written.

`syn://` addresses read and write Synapse. `syn://syn123` names an entity, `syn://syn123.5` its version 5, and `syn://syn123.fastq.gz` the same entity, with a suffix for file name patterns. Files download through presigned addresses. Writing `syn://syn123/a/b.txt` makes the missing folder `a` below the folder `syn123`, uploads the file in parts and makes the file entity `b.txt`, or a new version of it. A `[storage.<name>]` table of type `synapse` sets `endpoint` and `token`. Without one, uwe signs in with the token of the environment variable `SYNAPSE_AUTH_TOKEN`. Synapse folders list no files.

`irods://` addresses read and write data objects in an iRODS zone over the iRODS protocol of iRODS 4.3 and later, with TLS when the server's policy and uwe's agree. `irods:///tempZone/home/rods/a.txt` and `irods://tempZone/home/rods/a.txt` name the same data object: the server comes from the settings, never from the address. Writing makes the missing collections. A `[storage.<name>]` table of type `irods` sets `host`, `port`, `user`, `zone`, `password`, `authentication` (`native`, or `pam_password` for a PAM password) and `negotiation` (`CS_NEG_REQUIRE`, `CS_NEG_DONT_CARE` or `CS_NEG_REFUSE`, the default). Unset values come from `IRODS_HOST`, `IRODS_PORT`, `IRODS_USER_NAME`, `IRODS_ZONE_NAME` and `IRODS_PASSWORD`, then from the iRODS environment file (`IRODS_ENVIRONMENT_FILE`, else `~/.irods/irods_environment.json`). uwe does not read the obfuscated password file `.irodsA` that `iinit` writes.

`oci://` addresses read and write Oracle Cloud Object Storage through its REST API, each request signed with the API key of a user. `oci:///bucket/a.txt` and `oci://bucket/a.txt` name the same object; the namespace of the tenancy comes from the service. A `[storage.<name>]` table of type `oci` sets `profile`, `region`, `tenancy`, `user`, `fingerprint`, `private_key` (the PEM text of the key) and `endpoint`. Without the four values of the key, uwe reads the profile of `~/.oci/config` (`profile`, else `OCI_PROFILE`, `OCI_DEFAULT_PROFILE`, `DEFAULT`). The region comes from `region`, else `OCI_DEFAULT_REGION`, else the profile, else `us-phoenix-1`. Keys with a pass phrase, objects over 50 GiB, server-side copies and presigned staging are not supported, so an `oci://` address cannot be the store or a staging root.

`lakefs://<repository>/<ref>/<path>` addresses read and write lakeFS through its REST API with the access key and secret key of a user. Reads and writes move the data through the presigned addresses that lakeFS hands out, or through the API when its store gives none. A write stages the file on its branch; with `create_branches`, a missing branch is made from `branch_source` (`main` by default). A `[storage.<name>]` table of type `lakefs` sets `endpoint` (the API, ending with `/api/v1`), `access_key`, `secret_key`, `create_branches` and `branch_source`. Without one, the `lakefs` scope of a Nextflow config with nf-lakefs serves these addresses. An address of lakeFS cannot be the store or a staging root.

A Nextflow config with `cloudcache.enabled = true` (nf-cloudcache) puts uwe's store at `cloudcache.path`, else at the address of the environment variable `NXF_CLOUDCACHE_PATH`, else at `workDir`, unless `-store` names a store. A place on this machine is refused. The address needs a storage provider that can write there. The store keeps uwe's own format, not Nextflow's cache files. `uwe clean` and other commands do not read the Nextflow config, so they need `-store` with the same address.

## Hosts

uwe builds and runs on Linux and macOS, and on Windows through WSL 2. The static release build is for x86_64 Linux. Remote nodes that receive the uwe binary must run Linux.

## How it is tested

| Area | Tested by |
| --- | --- |
| Parsers, planners, runtimes, store and database | The workspace tests, on every push |
| Local Docker runs: failure, resume, cancellation, simulation | Real containers in CI, and `just container-runs` |
| Language coverage | Recorded outcomes of the nf-core, Snakemake, CWL conformance and Argo example corpora (`just` recipes, run on request) |
| S3, Google Cloud Storage, Azure | `just s3-test` against RustFS, `just emulator-test` against fake-gcs-server and Azurite |
| TES | `just tes-test` against a local Funnel server |
| Kubernetes | `just kind-staging-test` against a local kind cluster |
| SLURM | Tests with simulated SLURM tools, and use on a real cluster |
| AWS Batch | Tests against a local fake of AWS Batch and CloudWatch Logs; not yet run on AWS |
| Google Batch | Tests against a local fake of Google Batch, its token endpoint and Cloud Logging; not yet run on Google Cloud |

Not yet tested on live systems as part of a release: rootless Podman and Docker, managed Kubernetes services, cloud TES services, and recovery after a crash of the machine that runs uwe on every remote executor.
