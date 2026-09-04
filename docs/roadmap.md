<!-- What iunctus still needs to be a universal workflow engine that is better than the native ones. -->
<!-- Copyright (c) 2026 The iunctus Contributors -->
<!-- SPDX-License-Identifier: MIT or Apache-2.0 -->

# What is missing for a universal workflow engine

Capability statements reconciled on 2026-10-03. Counts below are historical evidence recorded by 2026-09-30, not qualification of the current revision.

iunctus reads and plans four workflow languages in one Rust binary. Real execution and result delivery are implemented, including parallel local container execution and TES, SLURM and Kubernetes backends. The simulator remains the default and produces synthetic outputs (decision 21). Language parity, failure/restart behavior and real backend qualification still need work. See the [support matrix](support.md) for the implemented subsets and qualification limits; completion marks below mean implementation, not production acceptance.

| Language | Test workflows | Run in the simulator | Work natively, not in iunctus |
| --- | --- | --- | --- |
| Nextflow (nf-core and WorkflowHub) | 345 | 163 | 48: 47 DSL1 scripts that current Nextflow refuses too, 1 parser difference |
| Snakemake | 892 | 667 | 5: Python classes handed to packages in a container |
| CWL | 5974 graphs, 1517 with example jobs, 828 conformance tests | 5888 graphs, 1408 jobs, all 828 conformance tests (620 with checked outputs) | 0 |
| Argo | 1573 documents | 1390 | 104: features that need Kubernetes |

In that historical inventory, the remaining workflows failed in the reference tool or needed unavailable inputs (`invalid` or `missing`). These classifications and counts have not been refreshed for this revision.

## 1. What iunctus already does that native engines cannot

| Feature | iunctus today | Native engines |
| --- | --- | --- |
| One binary for four languages | Nextflow, Snakemake, CWL and Argo in one Rust binary, with no JVM, Python, Node.js or cluster | Nextflow needs Java, Snakemake and cwltool need Python (cwltool also Node.js), Argo needs Kubernetes |
| One execution graph | Every language plans into the same task graph, which `iunctus graph` prints and which grows during the run | Each engine has its own graph and format |
| Argo without Kubernetes | Steps, DAGs, loops, artifacts, plugins and child workflows run on one machine (decision 18) | Argo runs only inside a cluster |
| Content-addressed store | Files live by SHA-256 digest in a store on disk or S3 (`cas/`, `trees/`, `ac/`). The cache key is the digest of a canonical task description | Nextflow and Snakemake key their cache on a work folder or on file times |
| No work folder | A task runs in a scratch folder that is deleted afterwards; its outputs go to the store | Nextflow keeps every task folder in `work/`, Snakemake writes next to the workflow |
| Dry runs that go further | The simulator reads each task's shell script and makes files that look right, so a dry run goes past Snakemake checkpoints | Snakemake's dry run stops at the first checkpoint; Nextflow's stub run still runs the real script of every process without a stub |
| Simulated graphs that say where they can change | A simulated run says that it is one, and marks each task whose outputs decide how many tasks follow: a file that the workflow reads while it plans, files that a wildcard matched and that each start tasks, a Snakemake checkpoint, a CWL scatter or an Argo `withParam` or `when` over task outputs. A fan-out over a real sample sheet is not marked, as it is exact | No native dry run tells where real data can add or remove tasks |
| Graphs of tasks and of steps, drawn by the binary | The graph with one node per task or one node per step, as a summary, JSON, DOT or an SVG that iunctus lays out itself | Nextflow and Snakemake write DAG files that need Graphviz or a browser to draw; Nextflow draws processes, not tasks |
| Checked against the reference tools | Historical corpus evidence covers about 8,800 test workflows and 828 CWL conformance tests, with reference-tool outcomes. Separate order checks shuffle task completion. This is not a current real-execution qualification result | Reference-tool comparisons require explicit corpus and tool versions |

## 2. Gaps to parity with the native engines

Result delivery is implemented for all four languages, but exact output parity needs qualification for each workflow/backend combination. All task and package execution requires containers; a step without an image needs `-default-image`, and Conda-to-image preparation is not complete. Simulated outcomes alone cannot establish real execution semantics. "Decided" marks a limit set on purpose in [decisions](decisions.md); "not built" marks work still to do. The language-gap tables retain historical corpus observations where counts are shown.

### All languages

