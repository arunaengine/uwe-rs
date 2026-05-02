<!-- How `iunctus run` runs a workflow of any language: detection, drivers, the execution graph and executors. -->
<!-- Copyright (c) 2026 The iunctus Contributors -->
<!-- SPDX-License-Identifier: MIT or Apache-2.0 -->

# The engine

Status: implemented for Nextflow, 2026-09-28. Snakemake, CWL and Argo plug in at one place (section 3). This applies [decisions](../decisions.md) 15 and 16.

`iunctus run <target>` works the same way for every language:

1. It finds the language of the target (section 2).
2. It turns the command line into a `Launch`, the options all languages share.
3. It asks the language's crate for a `Driver` for that launch.
4. `workflow_graph::execute` runs the driver against an `Executor` and records the execution graph (section 5).
5. With `-graph <file>` it writes the graph; `iunctus graph <target>` prints it.

The engine, the driver interface, the graph and the mock executors live in the `workflow-graph` crate, which knows no language. Language crates depend on it, never the other way round.

## 1. The command line

| Option | Meaning | `Launch` field |
|---|---|---|
| `<target>` | workflow file or project directory | `target` |
| arguments after the target, `-entry <name>` | Snakemake targets, the Nextflow entry workflow, the Argo entrypoint | `targets` |
| `-lang nextflow\|snakemake\|cwl\|argo` | the language, when the target does not tell it | (chooses the driver) |
| `--<name> <value>`, `--config <name>=<value>...` | workflow parameters, in the order given | `params` |
| `-inputs <file>`, `-params-file <file>` | a CWL job file or a params file | `inputs` |
| `-profile <names>` | Nextflow profiles | `profiles` |
| `-c <file>`, `-configfile <file>` | extra config files | `configs` |
| `-stub` | run the stubs of tasks | `stub` |
| `-executor mock-tes\|mock\|local` | the executor (section 4) | (chooses the executor) |
| `-graph <file>` | write the graph, as DOT for a `.dot` name, else JSON | |
| `-format json\|dot\|summary` | the output of `iunctus graph`; `summary` by default | |

Parameter names reach the driver as written; Nextflow turns `--max-cpus` into `params.maxCpus`, as `nextflow run` does. `iunctus config` stays Nextflow only.

## 2. Detection

`-lang` wins. Otherwise:

| Target | Language |
|---|---|
| a directory with `main.nf` or `nextflow.config`, a `.nf` file | Nextflow |
| a directory with `Snakefile`, `snakefile`, `workflow/Snakefile`, `workflow/snakefile` or a `.smk` file; a file with one of those names, a `.smk` or `.snakefile` file | Snakemake |
| a `.cwl` file; JSON or YAML with a top-level `cwlVersion` | CWL |
| JSON or YAML with an `apiVersion` of `argoproj.io/...` | Argo |

A directory with both a Nextflow and a Snakemake workflow, or a file that matches nothing, is an error that asks for `-lang`.

## 3. Drivers

A driver is a state machine (`workflow_graph::Driver`): `start` loads the workflow and returns the first tasks, `complete(outcome)` returns the tasks that can run now, and `finish` checks that nothing waits for input that never comes. Each language crate exposes

```rust
pub fn driver(launch: &Launch) -> Result<Box<dyn Driver>, DriverError>
```

and the binary picks one in `driver` in `iunctus/src/main.rs`, a single match on `Language`. Snakemake, CWL and Argo return an unsupported error there until `snakemake_runtime::driver`, `cwl_runtime::driver` and `argo_runtime::driver` replace those three lines. A `DriverError` with `unsupported` set ends the command with exit code 33.

**The Nextflow driver** (`nextflow_runtime::driver`) loads the config (project, launch directory and `-c` files, profiles, the params file and the command-line params over it), runs `manifest.mainScript` or `main.nf` of a project directory, and starts the dataflow network. The binary gives it a host that downloads remote files into the cache. Each Nextflow task becomes a container task:

- `image` from the `container` directive, `command` from the `shell` directive (default `/bin/bash -ue`) with `.command.sh`;
- `script` is the task script; the `.command.sh` input holds it with the code that captures `env` and `eval` outputs into `.command.env`, which is then an output too;
- each staged file is an input with its source path or URL;
- `resources` from `cpus`, `memory`, `disk`, `time` and `accelerator`, `environment` from the config's `env` scope.

Outcomes go back the other way: output files by name, standard output, and the `env` and `eval` values read from `.command.env`. A value that a task did not write, as with a mock, is empty. `-entry` is not supported yet.

## 4. Executors

| `-executor` | What it does |
|---|---|
| `mock-tes` (default) | Writes the TES v1.1 task document of each task to `<cache>/scratch/mock-tes/<run>/tes/<id>.json` and pretends to run it in `work/<id>/`. |
| `mock` | Only pretends, in `<cache>/scratch/mock/<run>/<id>/`. |
| `local` | Runs each task in Docker or Podman (`IUNCTUS_ENGINE`) through the task runner, which reuses results from the cache store (`-store`). Remote and directory inputs, and standard input, fail as unsupported. |

**Pretending** (`workflow_graph::mock::pretend`) writes inputs with literal content, then makes each output: the names in the script that the output pattern matches (targets of `>` and `>>`, arguments of `touch` and `mkdir`, the last argument of `mv`, `cp` and `ln`, files inside a directory output), else one name made from the pattern (`*` becomes `mock`). A `.gz` file is an empty gzip file, and `versions.yml` gets the one entry nf-core code reads. The names come from one call in `pretend`, the place where a simulator of the script can take over.

## 5. The execution graph

`execute` records each task when the driver asks for it, so the graph grows during the run. A node is the task and its exit code. An edge is `file` when a task reads a file (or a file inside a directory) that an earlier task made, found by the local path of the output, or `order` for the tasks a task lists in `after`. `ExecutionGraph::check` confirms that every edge joins known tasks and points forward. A run that fails keeps the graph so far; `-graph` still writes it and `iunctus graph` still prints it.

`iunctus graph` sends what the workflow prints to standard error, so standard output holds only the graph. The formats are the JSON of `ExecutionGraph`, Graphviz DOT, and a summary with one line per task and its upstream tasks.

## 6. Checks

- `workflow-graph/tests/engine.rs`: a toy driver, the file edges, the TES documents, and what the mocks make.
- `nextflow-runtime/tests/driver.rs`: a two-process pipeline through the engine, the `env` scope, the params file.
- `iunctus/tests/graph.rs`: `iunctus graph` and `iunctus run -graph` on a two-process pipeline; unit tests in `iunctus/src/detect.rs` cover detection.
- `just run-test-workflows-test` runs every nf-core pipeline with the mock TES executor and checks the graph of each run that succeeds.
