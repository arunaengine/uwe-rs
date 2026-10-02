<!-- How iunctus makes runs reproducible: what each run records to pin it, what changed since a run, repeating a run exactly, the reproducibility check, the order check and tasks without network. -->
<!-- Copyright (c) 2026 The iunctus Contributors -->
<!-- SPDX-License-Identifier: MIT or Apache-2.0 -->

# Reproducibility

Status: implemented, 2026-09-30. This builds on [decisions](../decisions.md) 7, 9, 10, 11, 20, 24, 25 and 26, and fills the part "Reproducibility" of the [roadmap](../roadmap.md), section 4.

The goal: the same run gives the same results now and years later. iunctus knows every file by its digest and every task by its description, so it can pin a run, tell what changed since, and run it again exactly. It can also check what no engine checks: whether a tool gives the same bytes again, and whether a workflow depends on the order in which its tasks finish.

| Command or option | What it does |
|---|---|
| (every run) | records what pins it: the files of its workflow and command line, its remote files, its images and the inputs of its tasks, each by digest (section 1) |
| `iunctus changes <run>` | tells what changed since the run: files of the workflow, files that the command line named, input files, remote files and image tags (section 2) |
| `iunctus repeat <run>` | runs the run again with its files, remote files and images as it had them, and compares the outputs of each step (section 3) |
| `-check-reproducibility <percent>` | runs this share of the tasks whose results come from the cache again, like `10%`, and compares their outputs by digest; a tool that gives other bytes shares no results any more (section 4) |
| `iunctus check-order <workflow>` | runs the workflow twice, with the results of tasks in the order asked and in the reverse order, and names the steps whose outputs depend on the order (section 5) |
| `-no-network` | runs the tasks without network whose workflow does not say that they need it; `[network]` of the config file names the steps that get it anyway (section 6) |

## 1. What a run records

Every run, simulated or real, records in the run database ([runs](runs.md), schema version 4) what it needs to be repeated, and a real run keeps the contents in its store ([the cache](cache.md)):

| What | In the run database | In the store of a real run |
|---|---|---|
| The iunctus that ran it | `runs.program`, like `iunctus 0.1.0` | |
| The folder it started in, where relative paths of its command line start | `runs.launch` | |
| The Git revision of the workflow | `runs.revision` | |
| The files of the workflow | `sources` of kind `workflow`: path in the folder of the workflow, digest, size; the folder in `runs.workflow_folder` | each file by digest, and their tree |
| Files that the command line names: `-c`, `-configfile`, `-params-file`, `-inputs` | `sources` of kind `named`: absolute path, digest, size | each file by digest |
| Parameters and the values of the inputs | `runs.parameters`, `runs.inputs` | |
| The JSON Schemas of the inputs and the outputs | `runs.input_schema`, `runs.output_schema` | |
| Remote files that tasks or workflow code read | `remote_files`: address, digest, size, the time the content was read, and its version (ETag or time of change) where the provider gave one | each file by digest |
| The inputs of each task | `files.digest` of each input, as the runner hashed it (`Outcome.inputs`) | local inputs stay where they are |
| The image of each task as it ran | `attempts.image`, pinned to its digest; `Database::images` pairs it with the name the task gave | |
| The run that this one repeats | `runs.repeats` | |

**The files of the workflow.** When the workflow is in a Git repository, they are the files that Git tracks, read from the index of the repository (`.git/index`, versions 2 to 4, also for work trees and submodules) without a Git program (decision 26). So the record holds exactly the files of the repository, also changes that are not committed. Outside Git, they are the files of the folder of the workflow, without hidden files, links and the folders that runs write into (`work`, `results`, `output`, `outputs`, `logs` and `-output-dir`). A folder of more than 5,000 files or 256 MB holds more than a workflow; then a repository gives the files of the folder of the target only, and a plain folder gives none, which a real run says in a note. The store keeps the files of the workflow as one tree, so a later run of the same files stores nothing and asks the store once.

