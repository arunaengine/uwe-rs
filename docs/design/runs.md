<!-- How iunctus records every run, resumes runs, shows them on a web page and exports them as RO-Crates. -->
<!-- Copyright (c) 2026 The iunctus Contributors -->
<!-- SPDX-License-Identifier: MIT or Apache-2.0 -->

# Runs: the run database, resume, the web page and RO-Crate

Status: implemented, 2026-09-30. This applies [decisions](../decisions.md) 10, 23 and 25, and the reuse scopes of the [Nextflow runtime](nextflow/runtime.md), section 5.3.

Every `iunctus run` and `iunctus graph` records itself in one SQLite file, the run database, in the same form for all four languages and for simulated and real runs. Three features read it:

- **Resume:** `iunctus run <workflow> -resume` continues the last run of the workflow without running its finished tasks again.
- **The web page:** `iunctus run -page` shows the run live in the browser, and `iunctus page` shows any past run.
- **RO-Crate:** `iunctus export <run>` writes a Workflow Run RO-Crate of the Provenance Run Crate profile, with the digest of every file.
- **Insight:** `iunctus why <run> <task>` tells why a task ran again, `iunctus origin <file>` where a file came from, `iunctus time <run>` where the time of a run went, `iunctus compare <run> <run>` what differs between two runs, `iunctus replay <run> <task>` runs one recorded task again alone, and `iunctus telemetry <run>` writes and sends its OpenTelemetry trace.

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
| `iunctus time <run>` | where the time of a run went: the critical path, waiting and running, and the steps that used the most (section 6.1) |
| `iunctus compare <run> [<run>]` | what differs between two runs, task by task; with one run, the run of its workflow before it (section 6.2) |
| `iunctus replay <run> <task>` | runs one recorded task again alone, on this machine or with `-executor tes\|slurm\|kubernetes`, and tells what differs (section 6.3) |
| `iunctus telemetry <run>` | writes the OpenTelemetry trace of a run to `-output`, by default `<run>-traces.jsonl`, and sends it to the endpoint of the config file (section 6.4) |
| `iunctus report <run>` | writes the report of a run, one HTML file without a server, to `-output`, by default `<run>-report.html` (section 4.2) |
| `-report <file>` | `iunctus run` writes the report of the run there when it ends; `file` of the `[report]` section of the config file does the same for every run |
| `-format text\|json` | what `time` and `compare` write: plain text (the default) or JSON |
| `-resume [<run>]` | continues a run; without an id, the last run of the same workflow |
| `-page` | serves the page of the run that `iunctus run` starts; it stays open after the run until Ctrl-C |
| `-address <ip>`, `-port <number>` | where the page listens: `127.0.0.1` and a free port by default |
| `-database <file>` | another run database than the default |
| `-output <folder>`, `-license <url>` | the folder and the license of an exported crate; for `telemetry`, the file of the trace |

A run id is the start time in milliseconds and the process id, both in hexadecimal, like `1a0f1cc4c10-3a2487`. `-resume` takes the next argument as the run only when it has this form, so `iunctus run -resume main.nf` resumes the last run of `main.nf`.

**What a real run shows.** `iunctus run` with a real executor shows its run as `nextflow run` does, the same for every language. Its first line names the version, the workflow, the language, the executor and the run id. In a terminal, a block below it is drawn again in place at each event and every second: the executor with its tasks, those running and the time so far, then one line per step with the task that last started or ended, how many of the step's tasks are done, how many came from the cache and how many failed, and at the end a check mark, or a cross for a step with failed tasks:

```text
iunctus 0.1.0: running main.nf (Nextflow) on slurm, run 1a0f6c9c956-144e02
executor >  slurm (4 tasks, 0 running, 41s)
[1] SAY (1) | 3 of 3 ✔
[4] JOIN    | 1 of 1 ✔

Done in 41s: 4 tasks, 4 ran, 0 from the cache, 0 failed
Results:
  results/ a.txt, all.txt, b.txt, c.txt
Files of every task: /vol/project/cache/store
Report: report.html
More: `iunctus runs`, `iunctus page 1a0f6c9c956-144e02`, `iunctus time 1a0f6c9c956-144e02`
```

