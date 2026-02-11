<!-- Research notes on GA4GH TES and other execution backends. -->
<!-- Copyright (c) 2026 The iunctus Contributors -->
<!-- SPDX-License-Identifier: MIT or Apache-2.0 -->

# GA4GH TES and execution backends for iunctus

Research date: 2026-09-27. Facts were checked on that date through the GitHub API, the crates.io API, shallow source clones and project documentation pages. Release dates come from GitHub release metadata. Anything marked "Inference" is my own reasoning, not a sourced fact.

## 1. TES specification: version, data model, states, endpoints, views, logs, URL schemes and scope

### Takeaway
The latest official TES release is still v1.1, tagged on 2022-11-15. There is no v1.2 release. A `develop-v1.2` branch and several open pull requests exist, but none has shipped. TES describes one containerized batch task (inputs, sequential executors, outputs, resources, logs). It has no dataflow, dependencies or workflow semantics, so iunctus must keep all of that on its own side.

### Cited Findings

Versions and change process
- Releases: v0.4 (2019-09-20), v0.5 (2021-02-11), v1.0 (2021-02-12), v1.1 (2022-11-15). No later release exists. [TES releases](https://github.com/ga4gh/task-execution-schemas/releases)
- The OpenAPI file on `develop` and `main` still says `version: 1.1.0`. The last commit on `develop` is from 2026-09-24 and only updates service-info references (PR #255). [TES OpenAPI file, develop](https://github.com/ga4gh/task-execution-schemas/blob/develop/openapi/task_execution_service.openapi.yaml); [PR #255](https://github.com/ga4gh/task-execution-schemas/pull/255)
- A `develop-v1.2` branch exists. PR #230 ("fractional resource requirements for CWL 1.1 and 1.2 spec") was merged into it on 2025-03-05. On that branch `cpu_cores` becomes a floating-point number. The file there still says version 1.1.0. [PR #230](https://github.com/ga4gh/task-execution-schemas/pull/230); [develop-v1.2 branch](https://github.com/ga4gh/task-execution-schemas/tree/develop-v1.2)
- A `develop-v2.0` branch also exists. I did not inspect its content. [TES branches](https://github.com/ga4gh/task-execution-schemas/branches)
- Changes that went into 1.1 include wildcards in output paths (PR #185, merged 2022-09-21), the `PREEMPTED` state (PR #184, merged 2022-09-21) and the `CANCELING` state (PR #189, merged 2022-10-10). [PR #185](https://github.com/ga4gh/task-execution-schemas/pull/185); [PR #184](https://github.com/ga4gh/task-execution-schemas/pull/184); [PR #189](https://github.com/ga4gh/task-execution-schemas/pull/189)
- Open proposals (not merged): a `GetCapabilities` endpoint (PR #244, opened 2025-12-02), removal of `path_prefix` because it is "ambiguous" (PR #195, opened 2023-08-17), a way to report exceeded quota (PR #200, 2023-10-24, motivated by Azure tasks stuck in `INITIALIZING` for "minutes, hours, or even days"), mandatory JWT bearer auth (PR #199, 2023-10-04) and callbacks (PR #176, 2022-06-27). [PR #244](https://github.com/ga4gh/task-execution-schemas/pull/244); [PR #195](https://github.com/ga4gh/task-execution-schemas/pull/195); [PR #200](https://github.com/ga4gh/task-execution-schemas/pull/200); [PR #199](https://github.com/ga4gh/task-execution-schemas/pull/199); [PR #176](https://github.com/ga4gh/task-execution-schemas/pull/176)
- There is no CHANGELOG file in the repository. Version history has to be read from PRs and tags. [TES repository](https://github.com/ga4gh/task-execution-schemas)

Endpoints (all from the 1.1.0 OpenAPI file, base path `/ga4gh/tes/v1`) [TES OpenAPI file](https://github.com/ga4gh/task-execution-schemas/blob/develop/openapi/task_execution_service.openapi.yaml)
- `GET /service-info`: GA4GH service-info plus TES fields.
- `GET /tasks` (ListTasks): filters `name_prefix`, `state`, `tag_key`/`tag_value` (zipped arrays; an empty value matches any value), `page_size` ("Must be less than 2048. Defaults to 256"), `page_token`, `view`. Response: `tasks` and `next_page_token`.
- `POST /tasks` (CreateTask): body is a `tesTask`. Response: `{ "id": ... }`.
- `GET /tasks/{id}` (GetTask) with `view`.
- `POST /tasks/{id}:cancel` (CancelTask): response is an empty object.
- There is no delete, update, retry, log streaming or batch submit endpoint.

Views
- `MINIMAL` (the default): only `id` and `state`.
- `BASIC`: everything except `ExecutorLog.stdout`, `ExecutorLog.stderr`, `tesInput.content` and `tesTaskLog.system_logs`.
- `FULL`: all fields. [TES OpenAPI file](https://github.com/ga4gh/task-execution-schemas/blob/develop/openapi/task_execution_service.openapi.yaml)

Data model [TES OpenAPI file](https://github.com/ga4gh/task-execution-schemas/blob/develop/openapi/task_execution_service.openapi.yaml)
- `tesTask`: `id` (read only), `state` (read only), `name`, `description`, `inputs`, `outputs`, `resources`, `executors` (the only required field), `volumes`, `tags`, `logs` (read only), `creation_time` (read only, RFC 3339).
- `tesExecutor`: `image` and `command` are required. `command` is an argv array. Optional: `workdir`, `stdin`, `stdout`, `stderr` (absolute container paths), `env` (string map), `ignore_error`. Executors run "one at a time sequentially". Each may use a different image. All see the same inputs and volumes. "Execution stops on the first error" unless `ignore_error` is true.
- `tesInput`: `path` is required and must be absolute. `url` is required unless `content` is set. `content` is a UTF-8 literal; "Implementations should support a minimum of 128 KiB in this field and may define their own maximum." `type` is `FILE` or `DIRECTORY`. `streamable` is a hint that a streaming mount (for example FUSE on S3) may be used; the backend is not required to honor it.
- `tesOutput`: `path` and `url` are required. `path` may contain POSIX wildcards (IEEE Std 1003.1-2017, section 12.3). If it does, `url` must be a directory and `path_prefix` is required; the prefix is stripped from each match and the rest is appended to `url`.
- `tesFileType`: `FILE` (default) or `DIRECTORY`. The client does not have to set it, but "the server fill in the value once the information is avalible at run time".
- `tesResources`: `cpu_cores` (int32), `preemptible` (bool), `ram_gb` (double), `disk_gb` (double), `zones` (array of strings), `backend_parameters` (string to string map), `backend_parameters_strict` (bool, default false). Backend parameter keys are case insensitive. Unsupported keys "shall" produce a system warning and are not stored. With strict mode the task should fail. There is no GPU field and no wall time field.
- `volumes`: directories created empty at task start and mounted at the same path in every executor, "Essentially, this translates to a `docker run -v` flag".
- `tags`: free string map, used by the ListTasks tag filter.

Logs
- `logs` is a list of `tesTaskLog`. "Normally, this will contain only one entry, but in the case where a task fails and is retried, an entry will be appended to this list."
- `tesTaskLog`: `logs` (one `tesExecutorLog` per executor), `metadata` (free map), `start_time`, `end_time`, `outputs` (list of `tesOutputFileLog`; "Directory outputs are flattened into separate items"), `system_logs` (FULL only; for example the reason for a `SYSTEM_ERROR`).
- `tesExecutorLog`: `start_time`, `end_time`, `stdout`, `stderr`, `exit_code` (required, int32). For stdout and stderr, "No guarantees are made about the content". To capture full output the client "should set Executor.stdout to a container file path, and use Task.outputs to upload that file".
- `tesOutputFileLog`: `url`, `path`, `size_bytes` (int64 encoded as a string). [TES OpenAPI file](https://github.com/ga4gh/task-execution-schemas/blob/develop/openapi/task_execution_service.openapi.yaml)

States
- `UNKNOWN`, `QUEUED`, `INITIALIZING` (worker is preparing, for example downloading inputs), `RUNNING`, `PAUSED`, `COMPLETE` (executors succeeded and outputs uploaded), `EXECUTOR_ERROR` (an executor exited non-zero), `SYSTEM_ERROR` (for example upload failure or full disk), `CANCELED`, `CANCELING` (canceled, cleanup pending), `PREEMPTED`. [TES OpenAPI file](https://github.com/ga4gh/task-execution-schemas/blob/develop/openapi/task_execution_service.openapi.yaml)

service-info, URL schemes and auth
- `tesServiceInfo` extends GA4GH service-info v1.0.0 with `storage` ("Lists some, but not necessarily all, storage locations supported by the service", example `file:///path/to/local/funnel-storage`, `s3://ohsu-compbio-funnel/storage`) and `tesResources_backend_parameters` (supported keys). `type.artifact` is `tes`. [TES OpenAPI file](https://github.com/ga4gh/task-execution-schemas/blob/develop/openapi/task_execution_service.openapi.yaml)
- URL schemes are only given as examples: `s3://`, `gs://`, `file://` and bare absolute paths. No scheme list is normative. [TES OpenAPI file](https://github.com/ga4gh/task-execution-schemas/blob/develop/openapi/task_execution_service.openapi.yaml)
- The spec declares optional `BearerAuth` (JWT) and `BasicAuth` schemes. It recommends an OAuth2 bearer token. Authorization is left to implementations. [TES OpenAPI file](https://github.com/ga4gh/task-execution-schemas/blob/develop/openapi/task_execution_service.openapi.yaml)

Scope
- The spec describes a task as "a set of input files, a set of containers and commands to run, a set of output files and some other logging and metadata". Servers "execute them asynchronously" on HPC queues (Grid Engine, Slurm) or cloud systems (AWS Batch, Kubernetes). One stated use case is running workflow engines on new infrastructure. [TES OpenAPI file](https://github.com/ga4gh/task-execution-schemas/blob/develop/openapi/task_execution_service.openapi.yaml)
- The README examples are not consistent with the schema. The create example uses camelCase (`cpuCores`, `ramGb`, `diskGb`), while the schema uses snake_case. The FULL example shows `zones` as a string, while the schema has an array. [TES README](https://github.com/ga4gh/task-execution-schemas/blob/develop/README.md)

### Inferences
- iunctus should target TES 1.1.0 as the wire contract. Treat anything from `develop-v1.2` (fractional CPUs) or open PRs (capabilities endpoint) as optional future features.
- Because `cpu_cores` is an integer in 1.1, fractional CPU requests from CWL (`coresMin: 0.5`) must be rounded up.
- The schema has no fields for dependencies, scatter, caching, conditionals or data passing between tasks. All of this belongs to the iunctus engine. TES is only the "run this container with these files" layer.
- There is no standard way to ask a server which optional features it supports (inline content, volumes, multiple executors, wildcards, `ignore_error`). Only `storage` and `tesResources_backend_parameters` are advertised. iunctus needs per-server configuration or probing.
- Output wildcard semantics are officially under discussion (PR #195 calls `path_prefix` ambiguous). iunctus should not depend on wildcards being correct everywhere.
- Always use snake_case field names from the OpenAPI file, not the README examples.

### Gaps
- I did not inspect the `develop-v2.0` branch, so I cannot say what a TES 2.0 may contain.
- I found no GA4GH roadmap with a date for a v1.2 release.

## 2. TES implementations and their status in 2026

### Takeaway
Funnel (now at `calypr/funnel`, v0.11.14 from 2026-06-11) is the most active and most widely compatible TES server and the best choice for local tests and CI. Planetary (St. Jude, Rust, Kubernetes) and Poiesis (Kubernetes) are newer servers. TESK is older and slow moving. Microsoft's TES on Azure was archived in 2025. Aruna v3 itself now exposes a TES interface with a restricted subset of features.

### Cited Findings

Compatibility overview
- The GA4GH README has a compatibility matrix based on "limited tests performed on latest versions of implementations available in August 2026". Funnel is marked compatible with py-tes, Crankshaft, Snakemake, Sprocket, Cromwell and Nextflow, and not compatible with Toil. Poiesis is compatible with py-tes, Crankshaft, Snakemake and Nextflow, and not compatible with Sprocket, Cromwell and Toil. Planetary is compatible with py-tes, Crankshaft, Sprocket and Nextflow, and not compatible with Snakemake, Cromwell and Toil. TESK is "Unknown" for every client. [TES README](https://github.com/ga4gh/task-execution-schemas/blob/develop/README.md)
- Listed servers: Funnel, TESK, TES Azure (Archived), proTES, Poiesis, Planetary, poiesisd. Listed clients: cwl-tes (Archived), Cromwell, ELIXIR Cloud Components, py-tes, Nextflow, Snakemake, Toil, the St. Jude `tes` crate, Crankshaft, Sprocket, Pulsar, TESP API. [TES README](https://github.com/ga4gh/task-execution-schemas/blob/develop/README.md)

Funnel (OHSU, now CALYPR)
- The old repository `ohsu-comp-bio/funnel` is now a "Redirect repo for calypr/funnel". [ohsu-comp-bio/funnel](https://github.com/ohsu-comp-bio/funnel)
- `calypr/funnel`: latest release v0.11.14 (2026-06-11). Earlier 2026 releases include v0.11.12 (2026-04-27) and several release candidates. Last push 2026-09-02. [Funnel releases](https://github.com/calypr/funnel/releases)
- Install and run: `curl -sSL https://calypr.org/tools/funnel/install.sh | bash -s -- v0.11.14`, then `funnel server run`. [Funnel v0.11.14 release](https://github.com/calypr/funnel/releases/tag/v0.11.14)
- Funnel is "a toolkit for distributed, batch task execution, including a server, worker, and a set of compute, storage, and database backends". A container image is on Quay. [Funnel README](https://github.com/calypr/funnel); [Funnel image on Quay](https://quay.io/repository/ohsu-comp-bio/funnel)
- Compute backends in the source tree: `local`, `manual` (Funnel scheduler with nodes), `aws_batch`, `gcp_batch`, `gridengine`, `htcondor`, `kubernetes`, `pbs`, `slurm`, `noop`. [Funnel compute directory](https://github.com/calypr/funnel/tree/develop/compute)
- Storage backends: local, Amazon S3, generic S3, Google Cloud Storage, Swift, HTTP(S), FTP. [Funnel storage directory](https://github.com/calypr/funnel/tree/develop/storage); [Funnel storage docs](https://github.com/calypr/funnel/tree/develop/website/content/docs/storage)
- Database backends: BoltDB, Datastore, DynamoDB, Elasticsearch, MongoDB, Postgres. [Funnel database docs](https://github.com/calypr/funnel/tree/develop/website/content/docs/databases)
- For Slurm, HTCondor, Grid Engine or AWS Batch, Funnel can hand tasks to the external scheduler "without using nodes or the scheduler". [Funnel deployment docs](https://github.com/calypr/funnel/blob/develop/website/content/docs/compute/deployment.md)
- Funnel CI has a `compliance.yaml` and a `nextflow.yaml` workflow. [Funnel workflows](https://github.com/calypr/funnel/tree/develop/.github/workflows)
- Funnel is used with Gen3-Workflow deployments together with the Nextflow plugin. [nf-ga4gh issue #19](https://github.com/nextflow-io/nf-ga4gh/issues/19)

TESK (ELIXIR, Kubernetes)
- Architecture: an API pod (Java, Tomcat) turns TES requests into Kubernetes jobs. Each task gets a "taskmaster" pod, a PVC as scratch space, a "filer" pod that downloads inputs, executor pods run "one by one", and the filer uploads outputs at the end. [TESK README](https://github.com/elixir-cloud-aai/TESK)
- Requirements: Kubernetes, a default storage class (RWO is enough), and "either an FTP account or a PVC that can be accessed from within or from outside of the cluster by the workflow manager". [TESK README](https://github.com/elixir-cloud-aai/TESK)
- Releases: tesk-0.1.0 (2024-02-23) and tesk-0.1.1 (2024-08-16). Recent commits are dependency and docs updates (2026-01-26). `tesk-api` was last pushed 2025-02-28. [TESK releases](https://github.com/elixir-cloud-aai/TESK/releases); [tesk-api](https://github.com/elixir-cloud-aai/tesk-api)

Microsoft GA4GH TES on Azure
- Archived. The README says the project was retired with "Effective Date: August 4, 2025" and "no further updates, bug fixes, or support will be provided". Last releases: 6.0.0 (2025-03-07) and 6.1.0-beta (2025-03-13). [microsoft/ga4gh-tes](https://github.com/microsoft/ga4gh-tes)

Pulsar (Galaxy) and TESP API
- Pulsar 0.15.0 (2023-04-13) reworked container execution "to allow TES execution based on pydantic-tes". Latest release 0.15.15 (2026-07-13). [Pulsar HISTORY](https://github.com/galaxyproject/pulsar/blob/master/HISTORY.rst); [Pulsar releases](https://github.com/galaxyproject/pulsar/releases)
- TESP API (CESNET) is a "task execution microservice based on the TES standard that sends jobs to Galaxy Pulsar". Release 0.2.0 (2026-02-12). [CESNET/tesp-api](https://github.com/CESNET/tesp-api)

Newer servers
- Planetary (St. Jude Rust Labs): "an implementation of the GA4GH TES API that executes tasks in a Kubernetes cluster". Release v0.1.0 (2025-10-13), last push 2026-09-25. The README includes a security notice that it does not claim to be fully secure end to end. [Planetary](https://github.com/stjude-rust-labs/planetary)
- Poiesis: "TES v1.1.0 on Kubernetes". Releases v0.1.0 (2025-09-19) and v0.2.0 (2026-05-17). [Poiesis](https://github.com/JaeAeich/poiesis)
- proTES: "Proxy service for injecting middleware into GA4GH TES requests". Last push 2026-06-29. [proTES](https://github.com/elixir-cloud-aai/proTES)

Aruna v3 as a TES server
- Aruna v3 is "in public testing". It lists "Compute jobs: run analysis containers on your own infrastructure from the portal or through the standard GA4GH TES interface". [Aruna README](https://github.com/arunaengine/aruna)
- The Aruna TES route serves service-info, create, list, get and cancel. Limits: "Exactly one executor"; input and output URLs "must be `s3://bucket/key`"; at most 512 inputs and 1024 outputs; wildcard outputs require `path_prefix`; "Unsupported: directory entries, inline input content, wildcards in an input path, volumes, executor stdin/stdout/stderr redirection and resource zones". `PAUSED` and `PREEMPTED` are never emitted. [Aruna TES route](https://github.com/arunaengine/aruna/blob/main/api/src/routes/execution/tes.rs)
- Aruna compute backends in source: Docker, Apptainer and Kubernetes. [Aruna compute executors](https://github.com/arunaengine/aruna/tree/main/compute/src/executor)

Testing tools
- `elixir-cloud-aai/tes-compliance-suite`: "An application allowing the TES servers to test their conformance to TES Specifications". Last push 2026-07-24. [tes-compliance-suite](https://github.com/elixir-cloud-aai/tes-compliance-suite)
- The Rust `ga4gh-sdk` runs its integration tests against a local Funnel. [ga4gh-sdk README](https://github.com/elixir-cloud-aai/ga4gh-sdk)

### Inferences
- Production candidates in 2026: Funnel (broadest backend support and client compatibility) and, for Kubernetes, Planetary or Poiesis. TESK still works for existing ELIXIR sites, but its maintenance signal is weak. Azure TES should not be targeted.
- For local tests and CI: run Funnel (binary or Quay image) with the `local` compute backend and local storage (`file://` URLs inside `AllowedDirs`). Add MinIO (or an Aruna node) to test the S3 path. Run the compliance suite against any new server.
- Servers differ in subtle ways. Aruna allows one executor only. Poiesis rejects `ignore_error: true`. TESK needs FTP or a PVC for data exchange. iunctus must use a conservative subset by default and turn on extras per server.

### Gaps
- I did not verify, feature by feature, which TES 1.1 options Funnel supports (wildcards, `streamable`, `ignore_error`, inline `content` size limit).
- I did not verify Funnel's default HTTP port in its docs. The Nextflow plugin default endpoint is `http://localhost:8000`, which suggests port 8000, but this is indirect.
- I did not verify whether TESK's filer supports S3 in the current release. The README only mentions FTP and PVC.
- I found no public usage numbers for any TES server.

## 3. How workflow engines use TES today and what problems they report

### Takeaway
Every engine that uses TES has hit the same problems: data staging (most need a shared file system), wrong or missing output declarations (globs, directories), per-task polling, and small spec interpretation differences (first vs last task log, `ignore_error`, null exit codes). The St. Jude Rust stack (Crankshaft and Sprocket) is the most useful reference for iunctus: it polls in batches by tag and stages inputs through content-addressed object storage.

### Cited Findings

Nextflow (`nf-ga4gh` plugin)
- Latest release 1.5.0 (2026-07-03) "adds initial support for using `drs://` URIs as process inputs and outputs". Earlier 2026 releases: 1.4.0 (2026-03-26), 1.4.1 (2026-05-15), 1.4.2 (2026-06-28). [nf-ga4gh 1.5.0](https://github.com/nextflow-io/nf-ga4gh/releases/tag/1.5.0); [nf-ga4gh releases](https://github.com/nextflow-io/nf-ga4gh/releases)
- Config: `tes.endpoint` (default `http://localhost:8000`, no trailing slash allowed), `timeout` (default 10 s), `pollInterval` (default 5 s), `tags`, basic auth and API key auth. [nf-ga4gh README](https://github.com/nextflow-io/nf-ga4gh)
- README note: "While the TES API is designed to abstract workflow managers from direct storage access, Nextflow still needs to access the shared work directory used by your TES endpoint." [nf-ga4gh README](https://github.com/nextflow-io/nf-ga4gh)
- Issue #2 (open since 2022-07-17): "the GA4GH TES driver only support shared-file systems". It asks for an object-store copy strategy like the AWS Batch one. [nf-ga4gh #2](https://github.com/nextflow-io/nf-ga4gh/issues/2)
- Issue #6 (open since 2024-10-10): the plugin reads the first `TesTaskLog`, but "the last log should always be used" because servers may retry. The current handler still reads `tesTask.logs[0]`. [nf-ga4gh #6](https://github.com/nextflow-io/nf-ga4gh/issues/6); [TesTaskHandler.groovy](https://github.com/nextflow-io/nf-ga4gh/blob/main/src/main/groovy/nextflow/ga4gh/tes/executor/TesTaskHandler.groovy)
- Issue #7 (open since 2024-12-06): output mapping for a glob output (`split.*.tsv`) mixes directory and file paths. [nf-ga4gh #7](https://github.com/nextflow-io/nf-ga4gh/issues/7)
- Issue #11 (open since 2025-09-30): requests contain a file sent as `DIRECTORY` type, a regex mismatch for patterns, and null fields. [nf-ga4gh #11](https://github.com/nextflow-io/nf-ga4gh/issues/11)
- Issue #8 (open): `storeDir` does not work. Issue #14: the plugin broke on Nextflow 25.07 because an internal Nextflow method was removed (fixed 2026). [nf-ga4gh #8](https://github.com/nextflow-io/nf-ga4gh/issues/8); [nf-ga4gh #14](https://github.com/nextflow-io/nf-ga4gh/issues/14)
- Handler behavior: it polls each task with `getTask(id)` (default view), then fetches `FULL` at the end. Glob outputs are sent with `path_prefix` set to the work dir and `url` set to the task work dir. The input type is chosen by checking whether the local path is a directory. [TesTaskHandler.groovy](https://github.com/nextflow-io/nf-ga4gh/blob/main/src/main/groovy/nextflow/ga4gh/tes/executor/TesTaskHandler.groovy)
- Issue #19 (2026-04-24) asks for more maintainers. [nf-ga4gh #19](https://github.com/nextflow-io/nf-ga4gh/issues/19)

Snakemake (`snakemake-executor-plugin-tes`)
- Latest release v0.1.3 (2024-03-12). Last push 2024-09-07. Requires "Snakemake >=8.6". Settings: `--tes-url`, `--tes-user`/`--tes-password` or `--tes-token`. [plugin repository](https://github.com/snakemake/snakemake-executor-plugin-tes); [plugin catalog page](https://snakemake.github.io/snakemake-plugin-catalog/plugins/executor/tes.html)
- The plugin sets `non_local_exec=True`, `implies_no_shared_fs=True`, `job_deploy_sources=True` and `auto_deploy_default_storage_provider=True`. It sends the job script as an inline `content` input. It polls each job with `get_task(..., view="MINIMAL")`. [plugin source](https://github.com/snakemake/snakemake-executor-plugin-tes/blob/main/snakemake_executor_plugin_tes/__init__.py)
- Data: users "might either rely on a shared local filesystem or use a remote filesystem or storage", and then "have to additionally use a suitable storage plugin". [plugin catalog page](https://snakemake.github.io/snakemake-plugin-catalog/plugins/executor/tes.html)
- Issue #19 (2026-06-30): the plugin sends `"outputs": []`, so outputs are never uploaded and jobs fail with "missing files" (seen with Planetary and Azure Blob). [issue #19](https://github.com/snakemake/snakemake-executor-plugin-tes/issues/19)
- Issue #21 (2026-09-10): Funnel-specific `funnel.log` handling is embedded in the core error path. Issue #18: crash when `GITHUB_WORKSPACE` is missing. Issue #15: all files must be in the same working directory. PR #16 (update to py-tes 1.x) is open since 2024-12-09. Issue #3 "Implement native TES executor" is open since 2023. [plugin issues](https://github.com/snakemake/snakemake-executor-plugin-tes/issues)

cwl-tes
- Archived. Last release 0.3.0 (2019-11-25). The GA4GH README marks it Archived. [cwl-tes](https://github.com/ohsu-comp-bio/cwl-tes); [TES README](https://github.com/ga4gh/task-execution-schemas/blob/develop/README.md)

Toil
- Toil core batch systems are: single machine, Slurm, LSF, Grid Engine, HTCondor, Torque, Kubernetes, AWS Batch, Mesos. There is no TES batch system in core. [Toil batchSystems](https://github.com/DataBiosphere/toil/tree/master/src/toil/batchSystems)
- TES support lives in an external plugin, `adamnovak/toil_batch_system_tes` (last push 2026-08-26). Issue #7 "TES 1.1 and Compliance Overall Issue" (2026-08-19) collects its 1.1 gaps; all listed sub-issues are ticked. The GA4GH matrix still says "Not Compatible" for Funnel, Poiesis and Planetary. [toil_batch_system_tes #7](https://github.com/adamnovak/toil_batch_system_tes/issues/7); [TES README](https://github.com/ga4gh/task-execution-schemas/blob/develop/README.md)
- Toil latest release 9.5.0 (2026-06-02). Intermediate files go through a "job store" with file, AWS and Google implementations. [Toil releases](https://github.com/DataBiosphere/toil/releases); [Toil jobStores](https://github.com/DataBiosphere/toil/tree/master/src/toil/jobStores)

Cromwell
- "Currently this backend only works with files on a Local or Shared File System." Cromwell uses "an exponential backoff strategy to control the rate of requests sent to the TES server". Runtime attributes: `docker`, `dockerWorkingDir`, `cpu`, `memory`, `disk`, `disks` (limited: no custom mount points, no multiple disks), `preemptible`. TESK users should switch glob handling to symlinks instead of hard links. [Cromwell TES backend](https://cromwell.readthedocs.io/en/stable/backends/TES/)
- Latest Cromwell release: 92 (2026-01-30). [Cromwell releases](https://github.com/broadinstitute/cromwell/releases)

Crankshaft and Sprocket (St. Jude, Rust)
- Crankshaft 0.11.0 and crankshaft-engine 0.11.0 released 2026-08-26. Sprocket 0.31.0 released 2026-09-16. [crankshaft releases](https://github.com/stjude-rust-labs/crankshaft/releases); [sprocket releases](https://github.com/stjude-rust-labs/sprocket/releases)
- The Crankshaft TES monitor tags every task with `crankshaft-task-group` and polls all of them with one ListTasks call filtered by that tag, using `MAX_PAGE_SIZE - 1` and view `MINIMAL` (or `BASIC` when resource usage is wanted). [monitor.rs](https://github.com/stjude-rust-labs/crankshaft/blob/main/crankshaft-engine/src/service/runner/backend/tes/monitor.rs); [crankshaft #58](https://github.com/stjude-rust-labs/crankshaft/issues/58)
- The monitor reads resource usage from `TaskLog.metadata` keys such as `peak_memory_bytes`, `cpu_time_ms` and `disk_used_bytes`. These keys are a convention, not part of TES. [monitor.rs](https://github.com/stjude-rust-labs/crankshaft/blob/main/crankshaft-engine/src/service/runner/backend/tes/monitor.rs)
- Reported problems: #81 (open, 2026-07-08) `ignore_error` is hard-coded to `true`, so Poiesis returns 400 ("Kubernetes init containers abort on first failure"); #79 deserialization failed when earlier retry logs contain a null `exit_code`; #61 `EXECUTOR_ERROR` was treated as success; #37 wrong resource sizes; #63 and #60 polling fixes; #41 configurable client retries; #38 bearer auth. [crankshaft #81](https://github.com/stjude-rust-labs/crankshaft/issues/81); [crankshaft issues](https://github.com/stjude-rust-labs/crankshaft/issues?q=is%3Aissue+TES)
- Sprocket TES staging: inputs are uploaded to cloud storage under `file/<hex-digest>` or `directory/<hex-digest>` using a Blake3 digest; remote URLs are passed through; outputs go to "a unique prefix for the task's execution" and are downloaded only when a later step needs to read them. GPUs are not supported on this backend. The cache "assumes cached objects match local inputs without verification". Supported stores: Azure Blob, S3, Google Cloud Storage. [Sprocket TES backend docs](https://sprocket.bio/reference/backends/tes)

### Inferences
- Common failure themes, in order of how often they appear:
  1. Data staging. Nextflow and Cromwell still need a shared file system. Snakemake pushes staging into the container via storage plugins. Only Sprocket has a clean object-store path.
  2. Output declarations. Empty outputs (Snakemake #19), dir vs file confusion and glob mapping bugs (Nextflow #7, #11). The wildcard plus `path_prefix` design is easy to get wrong on both sides.
  3. Spec interpretation. First vs last task log, `ignore_error` defaults, null exit codes in retry logs, camelCase vs snake_case.
  4. Polling load. Per-task GET every few seconds (Nextflow, Snakemake) scales badly. Crankshaft moved to one tag-filtered ListTasks per interval. Cromwell uses exponential backoff.
  5. Plugin decay. The Snakemake plugin has had no release since 2024. The Nextflow plugin broke on an internal Nextflow change and asks for maintainers. cwl-tes is archived.
- None of these engines report cache or resume features at the TES level. Resume is done by the engine (Nextflow work dir hashes, Sprocket content digests). iunctus must own caching too.
- Overhead for many small tasks: each TES task means container start, input download and output upload on the server side. No source measured this, but the design implies high per-task latency. Grouping small steps into one task (several commands in one script) is the usual way out.

### Gaps
- I found no published benchmark of TES overhead per task or of polling load on servers.
- I did not check Cromwell's GitHub issues for TES-specific problems.
- I did not read the Toil TES plugin sub-issues one by one.

## 4. Data staging: how intermediate files move between tasks, and how Aruna could fit

### Takeaway
TES assumes that the server pulls inputs from URLs and pushes outputs to URLs. The engine must choose where intermediate files live. Shared file systems are simple but tie the engine to the cluster. Object storage (S3 compatible) works everywhere and allows content-addressed deduplication. Aruna fits well as the object store, and even as a TES server, but its TES subset is narrow.

### Cited Findings
- TES inputs "will be downloaded and mounted into the executor container". Outputs "will be uploaded from the executor container to long-term storage". [TES OpenAPI file](https://github.com/ga4gh/task-execution-schemas/blob/develop/openapi/task_execution_service.openapi.yaml)
- `streamable` lets a client say that a FUSE-style streaming mount may be faster than a full download. It is only a hint. [TES OpenAPI file](https://github.com/ga4gh/task-execution-schemas/blob/develop/openapi/task_execution_service.openapi.yaml)
- Shared file system approach: Cromwell TES "only works with files on a Local or Shared File System". Nextflow "still needs to access the shared work directory used by your TES endpoint". [Cromwell TES backend](https://cromwell.readthedocs.io/en/stable/backends/TES/); [nf-ga4gh README](https://github.com/nextflow-io/nf-ga4gh)
- Funnel local storage: files must be absolute `file:///` URLs inside `AllowedDirs`. Funnel tries to hard link instead of copying; this fails across file systems (for example NFS to local scratch). [Funnel local storage docs](https://github.com/calypr/funnel/blob/develop/website/content/docs/storage/local.md)
- Funnel S3: Amazon S3 is enabled by default with `MaxRetries` and SSE options. "Other S3 API Providers" (Ceph, Cleversafe, MinIO) are supported but "NOT enabled by default". [Funnel S3 docs](https://github.com/calypr/funnel/blob/develop/website/content/docs/storage/s3.md)
- Funnel HTTP storage: "Funnel supports downloading files from public URLs via GET requests. No authentication mechanism is allowed. This backend can be used to fetch objects from cloud storage providers exposed using presigned URLs." [Funnel HTTP storage docs](https://github.com/calypr/funnel/blob/develop/website/content/docs/storage/http.md)
- TESK moves data through a per-task PVC and needs FTP or a shared PVC to exchange files with the workflow manager. [TESK README](https://github.com/elixir-cloud-aai/TESK)
- Content-addressed staging: Sprocket uploads local inputs to `file/<hex-digest>` or `directory/<hex-digest>` (Blake3) and writes outputs to a unique prefix per execution. It downloads outputs only when the workflow needs to read them. [Sprocket TES backend docs](https://sprocket.bio/reference/backends/tes)
- Snakemake over TES uses storage plugins, and the default storage provider is deployed into the job. [plugin source](https://github.com/snakemake/snakemake-executor-plugin-tes/blob/main/snakemake_executor_plugin_tes/__init__.py)
- Toil keeps intermediate files in a job store (file, AWS, Google). [Toil jobStores](https://github.com/DataBiosphere/toil/tree/master/src/toil/jobStores)
- Nextflow's TES plugin was asked (issue #2) to copy the AWS Batch file copy strategy for object storage. [nf-ga4gh #2](https://github.com/nextflow-io/nf-ga4gh/issues/2)

Aruna
- Every Aruna node "offers an S3-compatible interface, so common tools, scripts and workflow systems work without changes". Datasets are described with RO-Crate. [Aruna README](https://github.com/arunaengine/aruna)
- Aruna TES facade: a realm task "reads every input from the bucket its `s3://` url names, at the version resolved when the task was accepted, so a later write to the source key never changes what a running task reads". Output URLs in the task log "name the exact `versionId`". [Aruna TES route](https://github.com/arunaengine/aruna/blob/main/api/src/routes/execution/tes.rs)
- An `aruna-engine.org/idempotency-key` tag deduplicates submissions per caller. Reusing a key for a different task returns 409. [Aruna TES route](https://github.com/arunaengine/aruna/blob/main/api/src/routes/execution/tes.rs)
- `EXECUTOR_ERROR` means a permanent job failure without retry. `SYSTEM_ERROR` means an infrastructure error that Aruna re-plans. [Aruna TES route](https://github.com/arunaengine/aruna/blob/main/api/src/routes/execution/tes.rs)
- Auth: realm bearer token, or HTTP Basic with an access key and secret issued by the node. The caller needs WRITE on the target group. [Aruna TES route](https://github.com/arunaengine/aruna/blob/main/api/src/routes/execution/tes.rs)
- Aruna v2 remains on a separate branch. Its last tagged release is v2.0.4 (2025-04-29). v3 is in public testing. [Aruna README](https://github.com/arunaengine/aruna); [Aruna releases](https://github.com/arunaengine/aruna/releases)

### Inferences
- Default for iunctus: keep intermediate data in an S3-compatible store (Aruna, MinIO or cloud S3). Use shared file systems only for local and HPC executors where they are natural.
- Use a content-addressed layout for inputs and a run-scoped layout for outputs, as Sprocket does. Content addressing gives free deduplication and a natural cache key. Run-scoped output prefixes avoid races between retries.
- Do not pull intermediate files back to the engine unless the engine must read them. Examples where it must: CWL `outputEval` and `loadContents`, Nextflow operators that read file contents, Snakemake checkpoints. Everything else can stay as URLs.
- Presigned URLs help with input access for servers that have no S3 credentials (Funnel HTTP storage), but they do not solve outputs in a portable way. Funnel's HTTP storage is GET only. So credentials for writing outputs must be configured on the TES server side.
- Aruna's version pinning at acceptance time is exactly the snapshot semantics a workflow engine wants for reproducibility and caching. Output `versionId`s in the task log can feed provenance directly.
- Aruna's TES subset forces a portable task shape: one executor, a wrapper script that does its own stdout and stderr redirection, and no inline content. If iunctus uses this shape by default, the same task works on Funnel, Planetary, Poiesis and Aruna.
- Many small files are expensive on object stores (one request per object). For tasks with thousands of small outputs, packing them (tar) or declaring a directory output is cheaper than one output entry per file. Aruna's limit of 1024 outputs per task makes this a hard requirement there.

### Gaps
- I did not find whether Aruna v3 issues presigned URLs. A search for "presign" in the Aruna source found nothing.
- I found no measurements of S3 vs shared file system staging costs for workflow tasks.
- I did not confirm which S3 features (multipart upload, versioning semantics, listing) the Aruna S3 interface supports in v3.

## 5. Rust TES clients

### Takeaway
The `tes` crate from St. Jude Rust Labs (v0.10.1, 2026-07-22) is the only mature Rust option. It has types and an async client covering all five endpoints, with retries and auth, and it is used in production by Crankshaft and Sprocket. `ga4gh-sdk` from ELIXIR is not published on crates.io and its README still says "TBA" for usage.

### Cited Findings
- `tes` crate: max version 0.10.1, updated 2026-07-22, created 2024-09-05, about 158,000 total downloads and about 39,600 recent downloads. [crates.io tes](https://crates.io/crates/tes)
- Releases: v0.8.0 (2025-06-04), v0.9.0 (2025-08-20), v0.10.0 (2026-01-08), v0.10.1 (2026-07-22). [tes releases](https://github.com/stjude-rust-labs/tes/releases)
- Features: `types` (default), `client` (reqwest, tokio-retry2, tracing), `oauth`, `serde`, `ord`. Edition 2024, `rust-version = "1.88.0"`. Dual licensed MIT and Apache 2.0. [tes Cargo.toml](https://github.com/stjude-rust-labs/tes/blob/main/Cargo.toml); [tes README](https://github.com/stjude-rust-labs/tes)
- Client methods: `service_info`, `list_tasks`, `create_task`, `get_task`, `cancel_task`. [tes client.rs](https://github.com/stjude-rust-labs/tes/blob/main/src/v1/client.rs); [docs.rs tes](https://docs.rs/tes)
- Crankshaft uses `tes::v1::types` (for example `ListTasksParams`, `View`, `State`, `MAX_PAGE_SIZE`). [monitor.rs](https://github.com/stjude-rust-labs/crankshaft/blob/main/crankshaft-engine/src/service/runner/backend/tes/monitor.rs)
- Crankshaft added basic auth (#6), bearer auth (#38) and configurable client retries (#41) on top of the crate. [crankshaft issues](https://github.com/stjude-rust-labs/crankshaft/issues?q=is%3Aissue+TES)
- `ga4gh-sdk` (elixir-cloud-aai): workspace version 0.1.0, crates `ga4gh-lib` and `ga4gh-cli`. It is not on crates.io (both `ga4gh-sdk` and `ga4gh_sdk` return not found). README: "Basic usage TBA", "Installation TBA". Models are generated from OpenAPI. A WES client was added on 2026-09-24. Integration tests use Funnel. [ga4gh-sdk](https://github.com/elixir-cloud-aai/ga4gh-sdk); [crates.io search result for ga4gh-sdk](https://crates.io/crates/ga4gh-sdk)
- `crankshaft` crate: 0.11.0, updated 2026-08-26, about 122,000 downloads. [crates.io crankshaft](https://crates.io/crates/crankshaft)
- The TES OpenAPI file is OpenAPI 3.0.1 and pulls `Service` and `ServiceType` from an external URL (`raw.githubusercontent.com/ga4gh/ga4gh-service-info/v1.0.0/service-info.yaml`). [TES OpenAPI file](https://github.com/ga4gh/task-execution-schemas/blob/develop/openapi/task_execution_service.openapi.yaml)

### Inferences
- iunctus should depend on `tes` with the `client` feature. It is small, typed and battle tested against Funnel and Planetary. Its release cadence (a few per year) matches the frozen spec.
- If iunctus needs features the crate does not expose (for example raw JSON passthrough of unknown fields), wrapping `reqwest` directly with the crate's `types` is a cheap fallback.
- OpenAPI code generation is possible, but the external `$ref` to service-info needs to be resolved at generation time. Given the tiny API surface (five calls), hand-written or crate types are simpler.
- `ga4gh-sdk` may become useful later for WES, TRS and DRS clients, but it is not ready as a dependency today.

### Gaps
- I did not check whether the `tes` crate keeps unknown JSON fields (for example the Aruna derived tags) or whether it tolerates non-standard values.
- I did not check the `tes` crate's handling of the `develop-v1.2` changes.

## 6. Related GA4GH standards and provenance

### Takeaway
WES 1.1.0 (2023) is the standard front door if iunctus should accept workflow submissions later. TRS 2.0.2 (2025) is the standard way to fetch workflows from registries. DRS 1.5.0 (2025) resolves `drs://` data IDs. Workflow Run RO-Crate 0.6.0 (2026-09-07) is the provenance format that matches Aruna's RO-Crate based metadata.

### Cited Findings
- WES: latest release 1.1.0 (2023-09-14); 1.0.1 (2023-01-10); 1.0.0 (2018-11-19). [WES releases](https://github.com/ga4gh/workflow-execution-service-schemas/releases)
- WES 1.1.0 task logs have an optional `tes_uri`: "An optional URL pointing to an extended task definition defined by a TES api". [WES 1.1.0 OpenAPI](https://github.com/ga4gh/workflow-execution-service-schemas/blob/1.1.0/openapi/workflow_execution_service.openapi.yaml)
- nf-ga4gh issue #12 (open) asks to expose WES task logs through TES URIs. [nf-ga4gh #12](https://github.com/nextflow-io/nf-ga4gh/issues/12)
- DRS: latest release 1.5.0 (2025-03-25). It adds fields for cold storage, explicit cloud locations, and object count and size metadata, plus guidance for using GA4GH Data Connect with DRS. [DRS 1.5.0](https://github.com/ga4gh/data-repository-service-schemas/releases/tag/drs-1.5.0)
- nf-ga4gh 1.5.0 resolves `drs://` URIs as inputs and outputs, with bearer or basic auth. [nf-ga4gh 1.5.0](https://github.com/nextflow-io/nf-ga4gh/releases/tag/1.5.0)
- TRS: latest release 2.0.2 (2025-09-02), a "maintenance release" with "tag-based search, descriptions for specific workflow versions, a fix to the zip return type". Previous 2.0.1 (2022-09-14). [TRS 2.0.2](https://github.com/ga4gh/tool-registry-service-schemas/releases/tag/2.0.2)
- service-info: TES 1.1 builds on GA4GH service-info v1.0.0. [TES OpenAPI file](https://github.com/ga4gh/task-execution-schemas/blob/develop/openapi/task_execution_service.openapi.yaml)
- Workflow Run RO-Crate: "an extension of RO-Crate ... to capture the provenance of the execution of computational workflows at different levels of granularity". Release 0.6.0 (2026-09-07) includes "Profile updates for RO-Crate 1.3". Release 0.5.0 was 2024-06-19. [Workflow Run RO-Crate](https://www.researchobject.org/workflow-run-crate/); [WRROC 0.6.0](https://github.com/ResearchObject/workflow-run-crate/releases/tag/0.6.0)
- Listed implementations include Galaxy (23.1.1+), runcrate (0.5.0+), COMPSs (3.4+), StreamFlow, WfExS (0.10.1+), Sapporo (1.5.1+), Autosubmit and Nextflow via nf-prov (1.4.0+). [Workflow Run RO-Crate](https://www.researchobject.org/workflow-run-crate/)
- Aruna describes datasets with RO-Crate and links crates to "the analyses that produced them". [Aruna README](https://github.com/arunaengine/aruna)

### Inferences
- A later iunctus WES endpoint can link each WES task log to the TES task via `tes_uri`. This costs little if iunctus stores TES task URLs from the start.
- TRS is the right way to fetch workflows from Dockstore or WorkflowHub by ID and version. It fits a future `iunctus run trs://...` style input.
- DRS support can be a staging resolver: turn `drs://` into an access URL (often presigned) before building the TES input.
- Emitting a Workflow Run RO-Crate (Provenance Run Crate profile) per run, and storing it next to outputs in Aruna, closes the loop between execution and FAIR metadata.

### Gaps
- The Workflow Run RO-Crate page did not list the three profile names and versions directly in the fetched content. I did not confirm the exact 0.6.0 profile list.
- I did not verify, in this session, which registries (Dockstore, WorkflowHub) implement TRS 2.0.2.
- I did not check the ga4gh-discovery service-info repository for a version newer than 1.0.0.

## 7. Executor abstractions in other engines and future executors

### Takeaway
All engines split execution into the same few operations: submit, check status (ideally in batches), cancel, collect result and logs. They differ mainly in who stages data and whether a shared file system is assumed. Snakemake makes those assumptions explicit as flags. Aruna's own `ExecutorBackend` trait is the richest model and adds idempotent submit, reconcile after restart, output listing and cleanup.

### Cited Findings

Nextflow
- `Executor` (abstract): `createTaskMonitor()`, `createTaskHandler(task)`, plus flags such as `isContainerNative()`, `isStageFileEnabled()`, `isFusionEnabled()`, `isSecretNative()`, and work, bin and stage directory helpers. [Executor.groovy](https://github.com/nextflow-io/nextflow/blob/master/modules/nextflow/src/main/groovy/nextflow/executor/Executor.groovy)
- `TaskHandler` (abstract): `submit()`, `checkIfRunning()`, `checkIfCompleted()`, `killTask()`, with states NEW, SUBMITTED, RUNNING, COMPLETED. [TaskHandler.groovy](https://github.com/nextflow-io/nextflow/blob/master/modules/nextflow/src/main/groovy/nextflow/processor/TaskHandler.groovy)
- Built-in executors documented in 2026: `local`, `awsbatch`, `azurebatch`, `google-batch`, `k8s`, `seqera`, `bridge`, `flux`, `condor`, `hq`, `lsf`, `moab`, `nqsii`, `oar`, `pbs`, `pbspro`, `sge`, `slurm`, `tcs`. TES is provided by the separate plugin. [Nextflow executors](https://docs.seqera.io/nextflow/executor); [nf-ga4gh](https://github.com/nextflow-io/nf-ga4gh)

Snakemake
- `RemoteExecutor`: `run_job` (per job), async `check_active_jobs(active_jobs)` which yields jobs still running, `cancel_jobs(active_jobs)`, `shutdown()`, and a status check interval (`next_seconds_between_status_checks`). [remote.py](https://github.com/snakemake/snakemake-interface-executor-plugins/blob/main/snakemake_interface_executor_plugins/executors/remote.py)
- `CommonSettings` flags: `non_local_exec`, `implies_no_shared_fs`, `job_deploy_sources`, `pass_default_storage_provider_args`, `pass_default_resources_args`, `pass_envvar_declarations_to_cmd`, `auto_deploy_default_storage_provider`, `init_seconds_before_status_checks`, `pass_group_args`, `spawned_jobs_assume_shared_fs`, `can_transfer_local_files`. [settings.py](https://github.com/snakemake/snakemake-interface-executor-plugins/blob/main/snakemake_interface_executor_plugins/settings.py)

Toil
- `AbstractBatchSystem`: `issueBatchJob`, `killBatchJobs`, `getIssuedBatchJobIDs`, `getRunningBatchJobIDs`, `getUpdatedBatchJob(maxWait)`, `getSchedulingStatusMessage`, `shutdown`, `setEnv`, plus class flags `supportsAutoDeployment` and `supportsWorkerCleanup`. [abstractBatchSystem.py](https://github.com/DataBiosphere/toil/blob/master/src/toil/batchSystems/abstractBatchSystem.py)

Crankshaft
- `Backend` trait: `default_name()` and `run(task, events, cancellation_token)`, which returns a future of `NonEmpty<ExecutionResult>` or `TaskRunError` (`Canceled`, `Preempted`, `Other`). Backends: `docker`, `generic` (shell or SSH driver, used for HPC schedulers) and `tes`. [backend.rs](https://github.com/stjude-rust-labs/crankshaft/blob/main/crankshaft-engine/src/service/runner/backend.rs); [crankshaft backends](https://github.com/stjude-rust-labs/crankshaft/tree/main/crankshaft-engine/src/service/runner/backend)
- Crankshaft `Task`: name, description, inputs, outputs, resources, `executions` (non-empty list), volumes. Input contents can be a URL, literal bytes or a host path. `Execution` allows several fallback images. [task.rs](https://github.com/stjude-rust-labs/crankshaft/blob/main/crankshaft-engine/src/task.rs); [input contents](https://github.com/stjude-rust-labs/crankshaft/blob/main/crankshaft-engine/src/task/input/contents.rs); [execution.rs](https://github.com/stjude-rust-labs/crankshaft/blob/main/crankshaft-engine/src/task/execution.rs)

Aruna
- `ExecutorBackend` trait: `kind`, `capabilities`, `run_identity`, `health`, `resolve_image`, `fence`, `submit` ("Idempotent under the deterministic attempt name: a name collision MUST return the existing attempt's status, never start a second run"), `stage`, `unsuspend`, `status`, `wait` (default polls `status()`, backends "override with native waits"), `cancel`, `fetch_logs` ("Bounded tail per stream"), `fetch_output`, `list_outputs` ("Zero matches is an empty list, never an error"), `open_session`, `reconcile` ("Query by deterministic name after restart / lease loss. Never mutates"), `tombstone`, `cleanup` ("Called only after terminal evidence is durably recorded"). [Aruna executor trait](https://github.com/arunaengine/aruna/blob/main/compute/src/executor/mod.rs)
- Implementations: Docker, Apptainer, Kubernetes. [Aruna compute executors](https://github.com/arunaengine/aruna/tree/main/compute/src/executor)

Funnel
- Funnel itself covers local, Slurm, PBS/Torque, Grid Engine, HTCondor, Kubernetes, AWS Batch and GCP Batch behind one TES API. [Funnel compute directory](https://github.com/calypr/funnel/tree/develop/compute)

### Inferences
- The shared minimum across Nextflow, Snakemake, Toil and Crankshaft is: submit, status, cancel, result with exit code. Everything else (staging, logs, resources) is handled by flags or by the engine.
- Batched status checks matter. Snakemake passes all active jobs to one call. Toil asks for "updated" jobs. Crankshaft uses one ListTasks per interval. HPC schedulers also prefer one `squeue` over many. The trait should accept a set of handles, not one handle.
- Explicit capability flags (Snakemake style) avoid silent failures. The TES servers differ enough (Section 2) that iunctus needs the same idea per executor.
- Idempotent submit with a deterministic name plus a `reconcile` call (Aruna style) is what makes engine restart and resume safe. TES has no idempotency key, but tags plus a ListTasks filter can emulate reconcile. Aruna offers a real idempotency tag.
- Future executors map as follows: local process and Docker/Podman (the engine does staging on the local file system); Slurm, PBS and LSF (script plus shared file system, batch status queries); Kubernetes, AWS Batch, Google Batch and Azure Batch (container plus object storage, native job APIs). TES can reach all of them through Funnel, so a native backend is only needed when TES overhead or feature gaps hurt.

### Gaps
- I did not read Nextflow's `TaskPollingMonitor` or the grid executor batch `queueStatus` code in this session.
- I did not read the Snakemake base `Executor.run_job` signature directly; only the remote executor subclass.
- I did not study AWS Batch, Google Batch or Azure Batch APIs for this note.

## 8. Recommendations for iunctus

### Takeaway
Map each iunctus task to one conservative TES task: one executor, a staged wrapper script, explicit outputs plus a manifest, snake_case fields, and tags for run, task and attempt. Keep intermediate data in S3-compatible storage (Aruna first) with a content-addressed input layout and run-scoped outputs. Define one async executor trait around batched status, capabilities and staging hooks, so TES, local, HPC and cloud backends can plug in later.

### Cited Findings
- Aruna rejects more than one executor, inline content, volumes and stdout/stderr redirection. [Aruna TES route](https://github.com/arunaengine/aruna/blob/main/api/src/routes/execution/tes.rs)
- Poiesis rejects `ignore_error: true`. [crankshaft #81](https://github.com/stjude-rust-labs/crankshaft/issues/81)
- The last task log is authoritative when a server retries. Retry logs may contain null exit codes. [TES OpenAPI file](https://github.com/ga4gh/task-execution-schemas/blob/develop/openapi/task_execution_service.openapi.yaml); [nf-ga4gh #6](https://github.com/nextflow-io/nf-ga4gh/issues/6); [crankshaft #79](https://github.com/stjude-rust-labs/crankshaft/issues/79)
- Executor stdout and stderr in logs have no content guarantees. Full logs require output files. [TES OpenAPI file](https://github.com/ga4gh/task-execution-schemas/blob/develop/openapi/task_execution_service.openapi.yaml)
- Batch polling by tag works in practice. [monitor.rs](https://github.com/stjude-rust-labs/crankshaft/blob/main/crankshaft-engine/src/service/runner/backend/tes/monitor.rs)
- Content-addressed input staging works in practice. [Sprocket TES backend docs](https://sprocket.bio/reference/backends/tes)
- Idempotent submit and reconcile are part of Aruna's executor trait. [Aruna executor trait](https://github.com/arunaengine/aruna/blob/main/compute/src/executor/mod.rs)

### Inferences

How to map an iunctus task to TES (default "portable" profile)
- One iunctus task becomes one `tesTask` with exactly one `tesExecutor`. Omit `ignore_error` (do not send `true`).
- The engine renders the tool command (CWL `baseCommand` plus arguments, Nextflow `script`, Snakemake `shell`) into a wrapper script. The wrapper does `cd` to the work dir, sets up environment, redirects stdout and stderr to files, runs the command, writes the exit code and an output manifest, and exits with the tool's code.
- Stage the wrapper as a normal input object (`s3://.../task.sh`), because Aruna does not accept inline `content`. Use inline `content` (up to 128 KiB) only when the server's profile allows it, which saves one upload.
- `command` is `["/bin/sh", "/iunctus/task.sh"]`. Set `workdir` to a fixed container path such as `/iunctus/work`. Do not rely on image defaults.
- Inputs: one `tesInput` per file with `url` and absolute `path`. Set `type` explicitly (`FILE` or `DIRECTORY`), since engines that guess get it wrong (nf-ga4gh #11). Set `streamable` only when the tool reads sequentially.
- Outputs: declare every known output file explicitly. Always add the stdout file, the stderr file, the exit code file and the manifest. For globs, prefer a directory output of a dedicated output folder, or have the wrapper pack matches, over TES wildcards. Use wildcards plus `path_prefix` only on servers where tests pass. After completion, read `tesOutputFileLog` (BASIC view) to learn the real files and sizes.
- Resources: `cpu_cores` rounded up to an integer, `ram_gb`, `disk_gb`, `preemptible`. Put GPUs, wall time and queue names into `backend_parameters`, using keys the server lists in `service-info.tesResources_backend_parameters`. Leave `backend_parameters_strict` false unless the user asks for strict mode.
- Tags: run ID, task ID, attempt number, a group tag for polling, and the task's cache key. On Aruna, also send `aruna-engine.org/idempotency-key` and the group tag it requires.
- Result: read `logs.last()`, then its last executor log. Treat a missing or null `exit_code` as unknown. Prefer the exit code from the staged exit code file when present.
- State handling: `COMPLETE` is success. `EXECUTOR_ERROR` is a tool failure that follows the workflow's retry policy. `SYSTEM_ERROR` and `PREEMPTED` are infrastructure failures that iunctus may retry. `CANCELING` is not terminal. `UNKNOWN` keeps polling with a limit.
- Polling: one ListTasks per interval with the run's group tag, view `MINIMAL`, and pagination. Fetch `FULL` only for tasks that reached a terminal state. Use backoff on errors (as Cromwell does).
- Startup: call `service-info`, record `storage` and `tesResources_backend_parameters`, and select a server profile (Funnel, Planetary, Poiesis, TESK, Aruna, generic). The profile says which optional features are allowed.

How to stage intermediate data
- Configure one object store per run (Aruna bucket, MinIO or cloud S3). Layout suggestion: inputs by content digest (for example `cas/<algorithm>/<hex>`) and outputs under `runs/<run-id>/<task-id>/<attempt>/`.
- Local host files are uploaded once by digest. Remote URLs (`s3://`, `https://`, `drs://` after resolution) pass through unchanged.
- An output of task A becomes an input URL of task B without passing through the engine. The engine downloads a file only if it must read it (CWL `loadContents` or `outputEval`, Nextflow operators on content, Snakemake checkpoints).
- The cache key of a task is a hash of its rendered command, image digest, resources that affect results, and the digests of its inputs. On a cache hit, iunctus reuses the recorded output URLs (with Aruna `versionId`s where available).
- For local, Docker and HPC executors, a shared file system mode with `file://` URLs and hard links (Funnel style) avoids the object store round trip.
- Credentials: the TES server needs write access to the output location. The engine needs read access for files it must inspect. Presigned GET URLs are an option for inputs on servers without S3 credentials. Do not depend on presigned uploads.
- Tasks with many small outputs should write a directory or an archive, because of per-object cost and Aruna's 1024-output limit.

What a common executor trait should contain (sketch; names are placeholders for discussion)
- `capabilities()`: multiple commands per task, inline content, volumes, output globs, directory inputs and outputs, GPUs, preemption, supported URL schemes, maximum inputs and outputs, and whether a shared file system exists.
- `prepare(task)`: a staging hook that turns engine-level inputs into executor-level locations (upload by digest, map to `file://` or `s3://`) and returns the concrete task.
- `submit(task) -> handle`: idempotent under a deterministic name or key made of run, task and attempt.
- `poll(handles) -> updates`: batched status for many tasks at once, with a default loop and backend-specific overrides (TES ListTasks by tag, Slurm `squeue`, Kubernetes watch).
- `cancel(handle)`: asynchronous. The caller waits for a terminal state.
- `result(handle)`: exit code, start and end times, output file list with sizes, and resource usage when the backend reports it.
- `logs(handle)`: a bounded tail of stdout and stderr, plus a reference to the full log files.
- `reconcile(run)`: after an engine restart, find existing tasks by run tag or name. Never mutate.
- `cleanup(handle)`: remove scratch data after results are recorded.
- A resource model close to TES (`cpu`, `memory`, `disk`, `preemptible`) plus a free map of backend parameters, so the TES mapping is almost one to one.
- Errors split into tool failure, infrastructure failure (retryable), preempted and canceled, matching TES states and Crankshaft's `TaskRunError`.

Local test and CI setup
- Run Funnel (Quay image or binary) with local compute and local storage for the default path. Run MinIO or an Aruna v3 node for the S3 path. Run Planetary on kind for a Kubernetes path. Run `tes-compliance-suite` against each server once.

### Gaps
- Whether Aruna v3 will accept inline content, volumes or several executors later is unknown. The portable profile above assumes it will not.
- The exact per-task overhead of Funnel, Planetary and Aruna is unknown. It should be measured before deciding how aggressively to group small steps.
- The recommendations assume the `tes` crate stays maintained. Its activity in 2026 (releases in January and July, pushes in September) supports this, but there is no formal support promise.
