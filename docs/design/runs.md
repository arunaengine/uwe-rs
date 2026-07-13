<!-- How iunctus records every run, resumes runs, shows them on a web page and exports them as RO-Crates. -->
<!-- Copyright (c) 2026 The iunctus Contributors -->
<!-- SPDX-License-Identifier: MIT or Apache-2.0 -->

# Runs: the run database, resume, the web page and RO-Crate

Status: implemented, 2026-09-30. This applies [decisions](../decisions.md) 10, 23 and 25, and the reuse scopes of the [Nextflow runtime](nextflow/runtime.md), section 5.3.

Every `iunctus run` and `iunctus graph` records itself in one SQLite file, the run database, in the same form for all four languages and for simulated and real runs. Three features read it:

- **Resume:** `iunctus run <workflow> -resume` continues the last run of the workflow without running its finished tasks again.
- **The web page:** `iunctus run -page` shows the run live in the browser, and `iunctus page` shows any past run.
- **RO-Crate:** `iunctus export <run>` writes a Workflow Run RO-Crate of the Provenance Run Crate profile, with the digest of every file.
- **Insight:** `iunctus why <run> <task>` tells why a task ran again, and `iunctus origin <file>` where a file came from.

| Crate | What it is |
|---|---|
| `run-database` | The schema, the recorder that the engine feeds, and the queries |
| `run-crate` | The export of a run as an RO-Crate |
| `run-page` | The web page: an HTTP server on localhost and the HTML, CSS and JavaScript it serves |
| `iunctus` | `record.rs` starts the record of a run, `resume.rs` answers finished tasks, `page.rs` and `runs.rs` hold the commands |

## 1. The command line

| Command or option | What it does |
|---|---|
| `iunctus runs` | lists the latest 50 runs: id, start, state, executor, number of tasks and failures, workflow |
| `iunctus page [<run>]` | serves the page of all runs, or opens one run, until Ctrl-C |
| `iunctus export <run>` | writes the RO-Crate of a run to `-output <folder>`, by default `<run>-crate` |
| `iunctus why <run> <task>` | what changed since the same task ran before, or where its result came from when it did not run (section 6) |
| `iunctus origin <file>` | the tasks that made a file, back to the data that no task made; the file is a path or `sha256:<hex>` (section 6) |
| `-resume [<run>]` | continues a run; without an id, the last run of the same workflow |
| `-page` | serves the page of the run that `iunctus run` starts; it stays open after the run until Ctrl-C |
| `-address <ip>`, `-port <number>` | where the page listens: `127.0.0.1` and a free port by default |
| `-database <file>` | another run database than the default |
| `-output <folder>`, `-license <url>` | the folder and the license of an exported crate |

A run id is the start time in milliseconds and the process id, both in hexadecimal, like `1a0f1cc4c10-3a2487`. `-resume` takes the next argument as the run only when it has this form, so `iunctus run -resume main.nf` resumes the last run of `main.nf`.

## 2. The run database

The file is `runs.sqlite` in the cache directory (`$XDG_CACHE_HOME/iunctus`, else `~/.cache/iunctus`), next to the default store. One database serves every workflow and store of the user, so `iunctus runs` lists them all; each run records its store.

**SQLite** through `rusqlite` with the bundled library, so nothing needs to be installed. The database runs in WAL mode: readers, like the page, never wait for a run that writes, and several iunctus processes can share the file. `synchronous = NORMAL` syncs at checkpoints only; a crash can lose the last commits, never break the file. All SQL is plain, so another engine can replace SQLite behind the same interface (decisions, open questions).

**How it is filled.** The engine reports each step of a run to an observer (`workflow_graph::Observer`, owned by the engine): a task was asked for, with its edges; an attempt starts; an attempt ends, with its outcome; the run ends. The `Recorder` of `run-database` is that observer. It writes each event before its effect, as the design of runtime.md section 4.2 asks. Writes gather in one transaction that is committed every 250 milliseconds, by the next write or by a small thread of the recorder, so a run of many short tasks commits rarely and a long task never holds the write lock. A failed write does not stop the run: the recorder keeps the first error and the command prints it at the end.

**Schema, version 1.** Times are milliseconds since the Unix epoch. The full definition with a comment on each column is in `run-database/src/schema.rs`.

