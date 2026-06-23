<!-- What iunctus still needs to be a universal workflow engine that is better than the native ones. -->
<!-- Copyright (c) 2026 The iunctus Contributors -->
<!-- SPDX-License-Identifier: MIT or Apache-2.0 -->

# What is missing for a universal workflow engine

As of 2026-09-30.

iunctus reads and plans the workflows of four languages in one Rust binary, but it does not run a single task for real yet. Every test workflow either runs through the mock executor or has a record of why it cannot, backed by the output of the reference tool. What stands between iunctus and a real engine is the execution layer (section 3), a few language features that only real runs need (section 2), and the features that should make iunctus better than the native engines, not just equal to them: deeper insight into runs, higher reproducibility, better efficiency and storage across several providers (section 4).

| Language | Test workflows | Run with the mock | Work natively, not in iunctus |
| --- | --- | --- | --- |
| Nextflow (nf-core and WorkflowHub) | 345 | 163 | 48: 47 DSL1 scripts that current Nextflow refuses too, 1 parser difference |
| Snakemake | 892 | 667 | 5: Python classes handed to packages in a container |
| CWL | 5974 graphs, 1517 with example jobs, 828 conformance tests | 5888 graphs, 1408 jobs, all 828 conformance tests (620 with checked outputs) | 0 |
| Argo | 1573 documents | 1390 | 104: features that need Kubernetes |

The rest fail in the native tool too: the workflow is broken (`invalid`) or needs something that is not public (`missing`).

## 1. What iunctus already does that native engines cannot

| Feature | iunctus today | Native engines |
| --- | --- | --- |
| One binary for four languages | Nextflow, Snakemake, CWL and Argo in one Rust binary, with no JVM, Python, Node.js or cluster | Nextflow needs Java, Snakemake and cwltool need Python (cwltool also Node.js), Argo needs Kubernetes |
| One execution graph | Every language plans into the same task graph, which `iunctus graph` prints and which grows during the run | Each engine has its own graph and format |
| Argo without Kubernetes | Steps, DAGs, loops, artifacts, plugins and child workflows run on one machine (decision 18) | Argo runs only inside a cluster |
| Content-addressed store | Files live by SHA-256 digest in a store on disk or S3 (`cas/`, `trees/`, `ac/`). The cache key is the digest of a canonical task description | Nextflow and Snakemake key their cache on a work folder or on file times |
| No work folder | A task runs in a scratch folder that is deleted afterwards; its outputs go to the store | Nextflow keeps every task folder in `work/`, Snakemake writes next to the workflow |
| Dry runs that go further | The mock executor reads each task's shell script and makes files that look right, so a dry run goes past Snakemake checkpoints | Snakemake's dry run stops at the first checkpoint; Nextflow's stub run still runs the real script of every process without a stub |
| Checked against the reference tools | About 8,800 test workflows and 828 CWL conformance tests each run, or have a record with the reference tool's own error. Tests also run tasks in a shuffled order to catch order bugs | No engine checks itself against another |

## 2. Gaps to parity with the native engines

The largest gaps are the same in every language: results are not delivered, and steps without a container image cannot run. Per language, the mock runs hide a few features that only real runs reach. "Decided" marks a limit set on purpose in [decisions](decisions.md); "not built" marks work still to do.

### All languages

| Gap | What it blocks | Kind |
| --- | --- | --- |
| Delivering results: Nextflow `publishDir` and the `output` block, Snakemake target files in the workflow folder, the CWL output object, Argo output artifacts | Every real run: outputs stay in the store ("Files are not copied yet" in the Nextflow driver) | Not built |
| An image for every step: steps without a container need `-default-image` | Workflows that rely on tools installed on the host, CWL tools without `DockerRequirement` | Decided (decision 11) |
| Images built from Conda environments | Snakemake rules with only `conda:`, Nextflow processes with only `conda` | Not built (decision 12) |
| Run reports: Nextflow trace, timeline and report files, Snakemake `benchmark:` files and `--report` | Users who read timings, memory use and costs | Not built |
| Language coverage: Groovy, Python and JavaScript cover what the test workflows use | Workflows outside the test set may stop with a clear error (exit code 33), never with a wrong result | Grows with each new test workflow |