**Remote files.** The storage providers note every remote file that the process reads ([storage](storage.md), section 4): task inputs at addresses, files that workflow code reads while it plans, like a sample sheet on the web, and Snakemake modules from the web. A download keeps two notes next to its copy in `<cache>/downloads`: `<copy>.sha256` with its digest while the copy does not change, so later runs do not hash it again, and `<copy>.version` with the ETag or time of change that the provider sent with the download. The time of reading is the time of the copy, which an earlier run may have made. Outputs of tasks in a store at an address are no inputs and are left out. A password in an address never reaches a record.

**The RO-Crate** of a run (decision 25) holds the same: `workflow/` with the files of the workflow as the run had them, from the store of a real run (the main file at the top of the crate comes from there too, not from the folder as it is now), the named files in `data/`, each remote file with its `sha256`, `contentSize` and `sdDatePublished` (the time the run read it), `outputs.schema.json` with a `FormalParameter` per output as the `output` of the workflow, and each image with its digest.

## 2. What changed since a run

`iunctus changes <run>` compares what the run recorded with what is there now:

| Part | Compared by |
|---|---|
| The files of the workflow | digest; files that are gone or new count too; and the Git revision |
| Files that the command line named | digest |
| Files on this machine that tasks read and no task made | digest, else size where the run did not know the digest, as a simulated run does not |
| Remote files | a fresh request to the provider: gone, another version, or another size; a provider that gives no version leaves a file of the same size `unknown` |
| Images | the digest that the tag names at its registry now, against the digest the run ran at |

It prints one line per change, like `changed  main.nf` or `changed  https://example.org/a.csv: its version is "b" now; the run read "a"`. Asking providers and registries needs the network: one request per remote file and per image. Without network, those parts say `unknown` with the reason.

## 3. Repeating a run

`iunctus repeat <run>` runs a past run again, exactly:

1. It prints what changed since the run (section 2).
2. When a file of the workflow or of the command line changed, it restores them as the run had them from the store of the run into `<cache>/repeats/<run>/`, and runs the workflow from there. A simulated run keeps no copies, so its repeat stops with a clear error when its files changed.
3. Each remote file that the store keeps answers every read of its address, through the storage providers (`Storage::pin`), so the repeat reads the bytes that the run read, also when the file at the address has changed or is gone. A remote file that the store lacks is read again, with a note.
4. Each image runs at the digest that the run ran at (`Runner::pin_images`), whatever its tag names now.
5. It starts in the folder where the run started, with the command line of the run. Options of `iunctus repeat` may choose where the run goes, like `-executor`, `-store`, the settings of executors, `-page` and `-check-reproducibility`; the workflow and its parameters stay those of the run.
6. At its end it compares the steps of the repeat with those of the run by the digests of their outputs (section 5 explains the comparison) and names the steps that differ.

The repeat is a run of its own, with `runs.repeats` naming the run it repeats. Results come from the cache as in any run; add `-check-reproducibility 100%` to run the cached tasks again as well.

## 4. The reproducibility check

With `-check-reproducibility <percent>`, a real run runs that share of the tasks whose results come from the cache again, on its executor, and compares the outputs with those in the cache by digest (`task-runner/src/check.rs`). The standard output counts as an output, since drivers read it, like Nextflow's `stdout` outputs and Argo's `result`; the standard error does not, as tools write times there. Which tasks run again depends only on the run and the cache key, so each run checks others, and a run checks the same ones when it asks again.

The verdict is one of:

- **same**: every output has the same digest;
- **differs**: these outputs have other digests, or only one result has them;
- **failed**: the task failed when it ran again.

The run keeps the result of the cache, so that the tasks after it stay the same. The verdict goes into the outcome (`Outcome.reproducibility`), the run database (`checks`), a line of the run like `[checked] STAMP: ran again and gave other bytes than in run 1a0f...: stamp.txt; its tool shares no results any more`, and the panel of the task on the web page.

