<!-- What iunctus still needs to be a universal workflow engine that is better than the native ones. -->
<!-- Copyright (c) 2026 The iunctus Contributors -->
<!-- SPDX-License-Identifier: MIT or Apache-2.0 -->

# What is missing for a universal workflow engine

As of 2026-09-30.

iunctus reads and plans the workflows of four languages in one Rust binary, but it does not run a single task for real yet. Every test workflow either runs through the mock executor or has a record of why it cannot, backed by the output of the reference tool. What stands between iunctus and a real engine is the execution layer (section 3), a few language features that only real runs need (section 2), and the features that should make iunctus better than the native engines, not just equal to them (section 4).

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

No task runs for real yet, except one at a time through the simple local executor. Ten steps are planned in [the engine](design/engine.md), section 8; six more are missing from that plan.

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
12. **The run database** (decision 10): the task graph, the event log, attempts and log locations in one SQLite file next to the store ([Nextflow runtime](design/nextflow/runtime.md), section 5). Resume, `iunctus log` and status views read it.
13. **Preparing images:** tags pinned to digests (done for the local executor), images built from Conda environments (decision 12), and one image for a whole workflow (decision 13).
14. **The context of embedded code:** the `snakemake` object for scripts, wrappers and notebooks, the Snakefile's functions for `run:` blocks, and Nextflow `exec:` sections in the native interpreter.
15. **Logs and reports:** standard output and error of each task, live progress, and trace, timeline and benchmark files.
16. **Secrets and credentials** for stores and remote inputs, kept out of task descriptions and cache keys.

## 4. Unique features to build

The biggest advantage of iunctus is that every task of every language is described the same way, and every file is known by its digest. Everything below builds on that. No native engine can offer these features without changing its core.

### Storage and reuse

| Feature | What the user gets | Native engines today | Builds on |
| --- | --- | --- | --- |
| **Global cache** | A result is reused by any run, pipeline, user or language that asks for the same task: the same image digest, command, inputs and environment. The same nf-core module in two pipelines runs once | Nextflow's task hash holds the session id and the process name, so only `-resume` of the same run reuses results. Snakemake shares only rules marked `cache:`; cwltool only with `--cachedir` on one machine | Cache keys and the S3 store (done) |
| **No work folder** | Tasks run in scratch folders that are deleted at once. The store keeps each file once, by digest, so a reference genome used by 50 runs takes its space once | Nextflow's `work/` grows with every run and needs `nextflow clean`. Snakemake writes into the workflow folder | The store (done), delivering results (section 3) |
| **Clean-up by rule** | Keep what was used in the last 90 days, what a named run needs, or what is expensive to make again. The rest is removed safely, because the store knows what refers to what | After Nextflow's `work/` is deleted, everything runs again | The run database |
| **Remote files downloaded once** | An `s3://`, `https://` or FTP file is fetched once, checked by digest and shared by all later runs | Nextflow downloads remote files again for each new run | Remote inputs, the store |

### Runs that survive anything

| Feature | What the user gets | Native engines today | Builds on |
| --- | --- | --- | --- |
| **Resume from a database** | A stopped run continues from its event log, also on another machine or with another executor: start on a laptop, finish on a TES cluster | Nextflow resumes only in the same launch folder with its `work/`. Snakemake decides by file times and its `.snakemake` folder | The run database, the S3 store |
| **No shared file system** | Tasks get their inputs from the store and send their outputs back, so a cluster needs no NFS | Nextflow and Snakemake need a shared folder or a special storage setup | The TES executor, the store |
| **Preview before the run** | `iunctus plan` shows which tasks are cached and which will run, also past Snakemake checkpoints | Snakemake's dry run stops at checkpoints; Nextflow's `-preview` skips every process, so it lists no tasks | The mock executor (done), the cache |
| **One entry point for all languages** | A GA4GH WES service that accepts Nextflow, Snakemake, CWL and Argo workflows, with one API for status and results | Each engine has its own server or platform | The run database |

### Trust and reproducibility

| Feature | What the user gets | Native engines today | Builds on |
| --- | --- | --- | --- |
| **Provenance for every language** | A Workflow Run RO-Crate for each run, with the digest of every input, output and image, in the same form for all four languages | nf-prov for Nextflow, CWLProv for cwltool, `--report` for Snakemake, each different | The run database, the store |
| **Reproducibility check** | Run a sample of cached tasks again and compare output digests. Tools that do not give the same bytes are marked, and their results are not shared | No engine checks this | The global cache |
| **Tasks without network by default** | A task that needs no network gets none, which makes it safer and its result shareable; the workflow says which steps may download | Nextflow, Snakemake and Argo give each task the full network; only CWL turns it off by default | The network field (section 3, step 6) |
| **Right-sized resources** | The measured CPU and memory of each step, by input size, suggest the resources of the next run, in every language | Nextflow and Snakemake use what the workflow says, or retry with more | The run database, reports |

### Across languages

| Feature | What the user gets | Native engines today | Builds on |
| --- | --- | --- | --- |
| **Mixed-language runs** | One run that calls a CWL tool from Snakemake, a Snakemake workflow from Nextflow, or an nf-core module from Argo, with files handed over by digest | Snakemake can call CWL tools; nothing else crosses languages | The execution graph (done) |
| **One check for all languages** | `iunctus check` lists every unsupported feature, missing file and broken reference before a run, with the same exit codes everywhere | Each engine has its own linter, if any | The drivers (done) |

## 5. Order of work

Each phase ends with a gate that shows it works. Language gaps (section 2) are closed along the way, in the order of how many test workflows they unlock.

1. **Real runs on one machine.** The async executor interface, `Launch.mock`, the `Files` service, the parallel local executor, retries and cancellation, delivering results, the context of embedded code, images from Conda environments, and logs.
    - Gate: the `test` profiles of nf-core/demo and 20 more nf-core pipelines, and a sample of Snakemake, CWL and Argo test workflows, run for real. Their output files match those of the native tool. This needs a real-run version of the evidence scripts, which run both tools and compare output names and digests.
    - Gate: the 208 CWL conformance tests that only run through today pass for real.
2. **The run database and the global cache.** The run database, resume, `iunctus plan`, output locations as strings, remote inputs downloaded once, and clean-up by rule.
    - Gate: a run that is stopped halfway resumes without running a finished task again. A second pipeline that uses the same nf-core module reuses its results.
3. **TES and no shared file system.** The TES executor, secrets, the network field, Argo container sets, and the store on S3 across machines.
    - Gate: a pipeline started on a laptop finishes on a TES server, from the same run database.
4. **Trust and reach.** Provenance, the reproducibility check, tasks without network by default, right-sized resources, the WES service, mixed-language runs and `iunctus check`.
    - Gate: one RO-Crate format for runs of all four languages; a mixed-language run of a Snakemake workflow that calls a CWL tool.

## 6. Open questions

1. **FTP inputs.** Should remote inputs include `ftp://`? Many public datasets are on FTP servers, and Nextflow reads them. Recommended: yes, in phase 2.
2. **Network by default.** Should tasks run without network unless the workflow says otherwise, or only when the user asks for it? Nextflow and Snakemake workflows never declare network use, so a default of "off" would break the steps that download.
3. **Trust in a shared cache.** A cache on S3 that several users share means trusting each other's results. Options: one cache namespace per team (the namespace is already part of the cache key), or signed cache entries.
4. **How results are delivered.** Copies are safe; links from the store save space but make the store's files editable by accident. Which is the default?
5. **How a mixed-language run is written.** It can use what each language already has, like Snakemake's `cwl:` action, or a small new file that joins workflows. The second needs a decision on its format first.