### Nextflow

| Gap | What it blocks | Kind |
| --- | --- | --- |
| `exec:` sections | At least 9 test workflows in a real run, like nf-core circrna, mcmicro and quantms | Not built |
| Real `script:` blocks across whole pipelines | Unknown: the pipeline runs were stub runs, as nf-core tests itself | Needs a check against Nextflow |
| Running a pipeline by its Git name and revision (`nf-core/rnaseq -r 3.14`), and remote modules | Users who never clone | Not built |
| `-entry` | Pipelines with several named workflows | Not built |
| Plugins other than nf-schema, nf-validation, nf-anndata and nf-fgbio | Pipelines that use other plugins | Not built |
| An empty CSV with `header: true` gives no rows, where Nextflow stops with an error | Real runs must follow Nextflow again | Mock workaround |
| DSL1 scripts | 47 test workflows; Nextflow 26.04 refuses them too | Decided (decision 14) |

### Snakemake

| Gap | What it blocks | Kind |
| --- | --- | --- |
| The `snakemake` object for `script:`, `wrapper:` and `notebook:` steps, and the Snakefile's functions for `run:` blocks | About 440 test workflows, half of all, in a real run | Not built |
| `pipe()` and `service()` outputs: both jobs must run at the same time on one machine | Workflows that stream between jobs | Not built |
| Workflow code that hands its own classes to a package in the Python container | 5 test workflows | Decided (decision 19) |
| PEP tables in Excel, `gitfile()` Snakefiles, meta-wrappers at other addresses, `listfiles`, `report` and `R` of `snakemake.utils` | No test workflow yet | Not built |

### CWL

| Gap | What it blocks | Kind |
| --- | --- | --- |
| Network access and writable inputs (`NetworkAccess`, `writable` in `InitialWorkDirRequirement`, `InplaceUpdateRequirement`) | Tools that download or change their inputs | Not built |
| `DockerRequirement` with `dockerFile`, `dockerLoad` or `dockerImport`; `SoftwareRequirement` as a requirement | Tools that build or load their own image | Not built |
| Checking the outputs of the 208 conformance tests that only run through | Proof of full conformance | Needs real runs |

### Argo

| Gap | What it blocks | Kind |
| --- | --- | --- |
| `resource` templates for Kubernetes objects, daemons, sidecars, `data` templates | 104 test documents, almost all Argo feature demos and CI/CD examples | Decided (decision 18) |
| Container sets, whose containers share one pod and its volumes | Workflows that use `containerSet` | Not built |
| Executor plugins that answer `Running` or `Pending` | Plugins that work in the background | Not built |

## 3. The execution layer

No task runs for real yet, except one at a time through the simple local executor. Ten steps are planned in [the engine](design/engine.md), section 8; seven more are missing from that plan.

**Planned, in the order they build on each other:**

1. **An async executor interface.** Executors run tasks on Tokio; the driver loop stays on one thread, and the engine keeps up to `max_pending` tasks running.
2. **Output locations as strings, with size and digest,** so an output can live in S3 and not only on the local disk.
3. **A shared `Files` service** that makes a location local when a driver must read it, like a Nextflow `.command.env` or a CWL `cwl.output.json`.
4. **`Launch.mock`,** so that mock workarounds apply only to mock runs.
5. **Retries and cancellation:** attempt numbers, Nextflow `errorStrategy`, Snakemake `retries`, Argo `retryStrategy`. The engine retries failures of the machine; the driver decides about failures of the tool.
6. **Two new task fields:** whether a task may use the network, and whether an input must be writable.
7. **The TES executor** (decision 8): one TES task per task, with the wrapper as a staged input. TES servers such as Funnel also reach HPC schedulers.
8. **A parallel local executor** on Docker or Podman.
9. **Remote inputs:** `s3://`, `gs://` and `https://` files, downloaded in parts or passed to TES as URLs.
10. **Argo container sets** as one task.

**Missing from the plan:**