**Tools that give other bytes.** When a task gives other bytes from the same inputs, its tool is marked in the store: `tools/<digest>.json` notes the image, the step, the run and the outputs that differed. The tool is its image, pinned to a digest, and the last part of the name of its step, so the same nf-core module in two pipelines is one tool. No run shares the results of a marked tool any more: its tasks run in every run. Resume still takes them, as it takes results from the run database of its own lineage ([runs](runs.md), section 3), which is the `lineage` scope of [the Nextflow runtime](nextflow/runtime.md), section 5.3. A new image of the tool, with another digest, is another tool and shares again.

## 5. The order check

`iunctus check-order <workflow> [options]` runs the workflow twice, with the simulator unless `-executor` says otherwise: once with the results of its tasks handed to the workflow in the order it asked for them, and once in the reverse order, with up to 64 tasks pending at once (`-max-pending` changes it). This uses the orders of the engine that the tests of the test workflows use ([the engine](engine.md), section 5). Then it compares the steps of the two runs by content (`Database::compare_steps`), without pairing tasks, because the names and ids of tasks may depend on the order:

| Kind | What differs | What it means |
|---|---|---|
| `Tasks` | the number of tasks of the step | the plan depends on the order |
| `Made` | how the tasks are made: command, script, environment, or which content a task sees at which path, while the content they read is the same | the step depends on the order, like a list collected in the order tasks finished |
| `Inputs` | the content that the tasks read | it follows from an earlier step that differs |
| `Outputs` | only the bytes the tasks made | the tool gives other bytes; the reproducibility check finds these |

It prints the steps that depend on the order first, then those that follow from them, and fails (exit code 1) when a step depends on the order. The made-up files of the simulator have digests too, so a simulated run finds a step whose script lists files in the order they finished even when its output is made up.

**What it found.** A Nextflow `collect()` of three files, whose script `cat`s them: `JOIN` depends on the order; sorted by name, nothing does. Among 16 Snakemake test workflows chosen at random from those that run without special launch settings, run without network: 15 do not depend on the order; `PathoGenOmics-Lab__VIPERA` shows 7 steps whose `run:` blocks get the task id as Snakemake's `jobid`, which depends on the order, so their cache keys change between runs. Six Argo examples with loops and parameter aggregation do not depend on the order. It also found that iunctus's Nextflow `collect(sort: true)` did not sort.

## 6. Tasks without network

A workflow may say whether a task needs the network (`Task.network`): a CWL tool without `NetworkAccess` runs without it (`Denied`), one that asks for it gets it (`Allowed`), and CWL v1.0 tools get it, as with cwltool. Nextflow, Snakemake and Argo say nothing (`Unstated`), and the run decides (`task_runner::NetworkRule`):

- By default, such tasks get the network, as before.
- `-no-network`, or `default = "none"` in `[network]` of the config file, runs them without it.
- `allowed_steps` in `[network]` names steps that get the network in any case, by the name of the step or its last part after `:`, with `*` for any text:

```toml
[network]
default = "none"
allowed_steps = ["DOWNLOAD_*", "fetch_genome"]
```

| Executor | Tasks without network |
|---|---|
| local (Docker, Podman) | yes: network mode `none` |
| SLURM with Podman or Docker | yes: `--network none` |
| SLURM with Apptainer or Singularity | not yet: they cut a container off only where the site allows it |
| TES | not yet: TES has no such setting |
| Kubernetes | not yet: it needs a network policy, which iunctus does not make yet |

A run that asks for tasks without network on an executor that cannot stops before any task, with exit code 2 and a sentence that names why. CWL tools without `NetworkAccess` still run with network there, as before.

Whether tasks run without network by default is an open question of the user ([roadmap](../roadmap.md), section 6). Two more points depend on it: the design of the cache shares a result only when its task needs no network ([the Nextflow runtime](nextflow/runtime.md), section 5.3), which iunctus does not ask yet, since it would stop sharing the results of every Nextflow, Snakemake and Argo task; and a result made with the network answers a task without network, as the network is not part of the cache key.