What else the run prints, like Nextflow's `view`, a CWL output object or a warning, goes above the block: while it is drawn, standard output, and standard error when it is the same terminal, go through pipes that `iunctus/src/terminal.rs` reads. Elsewhere, like in a log file, each task prints a line when it ends (`[done] SAY (1)`, `[cached] ...`), and the summary comes at the end. The summary names the results, the files that the run delivered (Nextflow's `publishDir`, Snakemake's targets, CWL's output folder, Argo's artifacts), by folder, relative to the folder of the run; then the store, which keeps the files of every task, and the commands that show more. A failed run says `Failed after`, and its error follows on standard error as before. Simulated runs print as before. `NO_COLOR` turns the colors off.

## 2. The run database

The file is `runs.sqlite` in the cache directory (`$XDG_CACHE_HOME/iunctus`, else `~/.cache/iunctus`), next to the default store. One database serves every workflow and store of the user, so `iunctus runs` lists them all; each run records its store.

**SQLite** through `rusqlite` with the bundled library, so nothing needs to be installed. The database runs in WAL mode: readers, like the page, never wait for a run that writes, and several iunctus processes can share the file. `synchronous = NORMAL` syncs at checkpoints only; a crash can lose the last commits, never break the file. All SQL is plain, so another engine can replace SQLite behind the same interface (decisions, open questions).

**How it is filled.** The engine reports each step of a run to an observer (`workflow_graph::Observer`, owned by the engine): a task was asked for, with its edges; an attempt starts; an attempt ends, with its outcome; the run ends. The `Recorder` of `run-database` is that observer. It writes each event before its effect, as the design of runtime.md section 4.2 asks. Writes gather in one transaction that is committed every 250 milliseconds, by the next write or by a small thread of the recorder, so a run of many short tasks commits rarely and a long task never holds the write lock. A failed write does not stop the run: the recorder keeps the first error and the command prints it at the end.

**Schema, version 1.** Times are milliseconds since the Unix epoch. The full definition with a comment on each column is in `run-database/src/schema.rs`.

| Table | One row per | Columns |
|---|---|---|
| `runs` | run | id, lineage, the run it resumes, workflow (absolute path), language, Git revision, command line and parameters (JSON), executor, simulated, store, process id, start, end, state (`running`, `succeeded`, `failed`), error |
| `tasks` | task the driver asked for | run, id, step, name, the task as JSON, fingerprint, time asked, state (`waiting`, `running`, `succeeded`, `failed`, `stopped`) |
| `attempts` | execution of a task | run, task, number, start, end, exit code, reused (`cache` or the id of an earlier run), image as it ran, retried, and what it used when the executor measured it (CPU time, peak memory, block I/O) |
| `files` | input of a task, or output of an attempt | run, task, attempt (none for inputs), role, the path the task sees, location, directory, SHA-256 digest, size |
| `edges` | edge of the execution graph | run, source task, target task, kind (`file` or `order`), path |
| `markers` | point where real data can change the tasks (decision 21) | run, task, kind (`content`, `wildcard`, `checkpoint`), reason |
| `logs` | standard output or error of an attempt | run, task, attempt, stream, SHA-256 of the whole text, its size, the text or its last 64 KiB |
| `events` | step of the run, in the order the engine saw it | run, sequence, time, kind (`requested`, `started`, `finished`, `reused`, `marked`, `ended`), task, attempt |

The schema version is SQLite's `PRAGMA user_version`. Opening a file brings it to the current version with the missing steps of `schema::MIGRATIONS`, in one transaction; a file of a newer version is refused with a clear error. A change of the tables adds a step and raises `VERSION`.

A run whose process has gone while its state is still `running` reads as `stopped`, so the list shows runs that were killed.

**Logs.** The database keeps the whole text of a standard output or error up to 64 KiB, and the end of a longer one. A real run keeps the whole text in its store under the digest, and the page serves it from there.