11. **Delivering results** to the place each language expects (section 2), from the store, as copies or links.
12. **The run database** (decision 10): the task graph, the event log, attempts and log locations in one SQLite file next to the store ([Nextflow runtime](design/nextflow/runtime.md), section 5). Resume, status views and the insight features of section 4 read it.
13. **Preparing images:** tags pinned to digests (done for the local executor), images built from Conda environments (decision 12), and one image for a whole workflow (decision 13).
14. **The context of embedded code:** the `snakemake` object for scripts, wrappers and notebooks, the Snakefile's functions for `run:` blocks, and Nextflow `exec:` sections in the native interpreter.
15. **Logs and reports:** standard output and error of each task, live progress, and trace, timeline and benchmark files.
16. **Secrets and credentials** for each storage provider and remote input, kept out of task descriptions and cache keys.
17. **Several storage providers:** a config file that maps buckets and address prefixes to endpoints and credentials, with secrets as references to environment variables (decision 20), used by the store, remote inputs and delivering results. The store connects to one S3 bucket today, with credentials from the environment.

## 4. Better than the native engines: insight, reproducibility, efficiency

iunctus describes every task of every language the same way, and it knows every file by its digest. Three things follow that no native engine can offer without changing its core: deeper insight into what a run did, higher reproducibility, and better efficiency. Storage across several providers and runs that move between machines build on the same base. Each table says what the user gets, what the native engines do today, and what the feature builds on.

### Insight: see what a run did and why

| Feature | What the user gets | Native engines today | Builds on |
| --- | --- | --- | --- |
| **One record of every run** | Every task, attempt, input and output file, image, time and resource use of every run, in one SQLite database and in the same form for all four languages. Users can query it with SQL | Nextflow's trace file and `nextflow log`, Snakemake's `.snakemake` folder and report, Argo's workflow status: each with its own fields | The run database |
| **Why a task ran again** | For each task that did not come from the cache, the part of its description that changed: the image, the command, an input file or a parameter | Nextflow needs `-dump-hashes` and a manual comparison of two logs. Snakemake names the kind of change (code, params, input), not the change | Cache keys (done), the run database |
| **Where a file came from** | For any output, the chain of tasks, commands, images and inputs that made it, back to the raw data, also across runs, because files are known by digest | Nextflow and Snakemake know only the task folder or rule of the last run | The store, the run database |
| **Where the time went** | The chain of tasks that set the length of the run, the time tasks waited versus ran, and the steps that used the most CPU, memory or money | Nextflow's timeline shows tasks, not the chain that set the length | Logs and reports (section 3) |
| **Comparing two runs** | Which tasks, parameters, images and outputs differ between two runs of a workflow, or between two versions of it | No engine compares runs | The run database |
| **Replay one task anywhere** | Any task, also a failed one, runs again alone on another machine from its description, with the same image and the same inputs by digest | Nextflow's `.command.run` works only in its task folder, on the same file system | Task descriptions (done), the store |
| **Live view and export** | Progress per step, cache hits, running and failed tasks, in the terminal and through the WES API. Traces go out in the OpenTelemetry format to existing dashboards | Nextflow and Snakemake print progress in the terminal; more needs a separate platform | The run database |

### Reproducibility: the same run gives the same results, now and years later

| Feature | What the user gets | Native engines today | Builds on |
| --- | --- | --- | --- |
| **Everything pinned** | Each run records the Git revision of the workflow, the digest of every image and remote input, and every parameter and config value, so the same run can be repeated years later. A remote file or image tag that changed since is reported | Nextflow and Snakemake resolve image tags and remote files again at each run, so a moved tag changes results without notice | Preparing images, remote inputs, the run database |
| **Containers only, images by digest** | Every task runs in a container from an image pinned to its digest, never on the host. Conda environments are locked and built into images (decisions 11 and 12) | Nextflow, Snakemake and cwltool run a step on the host when it names no container; Conda environments are solved again on each machine | Preparing images |
| **Tasks without network by default** | A task that needs no network gets none, so it cannot fetch anything that is not in its description. The workflow or the user says which steps may download | Nextflow, Snakemake and Argo give each task the full network; only CWL turns it off by default | The network field (section 3, step 6) |
| **Reproducibility check** | A sample of cached tasks runs again and the output digests are compared. Tools that do not give the same bytes are marked, and their results are not shared | No engine checks this | The global cache |
| **Order check** | A run with task results in a shuffled order finds steps whose output depends on which task finished first, like a file list collected in completion order. This check already runs on all test workflows | No engine offers it | The shuffled order of the engine (done) |
| **Provenance for every language** | A Workflow Run RO-Crate for each run, with the digest of every input, output and image, in the same form for all four languages | nf-prov for Nextflow, CWLProv for cwltool, `--report` for Snakemake, each different | The run database, the store |