## 7. Where it is in the code

| Part | Code |
|---|---|
| The records of a run | `run-database/src/schema.rs` (version 4), `pinned.rs`, `recorder.rs`; `iunctus/src/sources.rs`, `tracked.rs` (the Git index), `fetched.rs` |
| Remote reads | `storage/src/journal.rs`, `downloads.rs` (`Storage::{fetched, pin, copy_of}`, `Metadata.version`) |
| Changes and repeats | `iunctus/src/changes.rs`, `repeat.rs`; `task_runner::Runner::pin_images` |
| The reproducibility check | `task-runner/src/check.rs`, `runner.rs`; `content_store::Store::{mark_unreproducible, unreproducible}`; `run-page/assets/run.js` |
| The order check | `run-database/src/steps.rs`, `iunctus/src/order.rs`; digests of made-up files in `workflow-graph/src/simulator.rs` |
| Tasks without network | `workflow_graph::Network`, `task-runner/src/network.rs`, `iunctus/src/setup.rs` (`[network]`), `main.rs` |
| The RO-Crate | `run-crate/src/pinned.rs` |

## 8. Checks

- `run-database/tests/pinned.rs`: a run reads back its sources, remote files, images, output schema, input digests and checks; a database of version 3 gets the new tables. `run-database/tests/steps.rs`: steps that differ in how tasks are made, in what they read, in their number, and in their bytes.
- `task-runner/tests/runner.rs`: the check with a fake backend that gives the same bytes, then one that gives other bytes each time: the tool is marked, a later run runs it, another step with the same script still shares; images pinned to given digests. Unit tests: the choice of tasks (about 10% of 1,000 keys for `10%`), verdicts, tools, the network rule.
- `storage`: the version of files of web servers, folders and object stores; notes and pins of remote reads against a local web server; passwords of addresses never noted.
- `iunctus`: the Git index of versions 2, 3 and 4 made by Git; the files of a folder outside Git; `iunctus/tests/reproducibility.rs`: a simulated run records its sources, named file, remote sample sheet with its ETag; `changes` finds a changed workflow and sample sheet; the order check names `JOIN`; executors that cannot run tasks without network say so. With Docker (ignored, run without network in `unshare --map-current-user -n --keep-caps`): `-check-reproducibility 100%` gives `same` for `COUNT` and `differs` for `STAMP`, which then runs in every run while `COUNT` comes from the cache; after the workflow and the file on the web changed, `iunctus repeat` restores both and counts the 12 bytes the first run read; the exported crate holds the workflow as it ran; a task sees only `lo` with `-no-network`, and also `eth0` without it or when the config allows its step.
- `run-crate/tests/export.rs` and `validation.txt`: the crate with its files of the workflow, named file, remote file and output schema is a valid `provenance-run-crate-0.5` at the level REQUIRED (83 of 83 checks). At the level RECOMMENDED, 172 pass and 8 fail: the 6 of before, the web address of the fixture, which the validator could not fetch without network, and the formal parameters of outputs, which no file of the run names as its `exampleOfWork` yet.

## 9. Not yet

- **Local input data** stays where it is: the store keeps no copy of a local input file that no task made, so a repeat reads it as it is now, and `changes` tells when it changed. A switch to keep them would make repeats exact for them too.
- **Images** are pinned by digest but not kept: a repeat needs the registry, or the local engine, to still have the image. Python packages of decision 19, Snakemake wrappers and Conda environments are not pinned yet.
- **The versions of remote files** come with downloads from web servers and folders; object stores and FTP give none with a download, so `changes` compares their size.
- **Environment variables** that workflow code reads, and the config file of iunctus, are not recorded (the config file may hold secrets).
- **Formal parameters of outputs** are not yet linked to the files of a run.
- **The reproducibility check** compares whole files; a file with a time stamp inside differs as a whole. Tools could be compared by content without their headers later.
