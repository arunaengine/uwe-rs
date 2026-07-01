<!-- The graphs of a run: tasks and steps, simulated graphs, markers, formats and the SVG layout. -->
<!-- Copyright (c) 2026 The iunctus Contributors -->
<!-- SPDX-License-Identifier: MIT or Apache-2.0 -->

# The graphs of a run

Status: implemented in `workflow-graph` and the four drivers, 2026-09-30. This applies [decisions](../decisions.md) 15 and 21. How the engine records the graph is in [the engine](engine.md), section 6.

Every run gives two graphs: the graph with one node per task, which the engine records while the run goes on, and the graph with one node per step, which is made from it. A run of the simulator gives a simulated graph: it says so everywhere, and it marks the points where a run with real data can have more or fewer tasks.

## 1. The two graphs

**The graph of tasks** (`ExecutionGraph`) has a node per task that the driver asked for, in the order asked, with its exit code, and an edge per file that a task read from an earlier task, or per task that it waits for (engine section 6).

**The graph of steps** (`StepGraph`, `ExecutionGraph::step_graph`) has a node per step: a Nextflow process, a Snakemake rule, a CWL step or an Argo template, as `Task.step` names it. A step node counts its tasks, those that succeeded and those that failed, and carries the markers of its tasks (section 3). A step edge stands for the task edges between two steps: how many there are, and whether any of them passes a file. Edges between tasks of the same step are left out. The steps keep the order of their first task, and the step graph can have cycles, as when tasks of two steps take turns.

## 2. Simulated graphs

The simulator runs no task: it makes up the files that a task would make (engine section 4). Such a graph shows what the workflow plans from made-up files, not a prediction of a real run (decision 21). So a simulated graph says, in plain words, that no task ran, that the outputs are made-up files, and that a run with real data refines the graph:

| Where | How |
|---|---|
| Standard error of `iunctus run` and `iunctus graph` | `iunctus: This is a simulation: no task ran, and the outputs are made-up files. A run with real data refines this graph.`, after the error of a failed run, and how many points the graph marks |
| JSON of both graphs | the field `simulation`, which holds the same sentence; graphs of real runs have no such field |
| Summary | the sentence as the first line |
| DOT and SVG | the sentence as the second line of the title |

The binary sets it for every run with the simulator (`ExecutionGraph::simulate`); the sentence is `workflow_graph::SIMULATION`. Standard output stays free for the graph.

## 3. Markers

A marker (`Marker`, in `Node.markers`) is a point where the tasks that follow a task depend on its outputs: on their content, or on their number. In a simulated graph those outputs are made up, so a run with real data can have more or fewer tasks after this point. A marker has a kind and a short reason:

| Kind | Meaning | Example reason |
|---|---|---|
| `content` | The workflow read the content of an output of the task, or listed an output folder, while it planned the next tasks | the workflow read `samplesheet.valid.csv`, an output of this task, to plan the next tasks |
| `wildcard` | An output pattern with a wildcard matched files that each started tasks of their own | the output pattern `*.fastq.gz` matched 1 file, and each started tasks of its own |
| `checkpoint` | A Snakemake checkpoint, after which Snakemake plans the workflow again | rule `split` is a checkpoint: Snakemake plans the workflow again after it |

### 3.1 How markers come about

Drivers tell the engine what they notice while they plan, as notes (`Note`, from `Driver::notes`, which the engine asks for after `start` and after each `complete`, also when they fail). There are three kinds of notes, and only the third is specific to a language:

- `Read(path)`: the workflow read the content of this file, or listed this folder. The engine looks up which task made the file, by its location, as it does for the file edges. A read of a file that no task made, like a sample sheet of the user, marks nothing: a fan-out over a real sample sheet is exact. A read of a task output gives a `content` marker on that task.
- `Spread(path)`: each file like this one started tasks of its own. The engine marks the task that made the file only when an output pattern of that task with a wildcard matched it: a pattern without a wildcard names exactly one file, so the number of tasks cannot change there.
- `Mark(task, marker)`: a marker that only the driver can find.

Markers are recorded in every run, simulated or real. In a real run they are facts about the plan: these tasks depend on these outputs. Only a simulated graph draws the open points of section 4.

### 3.2 What each driver notes

| Language | Reads (`content`) | Spreads (`wildcard`) | Its own markers |
|---|---|---|---|
| Nextflow | every file that the workflow reads through the host (`text`, `readLines`, `splitCsv` and the other splitters, `parseJson` of a file, `countFastq`, ...) and every folder it lists (`files()`, `listFiles()`, `Channel.fromPath` with a glob) | the files that `flatten`, `flatMap` and `transpose` turn into items of their own, and the values of `each` inputs | none |
| Snakemake | every task output that workflow code reads or lists while it is planned (input functions, `checkpoints.<rule>.get()` with `glob_wildcards`, `pandas.read_csv`, `open`) | none: Snakemake outputs have no wildcards of the shell | `checkpoint` on each task of a checkpoint rule |
| CWL | none as reads: a tool's outputs, `loadContents` and `outputEval` are values, not plans | the files of a scatter over a step output, like the `File[]` of a glob | `content` on the tasks of the step whose output a scatter goes over when it holds no files, looking back through steps without tasks, like expression tools |
| Argo | none as reads: output parameters are values | none | `content` on the tasks of the steps or tasks whose outputs a `withParam`, a `withSequence` or a `when` reads |

What is not marked: values that the driver reads for its own work, like Nextflow's `.command.env` or a CWL `cwl.output.json`, unless the workflow plans from them as above; the standard output of a task and Nextflow `env` and `eval` outputs, whose values are not files; and `exists()` or `size()` checks.

## 4. Formats