**Digests.** A literal input gets its digest when the task is asked for. Outputs get digests from the executor once `Outcome` carries them (the execution layer, step 2); until then `iunctus export` computes the digest of each file it copies.

Version 2 adds the inputs of each run to `runs` (decision 24): `input_schema`, the JSON Schema of the inputs of the workflow, and `inputs`, the values that the run checked against it ([the inputs of a workflow](inputs.md)).

Version 5 adds replays to `runs`: `replays` and `replays_task` name the run and the task that a replay of one task runs again (section 6.3). Version 4 holds what a run needs to be repeated exactly (`program`, `launch`, `workflow_folder`, `repeats`, `output_schema`, and the tables `sources`, `remote_files` and `checks`); [reproducibility](reproducibility.md), section 1, describes them.

Version 3 adds what each attempt used to `attempts`: `cpu_usec`, the CPU time in microseconds, `memory_peak`, the peak of memory in bytes (page cache included), and `read_bytes` and `write_bytes` of block I/O. The wrapper of a task reads them from the cgroup of its container at its end (`control/usage`), so they are known with Docker and Podman, and empty where a container has no cgroup of its own.

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

**What it shows.** The list of runs. For one run: its workflow, state, times, executor, revision, parameters and command line; a notice when the run was simulated; the number of tasks in each state as a bar; the graph view, next to a panel of the chosen task or step; a table of steps with the tasks in each state and their time; a table of tasks, with a search and a filter by state and step. The panel of a task shows its command, script, image, markers, standard output and error, inputs (with a link to the task that made each one), outputs with links to download them, environment and attempts; the panel of a step shows its tasks by state, its markers, and buttons that open or fold it in the graph and show its tasks in the table. A card tells where the time of the run went (section 6.1), and the summary offers to compare the run with another run of its workflow (section 6.2).

**Live.** A running run keeps an EventSource open (Server-Sent Events), and the time of the run in its summary ticks every second between the updates. The server reads new events from the database twice a second and sends one update with the tasks that changed, so a run of thousands of short tasks sends few messages. The page changes the states of the boxes of the graph in place; when tasks were asked for or a marker came (the `graph` flag of the update), it lays the graph out again, at most every two seconds. A browser that reconnects says which event it saw last, and the stream ends after the update that shows the run has ended.

**Routes.** `/`, `/runs/<run>`, `/runs/<run>/tasks/<task>` and `/compare/<run>/<run>` serve the page; `/api/runs`, `/api/runs/<run>`, `/api/runs/<run>/tasks/<task>`, `/api/runs/<run>/events`, `/api/runs/<run>/graph.svg`, `/api/runs/<run>/time`, `/api/compare/<run>/<run>`, `/api/runs/<run>/tasks/<task>/why`, `/api/runs/<run>/tasks/<task>/logs/<attempt>/<stream>` and `/api/runs/<run>/tasks/<task>/{inputs,outputs}/<n>` serve JSON, logs and files. `graph.svg` takes `view=tasks|steps`, `direction=down|right`, `open=<steps, one name per line>` and `fold=<n>`.

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

### 4.2 The report

