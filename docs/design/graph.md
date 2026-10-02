<!-- The graphs of a run: tasks and steps, simulated graphs, markers, formats and the SVG layout. -->
<!-- Copyright (c) 2026 The iunctus Contributors -->
<!-- SPDX-License-Identifier: MIT or Apache-2.0 -->

# The graphs of a run

Status: implemented in `workflow-graph` and the four drivers, 2026-09-30. This applies [decisions](../decisions.md) 15 and 21. How the engine records the graph is in [the engine](engine.md), section 6.

Every run gives two graphs: the graph with one node per task, which the engine records while the run goes on, and the graph with one node per step, which is made from it. A run of the simulator gives a simulated graph: it says so everywhere, and it marks the points where a run with real data can have more or fewer tasks.

## 1. The two graphs

**The graph of tasks** (`ExecutionGraph`) has a node per task that the driver asked for, in the order asked, with its exit code and whether an earlier result answered it (`Node.reused`, from the cache store or from an earlier run that `-resume` continues), and an edge per file that a task read from an earlier task, or per task that it waits for (engine section 6).

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

The pictures, DOT and SVG, follow two more options. `-direction down` (the default) draws the graph from top to bottom, `-direction right` from left to right. `-fold <n>` folds each step with more than n tasks into one box in the graph of tasks, 5 by default; `-fold 0` draws every task. A wide graph folds more steps: when more than 16 boxes (`render::WIDTH`) would stand in a row, as when every step of a pipeline has four samples, steps with fewer tasks than n fold too, from n down to 2 tasks, as far as the widest row needs. The picture says so above the graph. JSON and the summary always hold every task.

| Format | What it holds |
|---|---|
| `summary` (the default) | the simulation note, counts (tasks, steps, edges and marked points), then one line per task or step with what it comes after, and one line per marker under it, starting with `more tasks may follow from real data` in a simulated graph |
| `json` | the `ExecutionGraph` or `StepGraph` with serde: `simulation`, `nodes` with `markers`, or `steps` with `tasks`, `succeeded`, `failed`, `marked` and `markers` |
| `dot` | Graphviz DOT with the title as the graph label, the colours of the states, file names on the edges, and dashed open points |
| `svg` | a picture drawn by iunctus itself (section 5), with no Graphviz, in the look of the web page of runs |

In DOT and SVG, a simulated graph has a dashed box `more tasks may follow` beside each marked task or step, joined by a short dashed line; its tooltip says `more tasks may follow from real data` and gives the reasons.

## 5. Drawing and layout

`ExecutionGraph::drawing(&render::Options)` turns the graph into a `render::Drawing`: boxes (`Shape`, with an id, a kind, lines of text, a tooltip and a state) and lines between them (`Link`, dashed or not, with a label). `render::dot` and `render::svg` write any drawing; `render::render(graph, &options, format)` does both steps. The options are the view, the direction, the fold (5 by default, `render::FOLD`), the steps that are open although they are large, and the states of tasks where the caller knows them better than the exit codes do, as the run database does for a run that goes on.

**Boxes.** A box stands for one task (`Kind::Task`), for all tasks of a step (`Kind::Step`: a box of the graph of steps, or a large step of the graph of tasks that is folded into one box), or for an open point (`Kind::OpenPoint`). The ids are `t<task id>` for a task, `s<n>` for the n-th step in the order of their first task (counted from 0), `m<task id>` for the open point after a task and `ms<n>` for the one after a step. The lines between a folded step and other boxes stand for all task edges between them: dashed when none passes a file, with the number of task edges as label. When more than 16 lines run into one box, as from an open step of 200 tasks into the next folded step, they become one thick line with the count as label (`Link.bundled`): the line of the middle source stays, and the others are hidden (`Link.hidden`), so that the layout still puts the box under all its sources; DOT draws them as invisible lines.

**States.** A task shows one of seven states: simulated, succeeded, reused (an earlier result answered it), failed, running, waiting and stopped. From the graph alone, a task without an exit code is waiting; the run database also knows which tasks run and which were stopped. A box of a step shows a failure first, then work that goes on (running, waiting), then how its tasks ended; its second line counts the tasks, like `24 tasks`, or `24 tasks: 20 succeeded, 4 running` when they are not all in the state of the box.

**Labels** are short: a name qualified with `:`, like Nextflow's `NFCORE_RNASEQ:RNASEQ:FASTQC (WT_REP1)`, keeps its last part and its tag (`FASTQC (WT_REP1)`), a step its last two parts, and a label longer than 40 characters is cut. The full name is in the tooltip. Labels use a monospace font, so that their width is known without measuring text.