`iunctus graph <target>` prints a graph, `iunctus run <target> -graph <file>` writes one. `-view tasks` (the default) gives the graph of tasks, `-view steps` the graph of steps. `-format` chooses the format of `iunctus graph`, and the name of a `-graph` file chooses its format: DOT for `.dot`, SVG for `.svg`, a summary for `.txt`, else JSON.

| Format | What it holds |
|---|---|
| `summary` (the default) | the simulation note, counts (tasks, steps, edges and marked points), then one line per task or step with what it comes after, and one line per marker under it, starting with `more tasks may follow from real data` in a simulated graph |
| `json` | the `ExecutionGraph` or `StepGraph` with serde: `simulation`, `nodes` with `markers`, or `steps` with `tasks`, `succeeded`, `failed`, `marked` and `markers` |
| `dot` | Graphviz DOT with the title as the graph label, the colours of the states, file names on the edges, and dashed open points |
| `svg` | a picture drawn by iunctus itself (section 5), with no Graphviz |

In DOT and SVG, a simulated graph has a dashed box `more tasks may follow from real data` after each marked task or step, joined by a dashed line; its tooltip gives the reasons.

## 5. Drawing and layout

Both graphs become a `render::Drawing`: boxes (`Shape`, with an id, lines of text, a tooltip and a state) and lines between them (`Link`, dashed or not, with a label). `render::dot` and `render::svg` write any drawing, so the web page of runs can draw its graphs the same way. The states have fixed colours: simulated (grey), succeeded (green), from cache (blue), failed (red), not finished (white), and the open point (amber, dashed).

Labels are short: a name qualified with `:`, like Nextflow's `NFCORE_RNASEQ:RNASEQ:FASTQC (WT_REP1)`, keeps its last part and its tag (`FASTQC (WT_REP1)`), a step its last two parts, and a label longer than 40 characters is cut. The full name is in the tooltip. Labels use a monospace font, so that their width is known without measuring text.

`layout::layout` places the boxes in layers from top to bottom, in the steps of Sugiyama's method:

1. Edges point from a node to a later one; an edge that points back is laid out turned round, so the order of the nodes is a topological order. The graph of tasks never has such edges.
2. Layers by the longest path from a node without predecessors; then each node with successors moves down to just above the first of them, which shortens long edges.
3. An edge that spans several layers gets a point in each layer between, so that it passes between the boxes there. Beyond 200,000 such points, edges over more than 16 layers go straight.
4. The order in each layer: sweeps down and up sort each layer by the mean position of its neighbours (the barycenter), at most 12 times, keeping the order with the fewest crossings, which a Fenwick tree counts.
5. The places in each layer: each box moves towards the mean place of its neighbours, as close as the order and the gaps allow. This is an isotonic regression, solved by pooling adjacent violators, in alternating sweeps.

The layout is deterministic: the same graph gives the same SVG. Its unit tests check that boxes do not overlap, that the output does not change, that long edges pass their layers, that back edges are drawn, and that a graph of 5,000 nodes takes well under a second.

## 6. Checks

- `workflow-graph/tests/engine.rs`: a driver whose notes read a task output, spread the files of a wildcard output and mark a checkpoint; the markers, the step graph, the summary, DOT and SVG of a simulated and of a real graph.
- `workflow-graph/src/layout_tests.rs`, and the unit tests of `render::drawing` and `render::svg`.
- `iunctus/tests/graph.rs`: the note on standard error, `-view steps`, `-format svg`, `.svg` files.
- The markers of each driver have their own tests: `nextflow-runtime/tests/driver.rs` (a read, a `flatten` over a wildcard output, a literal output and a sample sheet of the user), the `checkpoint` and `commands` workflows of `snakemake-runtime/tests/workflows.rs`, `cwl-runtime/tests/driver.rs` (a scatter over a glob and over a list without files) and `argo-runtime/tests/run.rs` (`withParam` and `when`).

## 7. Results on the test workflows

All test workflows ran with markers on 2026-09-30, with the same outcomes as before. The runs with markers, and the markers by kind:

| Language | Runs | Runs with markers | Marked tasks | Markers by kind |
|---|---|---|---|---|
| Nextflow (nf-core and WorkflowHub, stub runs) | 345 workflows, 163 of which run | 121 (90 of the runs that succeed) | 442 of 6,611 tasks | 284 content, 215 wildcard |
| Snakemake | 892 workflows, 667 of which run | 58 (43) | 126 of 226,315 | 115 checkpoint, 95 content |
| CWL graphs | 5,977 documents, 5,891 of which run | 243 (228) | 1,031 of 16,841 | 938 content, 656 wildcard |
| CWL with example jobs | 807 documents, 699 of which run | 13 (10) | 168 of 2,316 | 109 content, 149 wildcard |
| Argo | 1,345 documents with an entrypoint, 1,178 of which run | 117 (117) | 192 of 14,762 | 251 content |

Typical points: the `SAMPLESHEET_CHECK` task of older nf-core pipelines, whose checked sample sheet the pipeline reads (a real run can have more samples); the trimming reports of TRIMGALORE in nf-core/rnaseq, from which the pipeline drops samples with too few reads (the simulated run drops all five, a real run keeps them); `UNTAR` tasks whose folders a pipeline lists; split steps like `SEQKIT_SPLIT2`, `BAMTOOLS_SPLIT` or GATK `SplitIntervals` in CWL, whose files each start tasks; Snakemake checkpoints and the folders their input functions list; Argo `withParam` over a step's `outputs.result` and `when` over the flip of a coin. A per-sample fan-out over the sample sheet of the user is never marked.

Known limits: a spread whose items are collected into one task that the driver asks for in the same call, like a report of all files at the end of a run, is still marked; and a plan that follows the standard output of a task, or a Nextflow `env` or `eval` output, is not marked, as those are values and not files.