| Gap | What it blocks | Kind |
| --- | --- | --- |
| Qualifying result delivery: Nextflow `publishDir` and the `output` block, Snakemake target files, the CWL output object, Argo artifacts | Real outputs must match the reference tool, including failures and repeated delivery | Implemented; exact-result qualification remains |
| An image for every step: steps without a container need `-default-image` | Workflows that rely on tools installed on the host, CWL tools without `DockerRequirement` | Decided (decision 11) |
| Images built from Conda environments | Snakemake rules with only `conda:`, Nextflow processes with only `conda` | Locked without network from local repodata (`iunctus lock-environment`, see [Conda environments](design/conda.md)); images not built yet (decision 12) |
| Native run-report compatibility: Nextflow trace/timeline/report and Snakemake `--report` | Users who need native-engine report formats | Snakemake `benchmark:` files and the iunctus run report are implemented; native report-format parity remains |
| Language coverage: Groovy, Python and JavaScript cover what the test workflows use | Workflows outside the test set may stop with a clear error (exit code 33), never with a wrong result | Grows with each new test workflow |

### Nextflow

| Gap | What it blocks | Kind |
| --- | --- | --- |
| Real `script:` blocks across whole pipelines | Unknown: the pipeline runs were stub runs, as nf-core tests itself | Needs a check against Nextflow |
| Running a pipeline by its Git name and revision (`nf-core/rnaseq -r 3.14`), and remote modules | Users who never clone | Not built |
| `-entry` | Pipelines with several named workflows | Not built |
| Plugins other than nf-schema, nf-validation, nf-anndata and nf-fgbio | Pipelines that use other plugins | Not built |
| An empty CSV with `header: true` gives no rows, where Nextflow stops with an error | Real runs must follow Nextflow again | Simulator workaround |
| DSL1 scripts | 47 test workflows; Nextflow 26.04 refuses them too | Decided (decision 14) |

### Snakemake

| Gap | What it blocks | Kind |
| --- | --- | --- |
| FIFO `pipe()` outputs and complete `service()` group semantics | Workflows that stream or depend on ordering/retries inside a group | Bounded same-image service groups implemented; FIFO pipes, internal dependency ordering and group retries remain |
| Workflow code that hands its own classes to a package in the Python container | 5 test workflows | Decided (decision 19) |
| PEP tables in Excel, `gitfile()` Snakefiles, meta-wrappers at other addresses, `listfiles`, `report` and `R` of `snakemake.utils` | No test workflow yet | Not built |

### CWL

| Gap | What it blocks | Kind |
| --- | --- | --- |
| Network access and writable inputs (`NetworkAccess`, `writable` in `InitialWorkDirRequirement`, `InplaceUpdateRequirement`) | Tools that download or change their inputs | Done: `Task.network`, `Writable::{Copy, InPlace}` |
| `DockerRequirement` with `dockerFile`, `dockerLoad` or `dockerImport`; `SoftwareRequirement` as a requirement | Tools that build or load their own image | Not built |
| Checking the outputs of the 208 conformance tests that only run through | Proof of full conformance | Needs real runs |

### Argo

| Gap | What it blocks | Kind |
| --- | --- | --- |
| `resource` templates for Kubernetes objects, daemons, sidecars, `data` templates | 104 test documents, almost all Argo feature demos and CI/CD examples | Decided (decision 18) |
| Container sets, whose containers share one pod and its volumes | Workflows that use `containerSet` | Done: one task of several containers ([runtime](design/argo/runtime.md), section 8.2) |
| Executor plugins that answer `Running` or `Pending` | Plugins that work in the background | Not built |

## 3. The execution layer

The async engine, shared runner, parallel local executor and remote backends are implemented. The following list records the original execution workstreams and their remaining qualification work, with details in [the engine](design/engine.md) and [executors](design/executors.md). Real remote deployment requires the documented shared paths; implementing a backend does not qualify it on a live cluster.

**Implemented execution foundations, in their original dependency order:**