**The SVG look** follows the graph view of Argo Workflows (`render/svg.rs`, `render/icons.rs`). A task is a rounded box with a round icon of its state and its name: a check on green, a turning three-quarter circle on blue while it runs, a cross on red, a clock, a square on amber when stopped, an arrow that comes back round on purple when reused, and a dashed circle with a check when simulated. A step is a box with its name in bold and its counts, with a second box behind it when it has several tasks. An open point is a dashed amber box `more tasks may follow` beside the box before it, joined by a short dashed line. Lines are smooth curves that leave and reach boxes along the direction of the layout. The title is on top, and a legend with the icons of the states that the picture shows is below. Each icon is a `<symbol>` that the boxes use as `#icon-<state>`, and the colours are CSS variables with fixed defaults, so that the web page can change the state of a box in place and give the picture its dark look. The web page reads the classes and `data-` attributes of the boxes: `node task|step|open-point <state>`, `data-task`, `data-step`, `data-tasks` and `data-host`.

`layout::layout(&layout::Graph)` places the boxes in layers, in the steps of Sugiyama's method:

1. The nodes are put in a depth-first order (the reverse postorder of a search that starts at the nodes in their order), in which every edge points from a node to a later one, except the edges that close a cycle. Those are laid out turned round. The graph of tasks has no cycles; the graph of steps can have them.
2. Layers by the longest path from a node without predecessors; then each node with more successors than predecessors, like a node without predecessors, moves down to just above the first of its successors, which shortens more edges than it lengthens. A node with one input and one output stays next to its input, so the graph reads as a tree from the first tasks to the last.
3. An edge that spans several layers gets a point in each layer between, so that it passes between the boxes there. Beyond 200,000 such points, edges over more than 16 layers go straight.
4. The order in each layer: sweeps down and up sort each layer by the mean position of its neighbours (the barycenter), at most 12 times, keeping the order with the fewest crossings, which a Fenwick tree counts.
5. The places in each layer: each box moves towards the mean place of its neighbours, as close as the order and the gaps allow. This is an isotonic regression, solved by pooling adjacent violators, in alternating sweeps. The points of long edges weigh four times as much as boxes, so that long edges run straight and the boxes make way, instead of long edges sweeping around the layout.

A node can sit beside another one instead of in a layer of its own (`Graph.beside`): it takes room after that node in its layer, and the line between them is short and straight. The SVG places each open point so. `Direction::Right` lays the graph out from left to right: it is the layout down with widths and heights swapped, turned at the end, so the nodes beside another sit below it.

The layout is deterministic: the same graph gives the same SVG. Its unit tests check that boxes do not overlap, that the output does not change, that long edges pass their layers and run straight, which nodes move down, that back edges are drawn, that a layout to the right is the one down turned, where nodes beside others sit, and that a graph of 5,000 nodes takes well under a second.

## 6. Checks

- `workflow-graph/tests/engine.rs`: a driver whose notes read a task output, spread the files of a wildcard output and mark a checkpoint; the markers, the step graph, the summary, DOT and SVG of a simulated and of a real graph.
- `workflow-graph/src/layout_tests.rs`, and the unit tests of `render::drawing` (folded steps, their counts and states, the view of steps, known states, open points) and `render::svg` (boxes, lines, legend, open points beside their box in both directions).
- `iunctus/tests/graph.rs`: the note on standard error, `-view steps`, `-format svg`, `-direction right`, `.svg` files.
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

## File descriptions: workflow-graph

### `workflow-graph/src/executor.rs`

The interface between the engine and the executors that run tasks: the simulator, local
containers, TES and clusters. An executor starts an attempt of a task and gives back a
future of its outcome, which the engine drives on Tokio while the driver waits on its own
thread.

### `workflow-graph/src/files.rs`

The files that tasks made: reading them on this machine, and delivering them to where a
language expects its results, like Nextflow's `publishDir`, the working directory of
Snakemake, or the output folder of a CWL run.

### `workflow-graph/src/layout.rs`

A layered layout that draws a graph from top to bottom, or from left to right, as Graphviz
`dot` does, in the steps of Sugiyama's method:

1. The nodes are put in a depth-first order, in which every edge points from a node to a
   later one, except the edges that close a cycle. Those are laid out turned round.
2. Layers by the longest path from a node without predecessors. Then each node with more
   successors than predecessors, like a node without predecessors, moves down to just above
   the first of its successors, which shortens more edges than it lengthens.
3. An edge that spans several layers gets a point in each layer between, so that it can
   pass between the nodes there.
4. The order in each layer: sweeps down and up that sort each layer by the mean position of
   its neighbours (the barycenter), a bounded number of times, keeping the order with the
   fewest crossings.
5. The places in each layer: each node moves towards the mean place of its neighbours, as
   close as the order and the gaps between nodes allow (an isotonic regression). The points
   of long edges weigh more than nodes, so that long edges run straight and nodes make way.