| Table | One row per | Columns |
|---|---|---|
| `runs` | run | id, lineage, the run it resumes, workflow (absolute path), language, Git revision, command line and parameters (JSON), executor, simulated, store, process id, start, end, state (`running`, `succeeded`, `failed`), error |
| `tasks` | task the driver asked for | run, id, step, name, the task as JSON, fingerprint, time asked, state (`waiting`, `running`, `succeeded`, `failed`, `stopped`) |
| `attempts` | execution of a task | run, task, number, start, end, exit code, reused (`cache` or the id of an earlier run), image as it ran, retried |
| `files` | input of a task, or output of an attempt | run, task, attempt (none for inputs), role, the path the task sees, location, directory, SHA-256 digest, size |
| `edges` | edge of the execution graph | run, source task, target task, kind (`file` or `order`), path |
| `markers` | point where real data can change the tasks (decision 21) | run, task, kind (`content`, `wildcard`, `checkpoint`), reason |
| `logs` | standard output or error of an attempt | run, task, attempt, stream, SHA-256 of the whole text, its size, the text or its last 64 KiB |
| `events` | step of the run, in the order the engine saw it | run, sequence, time, kind (`requested`, `started`, `finished`, `reused`, `marked`, `ended`), task, attempt |

The schema version is SQLite's `PRAGMA user_version`. Opening a file brings it to the current version with the missing steps of `schema::MIGRATIONS`, in one transaction; a file of a newer version is refused with a clear error. A change of the tables adds a step and raises `VERSION`.

A run whose process has gone while its state is still `running` reads as `stopped`, so the list shows runs that were killed.

**Logs.** The database keeps the whole text of a standard output or error up to 64 KiB, and the end of a longer one. A real run keeps the whole text in its store under the digest, and the page serves it from there.

**Digests.** A literal input gets its digest when the task is asked for. Outputs get digests from the executor once `Outcome` carries them (the execution layer, step 2); until then `iunctus export` computes the digest of each file it copies.

## 3. Resume

`-resume` makes the new run join the lineage of the run it continues: the first run of a chain of resumed runs names the lineage. Before the run starts, iunctus loads the finished tasks of every run of the lineage, by fingerprint. The newest run wins when two have the same task.

**The fingerprint** of a task is the SHA-256 of the task as the driver described it (step, image, command, script, environment, inputs by path and source, outputs, standard input), and of the size and modification time of each local input, as Nextflow's standard cache mode compares them. It leaves out what differs between runs of the same task: its id and `after` list, its name for people, which can hold a number in the order tasks finished (Nextflow's `COUNT (2)`), and its resources, as Nextflow's hash does. So a retry with more memory, or tasks that finish in another order, keep their fingerprints. The outputs of earlier tasks are inputs by their location, so a task after a task that ran again gets a new fingerprint and runs again too.

**Answering a task.** The executor of a resumed run looks up each task by fingerprint first. When it finds a task that succeeded and whose output files are all still there, it hands the recorded outcome to the driver, prints `[resumed] <task> (from run <id>)`, and the database marks the attempt as reused from that run. Otherwise the task runs. A task whose standard output or error the database does not hold in whole runs again, because its driver may read them.

This works for every executor, simulated or real, and covers both reuse scopes of runtime.md section 5.3:

- `shared`: the store answers a task whose image is pinned to a digest, in any run, resumed or not.
- `lineage`: the run database answers any task that finished in a run of the same lineage, also one whose image could not be pinned.

**Checks.** The resumed run must have run the same workflow and must have ended. A real run cannot resume a simulated run, whose files are made up; a simulated run may resume a real one, which shows what is left to run. Without an earlier run of the workflow, `-resume` says so and the run starts from the beginning, as Nextflow does.

Not yet: recorded facts (time stamps and random values answered again, runtime.md section 5.7) and delivering cached results in the old completion order.

## 4. The web page

The page is one HTML file, its style and five scripts, all inside the binary: `page.js` for the helpers, navigation and the list of runs, `run.js` for one run, and `graph.js`, `camera.js` and `boxes.js` for its graph view (section 4.1). There is no build step, no framework and nothing to install. `run-page` serves them with axum on a thread of its own, so the page runs next to a run.

**What it shows.** The list of runs. For one run: its workflow, state, times, executor, revision, parameters and command line; a notice when the run was simulated; the number of tasks in each state as a bar; the graph view, next to a panel of the chosen task or step; a table of steps with the tasks in each state and their time; a table of tasks, with a search and a filter by state and step. The panel of a task shows its command, script, image, markers, standard output and error, inputs (with a link to the task that made each one), outputs with links to download them, environment and attempts; the panel of a step shows its tasks by state, its markers, and buttons that open or fold it in the graph and show its tasks in the table.

**Live.** A running run keeps an EventSource open (Server-Sent Events), and the time of the run in its summary ticks every second between the updates. The server reads new events from the database twice a second and sends one update with the tasks that changed, so a run of thousands of short tasks sends few messages. The page changes the states of the boxes of the graph in place; when tasks were asked for or a marker came (the `graph` flag of the update), it lays the graph out again, at most every two seconds. A browser that reconnects says which event it saw last, and the stream ends after the update that shows the run has ended.