Some machines have no browser and no way to reach the page, like the login nodes of a cluster. The report of a run is the page of that one run in one HTML file, which the user copies to a computer and opens from the disk. `run_page::report` builds it: the style and the scripts of the page inline, and in a JSON element the answers that the page would get from the server for the run, its time, each task (with at most the last 4 KB of each log, `LOG_TAIL`) and why it ran, and the four pictures of the graph (tasks or steps, down or right; a run of up to 200 tasks has every task in the picture). `report.js` answers the requests of the page's scripts from that data instead of the network; the views go by the part of the address after `#`. A view that the file does not hold, like another run, says so. The report adds three parts that the page does not have: a timeline of the tasks (waiting, then running, as Nextflow's timeline report), the CPU time, CPU share, memory peak and I/O of each task, and where the results went, from the table `deliveries` (schema version 6, which records the engine's `Delivered` events). The file refers to nothing outside itself; the reports of the demo are 135 to 255 KB.

## 5. The RO-Crate

`iunctus export <run>` writes a folder:

| File | What it holds |
|---|---|
| `ro-crate-metadata.json` | the metadata, as JSON-LD |
| `<workflow file>` | a copy of the workflow that ran; for a project folder its main file (`main.nf`, the Snakefile); from the store of a real run, as it ran |
| `workflow/` | the files of the workflow as the run had them, when the run recorded them ([reproducibility](reproducibility.md), section 1) |
| `outputs.schema.json` | the JSON Schema of the outputs of the workflow, when the run has one; each output is a `FormalParameter`, the `output` of the workflow |
| `README.md` | what the folder is, and for a simulated run that its files are made up |
| `data/<sha256>` | each file of the run once, named by its digest, as CWLProv and runcrate name them; a directory is `data/<sha256 of its manifest>/` |
| `inputs.schema.json` | the JSON Schema of the inputs of the workflow, when the run has one; each input is a `FormalParameter` of the workflow ([the inputs of a workflow](inputs.md), section 7) |

Files are copied, not linked, so that nothing done to the crate can change the store. A file that is gone, like an input that a simulated run assumed, stays in the metadata with a note that it was not there; a file on the web stays a URL, with its digest, size and the time the run read it (`sdDatePublished`). The files that the command line named, like a config file, are in `data/` too, as inputs of the workflow run.

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

**Validation.** `run-crate/tests/fixtures/validate.sh` writes the fixture crate of a small simulated run again and checks it with `rocrate-validator` 0.12.0 (`uv tool install roc-validator`); the report is `run-crate/tests/fixtures/validation.txt`. At the level REQUIRED all 83 checks pass: the crate is a valid `provenance-run-crate-0.5`. At the level RECOMMENDED 172 checks pass and 8 fail. Six are for something iunctus cannot know: the person who ran the workflow (`agent`, `author`), a `publisher`, the media type of files (`encodingFormat`), absolute web addresses for tools that only exist inside the workflow, and a full Bioschemas description of the workflow. Two came with the files that pin the run: the validator could not fetch the made-up web address of the fixture without network, and no file of the run names the formal parameters of outputs as its `exampleOfWork` yet. The crate of a real `iunctus run` of a two-step Nextflow workflow gives the same result. The test `run-crate/tests/export.rs` fails when the export changes without the fixture being written and checked again.

Not yet: the formal parameters of the workflow (they come with the JSON Schema of inputs, decision 24), resource use, and a zip file.

## 6. Insight

**Why a task ran again** (`Database::why`, `iunctus why`, and a section of the task on the page). The task is compared with the same task in the run that this run resumes, or else in the latest earlier run of the same workflow: of the tasks of the same step there, the one with the same fingerprint, else the one with the most inputs from the same places, else the one with the same name. Names alone can mislead, since some hold a number in the order tasks finished. The answer names each difference in plain words: the image, the command, the script, the standard input, the work directory, the outputs, each environment variable, the text of a file that the workflow writes, and each input: new, gone, of another size or digest, or a new output of a task that ran again. When nothing differs, it says why the result was not taken: the task failed before, an input was written again with the same size, the run did not resume the earlier one, or the earlier files were gone. A task that did not run says where its result came from.

**Where a file came from** (`Database::origin`, `iunctus origin`). From a file, by its location or its digest, the answer walks back: the task that made it in the latest run that did, then the tasks that made that task's inputs, down to the data that no task made. Since a resumed run reads the files of the runs before it, the chain crosses runs. Once outputs carry digests (section 2), the same content made by another run is found by its digest too. On the page, each input links to the task that made it.

### 6.1 Where the time went

`Database::time_spent` (`run-database/src/time.rs`), `iunctus time <run>`, and the card "Where the time went" on the page. It reads the times of the tasks, attempts, edges and events of a run.

**The critical path** is the chain of tasks that set the length of the run. It starts at the task that ended last and walks back: each task waited for the task before it, which is the task that ended last of those whose files it reads or that it comes after (its edges); or, when the workflow asked for it only after all of those had ended, the task whose end made the workflow ask for it. The event log names that task: the engine hears how an attempt ended, then asks the driver, and records the tasks that the driver asks for then, so the last `finished` event before a `requested` event made that request. This follows the graph as it grew, also where a task depends on a value rather than a file, like a Nextflow `collect` or a Snakemake checkpoint.

Each task of the path splits its part of the run into three: the time between the end of the task before it and the moment the workflow asked for it (iunctus stored the outputs, and the workflow planned), the time it waited from the moment it could start until it started (a free CPU or memory, staging, the start of the container), and the time it ran, from the start of its first attempt to the end of its last. With the time before the first task (loading the workflow, checking its inputs) and after the last one (delivering results, reports), the parts add up to the length of the run.

**Waiting and running.** For every task, the time it waited is from the moment it could start (the workflow had asked for it, and every task before it had ended) until it started; the time it ran is that of its attempts. The totals, the number of tasks that ran at a time on average, and the time in which no task ran at all (the union of the attempts) show whether a run was held back by its tools, by waiting for the machine, or by planning. In real runs, the start and end of an attempt are those of the tool itself: the wrapper writes them in `control/start` and `control/end` (with microseconds from Bash 5 on), so waiting for a share of the machine and starting the container count as waiting. A task that came from the cache or from an earlier run neither waited nor ran.

**Steps.** For each step: its tasks, how long they ran and waited together, and what they used as the cgroup of their containers counted it (section 2, version 3): the CPU time and block I/O of all attempts together and the highest peak of memory of one attempt. `iunctus time` lists the five steps that ran the longest, used the most CPU time, the most memory and the most I/O; the page shows a table of all steps in the chosen order, with a bar for the value it is sorted by.

**On the page**, the card shows the numbers of the run, the critical path as one bar (its parts in three colours, the names of the tasks inside the parts that are wide enough, a tooltip on every part, and a click opens the task), the path as a table, and the table of steps. The switch "Critical path" in the graph view (also in the card) marks the boxes and lines of the path, or the boxes of their steps where those are folded, and lets the rest of the graph step back. The lines of the picture name the boxes they join (`data-from` and `data-to`) for this. While a run goes on, the card is read again at most every five seconds.

### 6.2 Comparing two runs

`Database::compare` (`run-database/src/compare.rs`), `iunctus compare <run> [<run>]`, and the page `/compare/<run>/<run>`. The two runs may be runs of the same workflow or of two versions of it, like two revisions or two copies.

**The runs** differ in their workflow, language, executor, Git revision, whether they were simulated, their parameters (by name) and their input values (decision 24), each said in one sentence.

**Tasks are paired** step by step: first the tasks with the same fingerprint, then those with the same name, then those that read and make the most files of the same names (paths as the task sees them, like `sample1.bam`), and the rest in order. Names alone can mislead, as in resume, since Nextflow numbers tasks in the order they finished. A pair is:

| Pairing | Meaning |
|---|---|
| `same` | the task does the same and made the same outputs |
| `changed` | the task does something else: the image, the command, the script, the environment, what the workflow writes into a file, or an input by its content, size or place; also the image as it ran, pinned to its digest, when a tag moved |
| `outputs` | the task does the same, but outputs have other content, by their digest: a tool that does not give the same bytes |
| `first`, `second` | only one run has the task |

For each pair it also says how the task ended otherwise (succeeded in one run, failed with exit code 1 in the other) and where its result came from: `it ran in the first run and came from the cache in the second`. Inputs are compared by content where both digests are known: the digest of the input itself, or of the output of any run at its place; else by size and place. An input that a task made in each run lies at another place in each (the store or scratch folder of its run), so its place tells nothing; the tasks that made it are compared themselves.

**On the page**, the summary of a run has a list of the other runs of its workflow, the run before it chosen, and a button that opens the comparison: the two runs, what differs between them, the counts of each pairing, and a table for each kind of pair with links to the task in each run.

### 6.3 Replaying one task

`iunctus replay <run> <task>` (`iunctus/src/insight/replay.rs`) runs one recorded task again, alone, from its description in the run database, and the task panel of the page has a button that shows this command.

**The same task.** The task runs as the driver described it, without the tasks it waited for, in the image it ran in, pinned to its digest, which the attempt recorded. Each input is taken where it is: at its place when the file is still there, or else from the store by its digest, which the run database knows for every output and for the inputs that the runner hashed. A directory whose files are gone, or an input whose digest is unknown, stops the replay with a sentence that names it. A task of a simulated run cannot be replayed, since its files are made up.

**Anywhere.** It runs on this machine by default (`-executor local`), or on a TES server, a SLURM cluster or Kubernetes with the options of `iunctus run`; the store is the one of the run unless `-store` names another. It runs whatever the cache holds (`RunnerExecutor.reuse`, which sets the reuse scope `none`), and stores no new cache entry, so a replay never changes what later runs take from the cache.

**A run of its own.** The replay is recorded as a run with one task, the same task number, and `runs.replays` and `runs.replays_task` naming the task it replays. So its logs, outputs, times and what it used are on the page like those of any run. Resume, `why` and the list of earlier runs to compare with leave replays out; `iunctus compare <replay>` compares it with the run it replays, and `iunctus runs` names it.

**What differs.** After the run, the command compares the replay with the task as it ran (section 6.2): the exit code and time before and now, where the earlier result came from when the task had not run, any difference in what the task does, each output whose content differs by its digest (or that every output has the same content, which checks that the tool gives the same bytes), and the last lines of the standard output and error before and now. A failed task replays too: its old and new errors stand next to each other, and the command ends with exit code 1 when the task fails again. On the page, the panel of a task lists its replays, each with a link to compare it.

### 6.4 OpenTelemetry traces

The crate `run-telemetry` turns a run of the database into an OpenTelemetry trace in the OTLP JSON format, with no OpenTelemetry library: the format is plain JSON (ids as hex, 64-bit numbers and times as decimal text, enums as numbers).

| Span | Parent | From, until | Attributes | Events |
|---|---|---|---|---|
| the run, `run <workflow file>` | none | start, end of the run | `iunctus.run.id`, `.lineage`, `.state`, `.error`, `.resumes`, `.replays`, `.tasks`, `iunctus.workflow`, `.language`, `.executor`, `.simulated`, `.revision`, `.store`, `iunctus.parameter.<name>` | `ended` |
| each task, named by its step | the run | when the workflow asked for it, until its last attempt ended | `iunctus.task.id`, `.name`, `.step`, `.state`, `.exit_code`, `.attempts`, `.reused` | `requested`, `marked` with `iunctus.marker.kind` and `.reason` |
| each attempt, `attempt <n>` | its task | its start and end | `iunctus.attempt.number`, `.exit_code`, `.image` (pinned), `.reused`, `.retried`, `.cpu_usec`, `.memory_peak`, `.read_bytes`, `.write_bytes` | `started`, `finished`, `reused` |

The resource is `service.name = iunctus` with its version. A failed run, task or attempt has the status error with a message, a succeeded one ok. Tasks are named by their step, as OpenTelemetry advises names of few values, so that dashboards group the tasks of a step; the name of each task is an attribute. The trace id and the span ids come from the SHA-256 of the run id (and task and attempt), so the same run always gives the same trace, and sending it twice does not make two.

**To a file.** `iunctus telemetry <run>` writes the trace as JSON lines, one export request of at most 1,000 spans per line, as the file exporter of OpenTelemetry writes them and the `otlpjsonfile` receiver of the OpenTelemetry Collector reads them.

**To an endpoint.** With a `[telemetry]` section in the config file, the command also sends the requests, one after the other, as `POST <endpoint>/v1/traces` with JSON (an endpoint that ends with `/v1/traces` is taken as it is), through the one HTTP client of the process, with the headers of the section, whose values are secrets (decision 20) and marked as sensitive. Every real run and replay sends its trace there when it ends; simulated runs do not, since their times are made up. A failure to send is a warning after the run; a partial success says how many spans the endpoint rejected.

```toml
[telemetry]
endpoint = "http://localhost:4318"
headers = { authorization = { environment = "OTLP_AUTHORIZATION" } }
```

**Checked** against the trace messages of OTLP v1 (the same message and field names and numbers, built with `protoc` and read with protobuf's JSON parser, ids turned from hex to base64 first): the traces of a real run and of a failed run of 2026-09-30 parse without unknown fields, every trace id has 16 bytes, every span id 8, and no span ends before it starts. A collector or a tracing backend was not at hand (no downloads during the network stop).

## 7. Checks

- `run-database/tests/database.rs`: a run is recorded and read back with its logs, events, tasks, files, edges and markers; a resumed run joins the lineage and finds the finished tasks; why a task ran again, and where a file came from; a database of a newer version is refused. Unit tests cover the fingerprint and the end of long logs.
- `run-crate/tests/export.rs` and its fixture; unit tests cover image references and the copies by digest.
- `run-database/tests/insight.rs`: where the time of a recorded run went, a task that the workflow asks for again, two runs compared task by task, and a replay that resume leaves out and whose image, named by its digest, is the same as the tag it ran in before; unit tests in `time.rs` cover the critical path, waits, the steps and overlapping attempts.
- `run-page/tests/insight.rs`: the time of a run, the lines that name their boxes, the comparison of two runs, and the replays of a task.
- `iunctus/tests/insight.rs`: `iunctus time` and `iunctus compare` on simulated runs, a replay refused for a simulated run, `iunctus telemetry` to a file and to a small OTLP HTTP receiver of the test with a header from an environment variable, and (ignored, needs Docker) a real run that sends its trace, and a task replayed in its container after its input file was removed from its place in the store.
- `run-telemetry/tests/traces.rs`: the spans, parents, attributes, markers and events of a recorded run, requests of a given size, JSON lines, and sending to a receiver that answers with a partial success or refuses; unit tests cover the JSON form of values, ids, times and addresses.
- `run-page/tests/page.rs`: the page and its scripts, the JSON, a download, a log and the stream of a finished run; the graph down and to the right, a folded step with the states of the database, an open step, and `fold=0`.
- `iunctus/tests/runs.rs`: `iunctus runs`, `-resume` with nothing changed and with a changed input, `iunctus why` on the tasks that ran again, `iunctus origin`, `iunctus export`, and a real run that may not resume a simulated one. Unit tests cover the new options, the Git revision and the main file of a project.

## File descriptions: run-telemetry

### `run-telemetry/src/lib.rs`

OpenTelemetry traces of runs: a run of the run database as a trace, with the run as the
root span, each task as a span below it and each attempt below its task, with their
attributes, markers and events, in the OTLP JSON format. The traces go to a file of JSON
lines, one export request per line, as the file exporter of OpenTelemetry writes them and
the `otlpjsonfile` receiver of the OpenTelemetry Collector reads them, and to an OTLP HTTP
endpoint that the config file names. No OpenTelemetry library is needed: the format is
plain JSON.

### `run-telemetry/src/otlp.rs`

The shapes of the OTLP JSON format of traces: an export request holds the spans of one
resource, iunctus, in one scope. Following the JSON form of the OTLP protocol, ids are hex
text, 64-bit numbers and times are decimal text, and enums are numbers.

### `run-telemetry/src/send.rs`

Sending traces to an OTLP HTTP endpoint, like an OpenTelemetry Collector or a tracing
backend that takes OTLP: each export request as `POST <endpoint>/v1/traces` with JSON, one
after the other, with the headers of the config file, like a token.

### `run-telemetry/src/spans.rs`

The spans of a run from the run database: the run as the root span, each task as a span
from the moment the workflow asked for it until its last attempt ended, and each attempt as
a span below its task. Their attributes come from the database; the events of the engine
(`requested`, `started`, `finished`, `reused`, `marked`, `ended`) become events of the span
they concern, and each marker an event with its kind and reason. Ids are made from the run
id, so that the same run always gives the same trace.