A node can sit beside another one instead of in a layer of its own, like the open point
after a marked task: it takes room after that node in its layer. A layout from left to
right is a layout from top to bottom with widths and heights swapped, turned at the end.

Everything is deterministic: the same input gives the same layout. The work grows with the
number of nodes, edges and layers, so graphs of thousands of tasks stay fast.

### `workflow-graph/src/lib.rs`

The language-neutral side of a run. Every language driver (Nextflow, Snakemake, CWL, Argo)
turns its program into concrete [`Task`]s; [`execute`] hands them to an [`Executor`] and
records the [`ExecutionGraph`] of the run: every task that ran, and which task produced the
files another one read.

The graph grows while the run goes on, because Nextflow channels, Snakemake checkpoints,
CWL scatter over outputs and Argo `withParam` only know their tasks at run time.

### `workflow-graph/src/marker.rs`

Markers: the points of a graph where the tasks that follow depend on data, so that a run
with real data can have more or fewer tasks there than a simulated run (decision 21).
Drivers report what they notice while they plan ([`Note`]); the engine turns the notes into
markers on the tasks whose outputs they concern.

### `workflow-graph/src/render/drawing.rs`

The drawing of a graph: a box per task, where a step with many tasks can be folded into one
box that counts its tasks, or a box per step. In a simulated graph, each marked task or step
gets a dashed box beside it: more tasks may follow from real data.

### `workflow-graph/src/render/icons.rs`

The round icons of the states and of open points, as SVG symbols. Each box refers to the
icon of its state as `#icon-<state>`, so that the web page changes the state of a box by
changing one reference. The colours are those of the web page, which it can change for its
dark look through CSS variables of the same names.

### `workflow-graph/src/render/mod.rs`

Pictures and texts of a graph. The graph with one node per task or one node per step
becomes a [`Drawing`]: boxes with text and a state, and lines between them. In the graph of
tasks, a step with more tasks than [`Options::fold`] becomes one box that counts its tasks,
unless it is open; more steps fold when the graph would be wider than [`WIDTH`] boxes. Many
lines into one box become one line with a count. [`dot`] writes a drawing as Graphviz DOT, and [`svg`] draws it as SVG with
the layered layout of [`crate::layout`], with no program besides iunctus. The web page of
runs shows the same SVG. [`render`] gives a graph in each view and format.

### `workflow-graph/src/render/svg.rs`

A drawing as SVG, laid out by [`crate::layout`] down or to the right. Each task is a rounded
box with the round icon of its state and its name; each step, or step folded into one box,
is a box with its name and how many tasks it has in which states, stacked when it has
several; each open point of a simulated graph is a dashed box beside the task or step before
it. Smooth lines join the boxes. The title is on top and a legend of the states below.

The web page of runs shows the same picture and makes it move, zoom and follow a run: it
reads the classes and `data-` attributes of the boxes and turns the icons of `#icon-<state>`.
Names use a monospace font, so that their width is known without measuring text.

### `workflow-graph/src/simulator/mod.rs`

The simulator: a task does not run, it only makes the files it would make, and the simulator
writes the GA4GH TES v1.1 task document that a TES server would receive. So a whole workflow
runs through without containers or a server, and the TES documents can be checked or sent
later. The files are made up, so a simulated run gives a simulated graph, not a prediction
of a real run (decision 21).

### `workflow-graph/src/simulator/shell/mod.rs`

A small simulation of the shell scripts of tasks, for the simulator: which files a
script makes and what text it writes into them. It follows the commands that stub scripts
use to make their files, like `touch`, `mkdir`, `echo`, `printf`, `cat` with a here
document, `cp`, `mv`, `gzip`, `for` loops over words and `if` statements whose condition
it can decide. Anything else, like a tool, makes the simulation incomplete: the script may
make more files than it tells.

### `workflow-graph/src/simulator/shell/words.rs`

Word expansions of the shell for the simulator: variables, brace
expansion, the escapes of `echo -e` and `printf`, and paths relative to the working
directory.

### `workflow-graph/src/steps.rs`

The graph with one node per step (process, rule, step or template), made from the graph with
one node per task: a step node counts its tasks, and a step edge stands for the task edges
between two steps.

### `workflow-graph/tests/attempts.rs`

Attempts run on Tokio at the same time: the engine hands their outcomes to the driver as they
end, runs a task again after a failure of the machine or when the driver asks, and stops
the attempts in flight when the run is cancelled.

### `workflow-graph/tests/order.rs`

The engine keeps the order rules of a run: a task starts only after the tasks in its `after`
list have finished, and it may only name tasks that the driver asked for before. With run
options, several tasks are pending at once and finish in another order.