**Routes.** `/`, `/runs/<run>` and `/runs/<run>/tasks/<task>` serve the page; `/api/runs`, `/api/runs/<run>`, `/api/runs/<run>/tasks/<task>`, `/api/runs/<run>/events`, `/api/runs/<run>/graph.svg`, `/api/runs/<run>/tasks/<task>/why`, `/api/runs/<run>/tasks/<task>/logs/<attempt>/<stream>` and `/api/runs/<run>/tasks/<task>/{inputs,outputs}/<n>` serve JSON, logs and files. `graph.svg` takes `view=tasks|steps`, `direction=down|right`, `open=<steps, one name per line>` and `fold=<n>`.

**Safety.** The page listens on `127.0.0.1` unless `-address` says otherwise, and then warns that everyone who can reach the machine sees the runs. It serves only the files that the database lists for a task, by their place in the list, never a path that a request names; a file inside a directory output must stay inside it, also through links.

**Fast on large runs.** The first view reads all task rows once; the task table shows 300 rows and more on request, in the order asked or the longest first; the graph folds large steps (section 4.1). A run of 10,000 simulated tasks records itself in 2.4 seconds in all, into 19 MB, and its first view is 1.7 MB of JSON.

### 4.1 The graph view

The graph is the heart of the page: a large view of the run as a tree from the first tasks to the last, in the look of the graph view of Argo Workflows (graph.md, section 5).

**One picture, one layout.** The server draws the graph with the renderer of `iunctus graph` (`workflow_graph::render`, as SVG) from the tasks, exit codes, edges and markers of the database (`Database::graph`), and with the states that the database knows (waiting, running, stopped, reused). So the page and an exported `iunctus graph -format svg` look the same, and the layout is computed once, in Rust. The positions come with the picture, as the places of its boxes; the page has no layout of its own. It keeps only the graph part of the picture and drops its title and legend, which the page shows itself.

**Moving and zooming** (`camera.js`). Drag to move the graph, use Ctrl (or Cmd) and the wheel to zoom around the pointer, or the buttons `+`, `−` and `Fit`; the keys `+`, `-`, `0` and the arrows do the same. The first view shows the whole graph when it fits at half its size or more, else its start at a readable size, around the first box nearest the middle of the graph. A plain wheel over the graph scrolls the page and a hint says how to zoom; after a click into the graph the plain wheel zooms too, until the pointer leaves it, as on embedded maps. Until the user moves the view, new pictures are framed the same way. Switches choose the graph of tasks or of steps, and top down or left to right.

**Folding.** In the graph of tasks, a step with more than five tasks is one box (fewer when the graph would be wider than 16 boxes), stacked, like `FASTQC` with `24 tasks: 20 succeeded, 4 running`. A click opens it: its tasks take its place, the view keeps them where the box was, and the step appears in the panel. A list of the open steps above the graph, and the panel of the step, fold it again. So a run of thousands of tasks reads as a graph of its steps, with the tasks of the steps that the user opens.

**Boxes** (`boxes.js`). A tooltip shows the name, state, step, time, exit code, where a reused result came from, and markers of a task, or the counts of a step, or the reasons of an open point. A click on a task opens its panel and marks the box; a click on an open point stands for the box before it. When a task is chosen elsewhere, the view moves to its box if it is out of sight.

**Live, without jumping.** Each update of a running run changes the class and icon of the boxes of its tasks, and the counts of the boxes of their steps, in place. Only new tasks or markers need a new picture: the page then puts the new picture in the place of the old one, keeps the view on what the user looked at (the box nearest the middle of the view, or the step that opened or folded), and lets each box glide from its old place to its new one, new boxes and lines fading in. Large pictures change without gliding.

**Fast on large runs.** Below a zoom of 0.3 the boxes show their state by colour alone, without names and icons. Pictures of more than 1,500 boxes hide the boxes and lines that lie outside the view while it moves. On a run of 10,000 simulated tasks in two steps (release build, headless Chrome): the folded graph is served in 0.1 seconds; with both steps open, the server draws all 10,000 boxes in 0.13 seconds (4.2 MB of SVG) and the page shows them 1.4 seconds after the click; moving the view then takes 16 milliseconds per frame, zooming out to the whole graph 21, and 500 tasks that change their state are painted in 5 milliseconds.

## 5. The RO-Crate

`iunctus export <run>` writes a folder:

| File | What it holds |
|---|---|
| `ro-crate-metadata.json` | the metadata, as JSON-LD |
| `<workflow file>` | a copy of the workflow that ran; for a project folder its main file (`main.nf`, the Snakefile) |
| `README.md` | what the folder is, and for a simulated run that its files are made up |
| `data/<sha256>` | each file of the run once, named by its digest, as CWLProv and runcrate name them; a directory is `data/<sha256 of its manifest>/` |

Files are copied, not linked, so that nothing done to the crate can change the store. A file that is gone, like an input that a simulated run assumed, stays in the metadata with a note that it was not there; a file on the web stays a URL.

The crate conforms to Process Run Crate, Workflow Run Crate and Provenance Run Crate 0.5 and Workflow RO-Crate 1.0, on RO-Crate 1.1, the newest versions that the validator checks:

| Entity | Type | Holds |
|---|---|---|
| the workflow | `File`, `SoftwareSourceCode`, `ComputationalWorkflow`, `HowTo` | its language, its steps, its tools, the Git revision as `version` |
| each step | `HowToStep` and a `SoftwareApplication` for its tool | the name of the process, rule, step or template; its position in first-use order |
| the engine run | `OrganizeAction` | iunctus as instrument, the step actions, the workflow run as result |
| the workflow run | `CreateAction` | the inputs from outside the run and the parameters as objects, the outputs that no task read as results, times and status |
| each step's run | `ControlAction` | the step as instrument, the actions of its tasks |
| each task that ran | `CreateAction` | the command and script, the tool, inputs, outputs, start, end, status, error, container image, environment |
| each image | `ContainerImage` | registry, name, tag and SHA-256 digest when pinned |
| each file | `File` or `Dataset` | name, the path the task saw, `sha256`, `contentSize` |

A crate of a simulated run says so in the description of the crate, of the workflow run and of every task, in its README, and in a `simulated` property of the crate.

**Validation.** `run-crate/tests/fixtures/validate.sh` writes the fixture crate of a small simulated run again and checks it with `rocrate-validator` 0.12.0 (`uv tool install roc-validator`); the report is `run-crate/tests/fixtures/validation.txt`. At the level REQUIRED all 83 checks pass: the crate is a valid `provenance-run-crate-0.5`. At the level RECOMMENDED 174 checks pass and 6 fail, each for something iunctus cannot know: the person who ran the workflow (`agent`, `author`), a `publisher`, the media type of files (`encodingFormat`), absolute web addresses for tools that only exist inside the workflow, and a full Bioschemas description of the workflow. The crate of a real `iunctus run` of a two-step Nextflow workflow gives the same result. The test `run-crate/tests/export.rs` fails when the export changes without the fixture being written and checked again.

Not yet: the formal parameters of the workflow (they come with the JSON Schema of inputs, decision 24), resource use, and a zip file.

## 6. Insight

**Why a task ran again** (`Database::why`, `iunctus why`, and a section of the task on the page). The task is compared with the same task in the run that this run resumes, or else in the latest earlier run of the same workflow: of the tasks of the same step there, the one with the same fingerprint, else the one with the most inputs from the same places, else the one with the same name. Names alone can mislead, since some hold a number in the order tasks finished. The answer names each difference in plain words: the image, the command, the script, the standard input, the work directory, the outputs, each environment variable, the text of a file that the workflow writes, and each input: new, gone, of another size or digest, or a new output of a task that ran again. When nothing differs, it says why the result was not taken: the task failed before, an input was written again with the same size, the run did not resume the earlier one, or the earlier files were gone. A task that did not run says where its result came from.

**Where a file came from** (`Database::origin`, `iunctus origin`). From a file, by its location or its digest, the answer walks back: the task that made it in the latest run that did, then the tasks that made that task's inputs, down to the data that no task made. Since a resumed run reads the files of the runs before it, the chain crosses runs. Once outputs carry digests (section 2), the same content made by another run is found by its digest too. On the page, each input links to the task that made it.

## 7. Checks

- `run-database/tests/database.rs`: a run is recorded and read back with its logs, events, tasks, files, edges and markers; a resumed run joins the lineage and finds the finished tasks; why a task ran again, and where a file came from; a database of a newer version is refused. Unit tests cover the fingerprint and the end of long logs.
- `run-crate/tests/export.rs` and its fixture; unit tests cover image references and the copies by digest.
- `run-page/tests/page.rs`: the page and its scripts, the JSON, a download, a log and the stream of a finished run; the graph down and to the right, a folded step with the states of the database, an open step, and `fold=0`.
- `iunctus/tests/runs.rs`: `iunctus runs`, `-resume` with nothing changed and with a changed input, `iunctus why` on the tasks that ran again, `iunctus origin`, `iunctus export`, and a real run that may not resume a simulated one. Unit tests cover the new options, the Git revision and the main file of a project.