### Efficiency: less compute, less storage, less waiting

| Feature | What the user gets | Native engines today | Builds on |
| --- | --- | --- | --- |
| **Global cache** | A result is reused by any run, pipeline, user or language that asks for the same task: the same image digest, command, inputs and environment. The same nf-core module in two pipelines runs once | Nextflow's task hash holds the session id and the process name, so only `-resume` of the same run reuses results. Snakemake shares only rules marked `cache:`; cwltool only with `--cachedir` on one machine | Cache keys and the S3 store (done) |
| **The same task at the same time runs once** | Two runs that ask for the same task at the same moment share one execution | Both runs compute it | The global cache |
| **No work folder** | Tasks run in scratch folders that are deleted at once. The store keeps each file once, by digest, so a reference genome used by 50 runs takes its space once | Nextflow's `work/` grows with every run and needs `nextflow clean`. Snakemake writes into the workflow folder | The store (done), delivering results (section 3) |
| **Clean-up by rule** | Keep what was used in the last 90 days, what a named run needs, or what is expensive to make again. The rest is removed safely, because the store knows what refers to what | After Nextflow's `work/` is deleted, everything runs again | The run database |
| **Remote files downloaded once** | An `s3://`, `https://` or FTP file is fetched once, checked by digest and shared by all later runs | Nextflow downloads remote files again for each new run | Remote inputs, the store |
| **Preview before the run** | Before any compute is spent, a preview shows which tasks come from the cache and which will run, also past Snakemake checkpoints | Snakemake's dry run stops at checkpoints; Nextflow's `-preview` skips every process, so it lists no tasks | The mock executor (done), the cache |
| **Right-sized resources** | The measured CPU and memory of each step, by input size, set the requests of the next run, so more tasks fit on a machine and fewer fail for lack of memory | Nextflow and Snakemake use what the workflow says, or retry with more | The run database, reports |
| **The longest chain first** | The engine starts the tasks on the longest remaining chain first, using the times of earlier runs, so the run ends sooner on the same machines | Nextflow starts tasks as they become ready; Snakemake weighs the priorities that the workflow sets | The run database |
| **Short tasks together** | Many short tasks with the same image run in one container or one TES task, which saves the start-up time of each | Nextflow's `array` directive and Snakemake's job groups, where the workflow sets them | The TES executor |

### Storage across providers

| Feature | What the user gets | Native engines today | Builds on |
| --- | --- | --- | --- |
| **Several storage providers in one run** | Each bucket or address prefix has its own endpoint, region and credentials: AWS S3, MinIO, Ceph, Cloudflare R2 or another S3-compatible service, Google Cloud Storage, Azure Blob Storage, HTTPS, FTP and local disks. Inputs can come from one provider, the store can live on a second, and results can go to a third | Nextflow takes one S3 endpoint and one set of credentials for the whole run. Snakemake 8 can name several storage providers, but the Snakefile must declare them | The store (one S3 bucket today, credentials from the environment), remote inputs |
| **Storage settings outside the workflow** | A config file of the user or the site says which endpoint and credentials serve which bucket, with secrets taken from environment variables (decision 20), so the same workflow runs on any site without changes | Nextflow needs a config profile per site; Snakemake storage settings sit in the Snakefile or on the command line | Several storage providers |
| **Copies checked by digest** | Data that moves between providers is checked end to end, a file that is already at the target is not copied again, and a run can move its store, for example from a local disk to S3, without running any task again | Nextflow copies files with `publishDir` and does not check their content | The store |
| **A cache in layers** | A fast local disk in front of a shared S3 store: reads try the local disk first, and new results go to both | No engine has this | The store, several storage providers |
| **Tasks near their data** | With several providers and TES servers, a task goes to the site that holds its inputs, instead of the data going to the task | No engine chooses a site by where the data is | The TES executor, several storage providers |