1. **An async executor interface.** Executors run tasks on Tokio; the driver loop stays on one thread, and the engine keeps up to `max_pending` tasks running.
2. **Output locations as strings, with size and digest,** so an output can live in S3 and not only on the local disk.
3. **A shared `Files` service** that makes a location local when a driver must read it, like a Nextflow `.command.env` or a CWL `cwl.output.json`.
4. **A switch for simulated runs** in `Launch`, so that the workarounds of the simulator apply only to simulated runs.
5. **Retries and cancellation:** attempt numbers, Nextflow `errorStrategy`, Snakemake `retries`, Argo `retryStrategy`. The engine retries failures of the machine; the driver decides about failures of the tool.
6. **Two new task fields:** whether a task may use the network, and whether an input must be writable.
7. **The TES executor** (decision 8): one TES task per task, with the wrapper as a staged input. TES servers such as Funnel also reach HPC schedulers.
8. **A parallel local executor** on Docker or Podman.
9. **Remote inputs:** `s3://`, `gs://` and `https://` files, downloaded in parts or passed to TES as URLs.
10. **Argo container sets** as one task.

**Additional workstreams, with implementation and remaining work distinguished:**

11. **Delivering results** to the place each language expects (section 2), from the store, as copies or links. Implemented; qualify exact results and delivery failures.
12. **The run database** (decision 10): the task graph, the event log, attempts and log locations in one SQLite file next to the store. Done: see [runs](design/runs.md); resume, the web page, the RO-Crate export and the first insight features read it.
13. **Preparing images:** tags pinned to digests (done for the local executor), images built from Conda environments (decision 12), and one image for a whole workflow (decision 13).
14. **The context of embedded code:** the `snakemake` object for scripts, wrappers and notebooks, the Snakefile's functions for `run:` blocks, and Nextflow `exec:` sections in the native interpreter. Done: see [embedded code](design/snakemake/embedded-code.md).
15. **Logs and reports:** standard output and error of each task, live progress, and trace, timeline and benchmark files.
16. **Secrets and credentials** for each storage provider and remote input, kept out of task descriptions and cache keys. Done for storage: secrets stay in the providers of the config file, and a test checks that none reaches the output, the run database, the RO-Crate or the cache ([storage](design/storage.md), section 5).
17. **Several storage providers:** a config file that maps buckets and address prefixes to endpoints and credentials, with secrets as references to environment variables (decision 20), used by the store, remote inputs and delivering results. Done: see [storage](design/storage.md).

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
| **Live view and export** | Progress per step, cache hits, running and failed tasks, in the terminal, on a web page served by the binary (decision 23) and through the WES API. Traces go out in the OpenTelemetry format to existing dashboards | Nextflow and Snakemake print progress in the terminal; more needs a separate platform | The run database |

### Reproducibility: the same run gives the same results, now and years later

| Feature | What the user gets | Native engines today | Builds on |
| --- | --- | --- | --- |
| **Everything pinned** | Each run records the Git revision of the workflow, the digest of every image and remote input, and every parameter and config value, so the same run can be repeated years later. A remote file or image tag that changed since is reported | Nextflow and Snakemake resolve image tags and remote files again at each run, so a moved tag changes results without notice | Done: see [reproducibility](design/reproducibility.md) |
| **Containers only, images by digest** | Every task runs in a container from an image pinned to its digest, never on the host. Conda environments are locked and built into images (decisions 11 and 12) | Nextflow, Snakemake and cwltool run a step on the host when it names no container; Conda environments are solved again on each machine | Preparing images |
| **Tasks without network by default** | A task that needs no network gets none, so it cannot fetch anything that is not in its description. The workflow or the user says which steps may download | Nextflow, Snakemake and Argo give each task the full network; only CWL turns it off by default | Done as an option (`-no-network`); the default is open question 1 |
| **Reproducibility check** | A sample of cached tasks runs again and the output digests are compared. Tools that do not give the same bytes are marked, and their results are not shared | No engine checks this | Done: see [reproducibility](design/reproducibility.md) |
| **Order check** | A run with task results in a shuffled order finds steps whose output depends on which task finished first, like a file list collected in completion order. This check already runs on all test workflows | No engine offers it | Done: `iunctus check-order` |
| **Provenance for every language** | A Workflow Run RO-Crate of the Provenance Run Crate profile for each run (decision 25): the provenance graph with the digest of every input, output and image, in the same form for all four languages | nf-prov for Nextflow, CWLProv for cwltool, `--report` for Snakemake, each different | The run database, the store |

### Efficiency: less compute, less storage, less waiting