### Runs that move and reach further

| Feature | What the user gets | Native engines today | Builds on |
| --- | --- | --- | --- |
| **Resume from a database** | A stopped run continues from its event log, also on another machine or with another executor: start on a laptop, finish on a TES cluster | Nextflow resumes only in the same launch folder with its `work/`. Snakemake decides by file times and its `.snakemake` folder | The run database, the S3 store |
| **No shared file system** | Tasks get their inputs from the store and send their outputs back, so a cluster needs no NFS | Nextflow and Snakemake need a shared folder or a special storage setup | The TES executor, the store |
| **One entry point for all languages** | A GA4GH WES service that accepts Nextflow, Snakemake, CWL and Argo workflows, with one API for status and results | Each engine has its own server or platform | The run database |
| **Mixed-language runs** | One run that calls a CWL tool from Snakemake, a Snakemake workflow from Nextflow, or an nf-core module from Argo, with files handed over by digest | Snakemake can call CWL tools; nothing else crosses languages | The execution graph (done) |
| **One check for all languages** | `iunctus check` lists every unsupported feature, missing file and broken reference before a run, with the same exit codes everywhere | Each engine has its own linter, if any | The drivers (done) |

## 5. Order of work

Each phase ends with a gate that shows it works. Language gaps (section 2) are closed along the way, in the order of how many test workflows they unlock.

1. **Real runs on one machine.** The async executor interface, `Launch.mock`, the `Files` service, the parallel local executor, retries and cancellation, delivering results, the context of embedded code, images from Conda environments, and logs.
    - Gate: the `test` profiles of nf-core/demo and 20 more nf-core pipelines, and a sample of Snakemake, CWL and Argo test workflows, run for real. Their output files match those of the native tool. This needs a real-run version of the evidence scripts, which run both tools and compare output names and digests.
    - Gate: the 208 CWL conformance tests that only run through today pass for real.
2. **The run database, the global cache and first insights.** The run database, resume, the preview, output locations as strings, several storage providers, remote inputs downloaded once, clean-up by rule, everything pinned, why a task ran again, and where a file came from.
    - Gate: a run that is stopped halfway resumes without running a finished task again. A second pipeline that uses the same nf-core module reuses its results. After a changed parameter, the run names each task that ran again and the parameter that made it.
    - Gate: one run reads its inputs from AWS S3 and a MinIO server, keeps its store on a third provider, and delivers its results to a local disk.
3. **TES and no shared file system.** The TES executor, secrets, the network field, Argo container sets, short tasks together, and the store on S3 across machines.
    - Gate: a pipeline started on a laptop finishes on a TES server, from the same run database.
4. **Deeper insight, trust and reach.** Where the time went, comparing two runs, replaying one task, the live view and export, provenance, the reproducibility and order checks, tasks without network by default, right-sized resources, the longest chain first, a cache in layers, tasks near their data, the WES service, mixed-language runs and `iunctus check`.
    - Gate: one RO-Crate format for runs of all four languages; a mixed-language run of a Snakemake workflow that calls a CWL tool.

## 6. Open questions

1. **FTP inputs.** Should remote inputs include `ftp://`? Many public datasets are on FTP servers, and Nextflow reads them. Recommended: yes, in phase 2.
2. **Network by default.** Should tasks run without network unless the workflow says otherwise, or only when the user asks for it? Nextflow and Snakemake workflows never declare network use, so a default of "off" would break the steps that download.
3. **Trust in a shared cache.** A cache on S3 that several users share means trusting each other's results. Options: one cache namespace per team (the namespace is already part of the cache key), or signed cache entries.
4. **How results are delivered.** Copies are safe; links from the store save space but make the store's files editable by accident. Which is the default?
5. **How a mixed-language run is written.** It can use what each language already has, like Snakemake's `cwl:` action, or a small new file that joins workflows. The second needs a decision on its format first.