| Feature | What the user gets | Native engines today | Builds on |
| --- | --- | --- | --- |
| **Global cache** | A result is reused by any run, pipeline, user or language that asks for the same task: the same image digest, command, inputs and environment. The same nf-core module in two pipelines runs once | Nextflow's task hash holds the session id and the process name, so only `-resume` of the same run reuses results. Snakemake shares only rules marked `cache:`; cwltool only with `--cachedir` on one machine | Cache keys and the S3 store (done) |
| **The same task at the same time runs once** | Two runs that ask for the same task at the same moment share one execution | Both runs compute it | The global cache |
| **No work folder** | Tasks run in scratch folders that are deleted at once. The store keeps each file once, by digest, so a reference genome used by 50 runs takes its space once | Nextflow's `work/` grows with every run and needs `nextflow clean`. Snakemake writes into the workflow folder | The store (done), delivering results (section 3) |
| **Clean-up by rule** | Keep what was used in the last 90 days, what a named run needs, or what is expensive to make again. The rest is removed safely, because the store knows what refers to what | After Nextflow's `work/` is deleted, everything runs again | The run database |
| **Remote files downloaded once** | An `s3://`, `https://` or FTP file is fetched once, checked by digest and shared by all later runs | Nextflow downloads remote files again for each new run | Remote inputs, the store |
| **Preview before the run** | Before any compute is spent, a preview shows which tasks come from the cache and which will run, also past Snakemake checkpoints | Snakemake's dry run stops at checkpoints; Nextflow's `-preview` skips every process, so it lists no tasks | The simulator (done), the cache |
| **Right-sized resources** | The measured CPU and memory of each step, by input size, set the requests of the next run, so more tasks fit on a machine and fewer fail for lack of memory | Nextflow and Snakemake use what the workflow says, or retry with more | Done: see [efficiency](design/efficiency.md), section 1 |
| **The longest chain first** | The engine starts the tasks on the longest remaining chain first, using the times of earlier runs, so the run ends sooner on the same machines | Nextflow starts tasks as they become ready; Snakemake weighs the priorities that the workflow sets | The run database |
| **Short tasks together** | Many short tasks with the same image run in one container or one TES task, which saves the start-up time of each | Nextflow's `array` directive and Snakemake's job groups, where the workflow sets them | Done: see [efficiency](design/efficiency.md), section 2 |

### Storage across providers

| Feature | What the user gets | Native engines today | Builds on |
| --- | --- | --- | --- |
| **Several storage providers in one run** | Each bucket or address prefix has its own endpoint, region and credentials: AWS S3, MinIO, Ceph, Cloudflare R2 or another S3-compatible service, Google Cloud Storage, Azure Blob Storage, HTTPS, FTP and local disks. Inputs can come from one provider, the store can live on a second, and results can go to a third | Nextflow takes one S3 endpoint and one set of credentials for the whole run. Snakemake 8 can name several storage providers, but the Snakefile must declare them | Done: [storage](design/storage.md) |
| **Storage settings outside the workflow** | A config file of the user or the site says which endpoint and credentials serve which bucket, with secrets taken from environment variables (decision 20), so the same workflow runs on any site without changes | Nextflow needs a config profile per site; Snakemake storage settings sit in the Snakefile or on the command line | Done: [storage](design/storage.md) |
| **Copies checked by digest** | Data that moves between providers is checked end to end, a file that is already at the target is not copied again, and a run can move its store, for example from a local disk to S3, without running any task again | Nextflow copies files with `publishDir` and does not check their content | The store |
| **A cache in layers** | A fast local disk in front of a shared S3 store: reads try the local disk first, and new results go to both | No engine has this | Done: [the cache](design/cache.md), section 8 |
| **Tasks near their data** | With several providers and TES servers, a task goes to the site that holds its inputs, instead of the data going to the task | No engine chooses a site by where the data is | The choice of a site is done; TES servers that read their inputs themselves are next: see [efficiency](design/efficiency.md), section 3 |

### Flexibility and reach

| Feature | What the user gets | Native engines today | Builds on |
| --- | --- | --- | --- |
| **Runs anywhere** | The same workflow runs on a laptop with Docker or Podman, on any TES server, on Kubernetes, or on a SLURM cluster with Apptainer or Podman (decision 22) | Each engine has its own list of executors; Argo needs Kubernetes | The execution layer |
| **Checked inputs for every workflow** | One JSON Schema of the inputs of each workflow, in all four languages, checked before any task starts, with the same messages everywhere; `iunctus inputs` prints it or help made from it, and warns where the schema and the code differ (decision 24) | nf-core and Snakemake check params with a schema where the workflow has one; CWL checks its types; Argo only enums | Done: [the inputs of a workflow](design/inputs.md) |
| **Resume from a database** | A stopped run continues from its event log, also on another machine or with another executor: start on a laptop, finish on a TES cluster | Nextflow resumes only in the same launch folder with its `work/`. Snakemake decides by file times and its `.snakemake` folder | The run database, the S3 store |
| **No shared file system** | Tasks get their inputs from the store and send their outputs back, so a cluster needs no NFS | Nextflow and Snakemake need a shared folder or a special storage setup | The TES executor, the store |
| **One entry point for all languages** | A GA4GH WES service that accepts Nextflow, Snakemake, CWL and Argo workflows, with one API for status and results | Each engine has its own server or platform | The run database |
| **Mixed-language runs** | One run that calls a CWL tool from Snakemake, a Snakemake workflow from Nextflow, or an nf-core module from Argo, with files handed over by digest | Snakemake can call CWL tools; nothing else crosses languages | The execution graph (done) |
| **One check for all languages** | `iunctus check` lists every unsupported feature, missing file and broken reference before a run, with the same exit codes everywhere | Each engine has its own linter, if any | Done: [checks](design/checks.md) |

## 5. To-do list

The goal: iunctus is easy to set up, gives much better results than the native engines, and is far more flexible. Work runs in workstreams, at most three at a time; each item names its decision or section. Language gaps (section 2) are closed along the way, in the order of how many test workflows they unlock.

### Simulator and graphs

- [x] Rename the mock executors to one `simulator` (decision 21), and say "simulated" in code, docs, failure records and messages.
- [x] Every simulated run and graph says that it is a simulation with made-up files, which a run with real data refines.
- [x] Mark the points of a simulated graph where real data can add or remove tasks: a glob that can match more files, a task output whose content the workflow reads, a Snakemake checkpoint, a CWL scatter or an Argo `withParam` over a task output.
- [x] A graph with one node per step (process, rule, step or template), next to the graph with one node per task.
- [x] SVG output of both graphs, drawn by iunctus itself, with no Graphviz needed.
- Done when: `iunctus graph` gives both graphs as JSON, DOT, summary and SVG for all four languages, with the uncertain points marked.

### Real execution

- [x] The async executor interface (section 3, step 1).
- [x] Output locations as strings, the `Files` service, and a switch for simulated runs (section 3, steps 2 to 4).
- [x] Retries and cancellation (section 3, step 5).
- [x] Delivering results (section 3, step 11).
- [x] The parallel local executor on Docker or Podman, found without any config (decision 22).
- [x] The `snakemake` object for Snakemake scripts, wrappers and notebooks, as Snakemake 9.27 writes it in each language: see [embedded code](design/snakemake/embedded-code.md).
- [x] Snakemake `run:` blocks in the task's container, with the Snakefile's globals that they use: see [embedded code](design/snakemake/embedded-code.md), section 5.
- [x] Nextflow `exec:` sections evaluated by the native interpreter: see [embedded code](design/snakemake/embedded-code.md), section 6. External task and package execution remains in containers.
- [ ] Images built from Conda environments (decision 12).
- [ ] The TES executor (done, tested with a fake server and Funnel), tested with TESK too (decision 22).
- [ ] The SLURM executor with Apptainer or Podman (done, tested with a simulated SLURM, and with Apptainer on a real SLURM cluster: nf-core, CWL, Snakemake and Argo test workflows, see [executors](design/executors.md), section 4), tested with Podman on a SLURM cluster (decision 22).
- [ ] The Kubernetes executor, one Kubernetes Job per task (done through the API, tested with a fake API server), tested on a `kind` cluster (decision 22).
- [x] The network and writable-input fields, remote inputs, Argo container sets (section 3, steps 6, 9 and 10): see [engine](design/engine.md), section 4, and [Argo runtime](design/argo/runtime.md), section 8.2.
- [x] Several storage providers from a config file, secrets from environment variables (decision 20): see [storage](design/storage.md).
- Done when: the `test` profiles of nf-core/demo and 20 more nf-core pipelines, and a sample of Snakemake, CWL and Argo test workflows, run for real on each executor, and their outputs match those of the native tool. This needs a real-run version of the evidence scripts. The 208 CWL conformance tests that only run through today pass for real.
  - Historical local-executor evidence recorded by 2026-09-30, without network (see [engine](design/engine.md), section 7), not rerun qualification of the current revision: nf-core/demo gives all files of Nextflow; 284 of 287 CWL conformance tests give cwltool's output; 315 of 422 tests of Snakemake give the same result as Snakemake 9.27 (the other differences have known reasons); 277 Argo test workflows end as Argo's tests expect. Real-run evidence scripts exist for Nextflow, CWL, Snakemake and Argo.

### Run database, web page and provenance

- [x] The run database (decision 10), filled by every run, simulated or real ([runs](design/runs.md)).
- [x] Resume from the run database, for every executor.
- [x] The web page (decision 23): the graph as it grows, the state of each step, logs and outputs, for the current run or any past run.
- [x] The graph view of the page, as in Argo Workflows: move, zoom and fit, top down or left to right, round icons of the states, large steps folded into one box that opens on a click, open points beside their task, and live states in place; `iunctus graph -format svg` draws the same picture.
- [x] Workflow Run RO-Crate with the provenance graph (decision 25): valid at the level REQUIRED of the Provenance Run Crate profile 0.5.
- [x] Insight: why a task ran again, where a file came from, where the time went, comparing two runs, replaying one task, OpenTelemetry export (section 4): see [runs](design/runs.md), section 6.
- Done when: a real run can be followed live on the page, and its RO-Crate passes the checks of the Provenance Run Crate profile.

### Inputs

- [x] One JSON Schema of the inputs of each workflow in all four languages (decision 24), printed by `iunctus inputs` and kept in the run database and the RO-Crate ([the inputs of a workflow](design/inputs.md)).
- [x] Check every run against it before any task starts, with the same errors in every language.
- [x] Warn where the schema and the parameters that the workflow reads differ.
- [x] The same for outputs, so that one workflow can feed another: `iunctus outputs` prints one JSON Schema of the outputs of each workflow in all four languages ([the inputs of a workflow](design/inputs.md), section 9).

### Cache, efficiency and reproducibility

- [x] The global cache by default, the same task at the same time run once, clean-up by rule, remote files downloaded once (section 4): see [the cache](design/cache.md).
- [x] A cache in layers (section 4): a folder of this machine in front of a shared store, see [the cache](design/cache.md), section 8.
- [x] The preview before a run (section 4): see [the cache](design/cache.md), section 5.
- [x] The longest chain first (section 4), in the local executor: see [the cache](design/cache.md), section 6.
- [x] Right-sized resources (section 4): see [efficiency](design/efficiency.md), section 1.
- [x] Short tasks together (section 4): see [efficiency](design/efficiency.md), section 2.
- [ ] Tasks near their data (section 4): the choice of a site and the sending to its TES server are done; the data still goes through this machine ([efficiency](design/efficiency.md), section 3).
- [x] Everything pinned, with `iunctus changes` and `iunctus repeat`; tasks without network as an option (whether it becomes the default is open question 1); the reproducibility check; the order check for users: see [reproducibility](design/reproducibility.md).

### Easy setup

- [x] A laptop needs nothing but the binary and Docker or Podman (decision 26).
- [x] One config file for executors and storage, with secrets from environment variables ([storage](design/storage.md), [executors](design/executors.md)).
- [x] Clear errors that say what to install or set, in the same form for every language, with one exit code per kind of problem: see [errors, exit codes and checks](design/checks.md).
- [x] `iunctus check <workflow>` reports at once everything that would stop a real run, in every language, and `iunctus check-machine` what is ready on this machine and what is missing (section 4, "One check for all languages").

## 6. Open questions

1. **Network by default.** Should tasks run without network unless the workflow says otherwise, or only when the user asks for it? Nextflow and Snakemake workflows never declare network use, so a default of "off" would break the steps that download.
2. **Trust in a shared cache.** A cache on S3 that several users share means trusting each other's results. Options: one cache namespace per team (the namespace is already part of the cache key), or signed cache entries.
3. **How results are delivered.** Copies are safe; links from the store save space but make the store's files editable by accident. Which is the default?
4. **How a mixed-language run is written.** It can use what each language already has, like Snakemake's `cwl:` action, or a small new file that joins workflows. The second needs a decision on its format first.
