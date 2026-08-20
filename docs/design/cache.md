<!-- How iunctus reuses the results of tasks across runs, pipelines and languages, runs the same task once, cleans up its store by rule, previews a run, and starts the longest chain first. -->
<!-- Copyright (c) 2026 The iunctus Contributors -->
<!-- SPDX-License-Identifier: MIT or Apache-2.0 -->

# The cache: reuse, one execution, clean-up, the preview and the longest chain first

Status: implemented, 2026-09-30. This applies [decisions](../decisions.md) 7, 9 and 10, and the reuse scopes of the [Nextflow runtime](nextflow/runtime.md), section 5.3.

iunctus keeps the result of every task in a content-addressed store, and the times of every run in the run database. Five features build on them, so that iunctus spends less compute, less storage and less waiting than the native engines:

- **The global cache.** Any run, pipeline or language that asks for a task that ran before takes its result from the store. Nothing needs to be turned on.
- **One execution.** Two runs, or two tasks of one run, that ask for the same task at the same moment run it once. The other waits and takes the result.
- **Clean-up by rule.** `iunctus clean` removes what no rule keeps, like "keep what was used in the last 30 days". It replaces `nextflow clean`, and it never removes what a run still refers to.
- **The preview.** A simulated run tells, for each task, whether a real run would take it from the cache. So the user sees how much work is left before any compute is spent.
- **The longest chain first.** When tasks wait for the machine, the one with the most work after it starts first, by the times of earlier runs. So the run ends sooner on the same machine.
- **Digests of local files** that did not change are not computed again, and `-no-cache` runs every task of one run (section 7).
- **A store in layers:** a folder of this machine in front of a shared store on S3; reads look in the folder first, and new results go to both (section 8).

| Crate | What it does here |
|---|---|
| `task-model` | The task description and its cache key |
| `content-store` | The store: contents, directory manifests, outputs, cache entries, marks of use, leases, and listing and removing objects |
| `task-runner` | Answers tasks from the store, waits for a task that runs elsewhere, looks up results without side effects (`Lookup`), and plans clean-up (`clean`) |
| `run-database` | What runs refer to, which runs go on, the image digests of real runs, and the chains of steps with their times |
| `executor` | The pool of CPUs and memory of the local executor, which serves waiting tasks by priority |
| `iunctus` | `iunctus clean`, and the preview in simulated runs (`preview.rs`) |

## 1. The same task

A task is the same when it computes the same thing: the same image by digest, the same command, the same work directory and environment, the same inputs by content, the same output patterns and the same standard input. This is the task description (`TaskSpec`, format 2). Its cache key is the SHA-256 of its canonical JSON (RFC 8785). What does not change the result is left out: the name of the task, its step, its resources, the place of its inputs, and the run.

So a result is shared:

- by later runs of the same workflow, without `-resume`;
- by other pipelines that call the same module on the same data, whatever the process is called and wherever it sits in the workflow;
- by other languages, where their task descriptions are equal.

A result is shared only when the image is pinned to a digest (scope `shared`). A real run pins each image before its first task. An image without a registry digest, like one built on this machine, gives the scope `lineage`: only `-resume` reuses its results, through the run database ([runs](runs.md), section 3).

**What makes two descriptions differ in practice:**

- nf-core modules of the older style write `${task.process}` into `versions.yml` in their script. The script then holds the full name of the process, like `NFCORE_RNASEQ:RNASEQ:FASTQC`, so the same module in two pipelines is two different tasks: their `versions.yml` really differ. Modules of the newer style give their versions through `eval` outputs and topic channels, outside the script, and are shared.
- Each language describes its tasks in its own way. A CWL tool runs in `/var/spool/cwl` with `HOME` and `TMPDIR` set, and its inputs are staged under `/var/lib/cwl`, as the CWL standard asks. A Snakemake job runs `bash -c 'set -euo pipefail; ...'` in a directory that the executor chooses, with the thread variables of Snakemake. So a CWL tool and a Snakemake rule that do the same work are different tasks, and both run. A CWL tool and an Argo template written alike, with the same working directory, environment, command and input, are the same task, and the second takes the result of the first (the test in section 7).

## 2. The store

```text
<store>/
  cas/sha256/<2 hex>/<62 hex>          file contents by digest
  trees/sha256/<2 hex>/<62 hex>        directory manifests by digest
  runs/<run>/<task>/<attempt>/         the outputs of an attempt under their names
  ac/v1/<namespace>/<key>.json         the cache entry of a key
  ac/v1/<namespace>/<key>.used         when a run last took the entry
  locks/tasks/<namespace>/<key>        the lease of a task that runs now
  locks/runs/<run>                     the lease of a run that goes on
```

**A cache entry** names the run and the task that made the result, each output with its path, digest, size and location, the digests of the captured standard output and error, and how long the task ran (`milliseconds`). Writes are "put if absent": the first result of a key wins.

**A hit.** Before an entry answers a request, the runner checks that the store still has every file it names: each output, the manifest of each directory, and the captured streams. An entry whose files are gone is removed, and the task runs again and stores a new entry. A hit writes the mark of use, `<key>.used`, whose time clean-up reads. So a result that runs keep taking stays, however old it is.

## 3. One execution of the same task

A request for a task with the scope `shared` goes like this:

1. The runner looks for a complete entry of its key. A hit answers the request.
2. Otherwise the runner takes the lease of the key, `locks/tasks/<namespace>/<key>`.
3. When another run, or another task of the same run, holds the lease, the request waits. After a pause it looks for the entry again. The pauses start at 100 milliseconds and double up to 2 seconds. When the entry comes, it answers the request, which prints `[cached]`. When the lease comes free without an entry, because the other task failed or stopped, this run takes the lease and runs the task itself.
4. With the lease, the runner looks for the entry once more, since the other run may have stored it just before it gave the lease back. Then it runs the task, stores the entry, and gives the lease back.

A run that is stopped with Ctrl-C stops waiting at once.

**Leases never outlive their process.** A lease must not block others when the run that holds it crashes.

- In a store on this machine, a lease is a lock on a file under `locks/` (`flock`, through `std::fs::File::try_lock`). The kernel frees the lock when the process ends, also when it is killed. The next request takes the lease at once. Clean-up may remove a lock file that nobody holds; a run that locked the old file notices that it is no longer at its path and locks the new one.
- In an object store, like S3, a lease is an object with an expiry, one minute after it was written. The holder writes it again every 20 seconds. A run that crashed writes nothing, so its lease ends within a minute, and another run takes it over. Where the store has conditional writes (`If-Match`, as S3 has), only one of two runs that take over at the same moment wins. Without them, both may run the task; that costs time, but it is safe, as the first result stored wins. A holder removes only its own lease, never one that another run took over.

**The lease of a run.** Each real run holds `locks/runs/<run>` from its first task on. It tells clean-up, also on another machine, that the run goes on.

## 4. Clean-up by rule

```sh
iunctus clean -keep-days 30                    # list what would go
iunctus clean -keep-days 30 -remove            # list it, then remove it
iunctus clean -keep-run 1a0f1cc4c10-3a2487 -keep-slower-than 2h -remove
```

| Option | What it keeps |
|---|---|
| `-keep-days <n>` | what runs made or used in the last n days: the files of the runs that started then, and every result that was made or taken from the cache then, also by runs of other machines |
| `-keep-run <run>` | what a run needs: the files of its tasks, the results it made, and the results of earlier runs that it took from the cache. Give it once per run |
| `-keep-slower-than <time>` | the results of tasks that ran this long or longer, like `90s`, `10m`, `2h` or `1d`: they are the most expensive to make again |
| `-remove` | removes what the command lists. Without it, the command only lists what would go |

`iunctus clean` needs at least one rule. `-keep-days 0` keeps only what the runs that go on need. `-store`, `-database` and `-iunctus-config` choose the store and the run database, as for `iunctus run`.

**What always stays**, whatever the rules say:

- everything that a run that goes on refers to: a run of the run database whose process still runs, the runs that it resumes, and any run that holds its lease in the store;
- everything made or used after the start of the oldest of those runs, since such a run may take it from the cache at any moment;
- every file that a kept result or a kept run refers to. A result of an old run that a kept run took from the cache stays with it.

**What goes:** the entries that no rule keeps, then the files under `runs/` that nothing kept refers to, the contents in `cas/` and the manifests in `trees/` that no kept result or run needs, the leases that nobody holds, and the scratch folders that runs left behind: the made-up files of simulated runs that no rule keeps, and the folders of attempts of runs that were stopped, in the scratch folder of every executor (`scratch/tes`, `scratch/kubernetes` and `scratch/slurm` too; the SIF files of SLURM jobs in `scratch/slurm/images` stay). Entries go first, so that no request ever gets a result whose files are going.

The listing counts each file once. A task output and its content in `cas/` are two names of one file in a store on this machine; its space counts only when both names go.

```text
The store /home/me/.cache/iunctus/store holds 2 results and 7 files (144 bytes).
It keeps nothing: no run goes on.
It would remove 2 results and 7 files (144 bytes):
  run 1a0f31581de-16ca90 (2026-09-30 18:11:08, /home/me/pipeline/main.nf): 2 results, 2 files
  contents and directory manifests that nothing needs: 5 files
  scratch folders of runs that ended: 2 folders (2.0 kB)
Nothing was removed; add -remove to remove it.
```

**Limits.** Clean-up knows the runs of its own run database. On a store that several machines share, it sees the leases of the runs of the others, and it keeps what they made or used lately, but not which old files an ended run of another machine refers to. The rows of removed runs stay in the run database; their pages then show files that are gone. Scratch folders of runs that the database does not know stay, as they may belong to another database.

## 5. The preview

Every simulated run, `iunctus run` with the simulator and `iunctus graph`, is also a preview. For each task it asks the store whether a real run would take the task from the cache. Such a task gets the stored result, with its real files, and prints `[cached] <task>`; every other task is simulated. Since a task from the cache hands its real files on, the tasks after it can be found in the cache too.

To find the key of a task without running anything, the preview needs the digest of its image. It asks no registry: it takes the digest that the latest real run pinned the image to, from the run database. It hashes local inputs as a real run does, but stores nothing and downloads nothing (`task_runner::Lookup`). A task is simulated when:

- no real run pinned its image, or it names no image and `-default-image` names none that a real run pinned;
- an input is missing, or is a remote file that no run downloaded yet;
- an input is a file that the simulator made up: a real task would make another one.

After a task that runs, the tasks that read its outputs are counted as running too. If the task makes the same bytes again, a real run takes them from the cache after all, so the preview gives the most work that is left.

The preview shows in every output of a simulated run:

```text
[cached] PREPARE:WORDCOUNT (1)
[simulated] REPORT (1)
iunctus: This is a simulation: no task ran, and the outputs are made-up files. A run with real data refines this graph.
iunctus: A real run would take 1 of 2 tasks from the cache; 1 would run.
```

- `iunctus graph` (the summary) gives the same sentence, marks each such task with `(from the cache)`, and the graph of steps counts `1 from the cache` per step.
- The SVG draws such tasks in the state `reused` and puts the sentence under its title.
- The run database records them as reused from the cache, so the web page draws them so too, names them `from the cache` in the legend of a simulated run, and gives the sentence under it.

## 6. The longest chain first

A run ends when its longest chain of tasks ends. When more tasks are ready than the machine can run, the order in which they start decides how long the run takes: a task that starts late on the longest chain makes the whole run late, while a short task at the end of a side branch costs nothing.

So each task gets a priority (`Placement::priority`): the time, in milliseconds, of the longest chain of steps that starts with its step, from the latest five real runs of the same workflow (`Database::chains`). The time of a step is the mean time of its tasks that ran; tasks taken from the cache or from an earlier run took no time. The time of a chain adds the times of its steps, along the edges between the steps of those runs. A loop between steps stops the chain.

The pool of the local executor (`executor::Pool`) serves the tasks that wait by priority, the highest first, and of equal ones the one that asked first. The choice is made when a share of CPUs and memory comes free, so a task with a higher priority goes before the tasks that waited longer. Only the first task of the queue may take its share, so a task that needs much is never passed forever by tasks that need little. A run without earlier real runs gives every task the priority 0, and tasks start in the order they asked, as before.

The priority helps only when tasks wait: on a machine with room for every task, all start at once. The TES, SLURM and Kubernetes executors leave the order to their servers; they could pass the priority on, for example as a SLURM `--nice` value.

## 7. Digests of local files, and runs without the cache

**The digests of local files.** A task input is known by the digest of its content, so a run and a preview read every local input to hash it. For large inputs, like a reference genome, that costs as much time as some tasks. So the runner keeps the digest of each local file it read in a small SQLite file next to the run database, `<cache>/digests.sqlite` (`task_runner::DigestCache`), with what tells that version of the file: its device and inode, its size, the time of its last change of content, and the time of its last change of any kind, which no program can set back. When a later run or preview finds all of them the same, it takes the digest from there and does not read the file. Any change, also one that keeps the size and sets the time of the content back, gives the file a new time of change, so it is read again. A file that changed less than two seconds before it was read is not kept, as a file system with coarse times could miss a change right after the read. Downloaded copies of remote files are kept the same way; tasks of executors on other machines take such a file into the store under its known digest (`Store::put_file_as`), without reading it twice. The cache only saves time: when it cannot be opened, inputs are read as before.

**Runs without the cache.** `-no-cache` runs every task of one run, whatever the store holds: the run takes no result from the cache and keeps none there, and it waits for no other run that runs the same task (the scope `none` of each request, which a replay of one task uses too). A simulated run with `-no-cache` previews nothing from the cache, as such a real run would run everything. `-resume` still reuses the finished tasks of the run it continues, as the user asked for them.

```sh
iunctus run main.nf -executor local -no-cache
```

## 8. A store in layers

A store on S3 lets several machines share results, but every read of a result goes over the network. A store in layers puts a folder of this machine in front of it:

```toml
[store]
location = "s3://lab-cache/iunctus"    # shared by the machines of the lab
local = "/scratch/iunctus-store"       # a fast disk of this machine
```

- **Reads look in the folder first.** What the folder lacks comes from the shared store, and the folder keeps a copy: file contents, directory manifests and cache entries as soon as a run reads them, task outputs when a task of this machine reads them as inputs. A result that no task of this machine reads stays in the shared store only; its place for drivers and deliveries is then its address there.
- **New results go to both.** Outputs move into the folder, as in a store on this machine, and are uploaded to the shared store; contents, manifests, entries and marks of use go to both.
- **The shared store decides which result of a task is first**, so that machines agree: an entry goes to the shared store first, and the folder keeps a copy only of the entry that won.
- **Leases live in the shared store**, so that two machines that run the same task at the same moment run it once (section 3).
- **Clean-up works on the folder only.** `iunctus clean` removes from the folder what no rule keeps; the shared store keeps everything until it is cleaned on its own, with `iunctus clean -store s3://lab-cache/iunctus`.

`iunctus check-machine` shows the free space of the folder next to the answer of the shared store. The code: `content_store::Store::layered` and `layers.rs` (`local_file` takes an object into the folder); `Setup::store` in the binary.

## 9. Where it is in the code

- `task_runner::Runner::execute`: the hit, the wait for the lease of a key, the lease of the run, and the entry with its time.
- `task_runner::CacheEntry` (`entry.rs`): the entry and the check that its files are still there.
- `content_store::{Lease, Holder}` (`lease.rs`): leases in both kinds of store; `Store::{objects, remove, mark_used, remove_entry}` (`objects.rs`).
- `task_runner::clean`: `Keep`, `plan` and `remove`; `iunctus/src/clean.rs`: the rules from the command line, the runs from the database, the listing and the scratch folders.
- `task_runner::Lookup` (`lookup.rs`, `inputs.rs`): the key of a task without side effects; `iunctus/src/preview.rs`: `Previewed`, the simulator with the cache; `Database::pins`.
- `ExecutionGraph::cache_note` and `StepNode.reused`: the counts in notes, summaries and pictures.
- `Database::chains` (`chains.rs`), `RunnerExecutor::priorities`, `Placement::priority` and `Pool::share`: the longest chain first.
- `task_runner::DigestCache` (`digests.rs`) and `Inputs::hash` (`inputs.rs`): the digests of local files; `-no-cache` in `iunctus/src/arguments.rs` and `main.rs` (`RunnerExecutor::reuse`, and no preview).

## 10. Checks

- `task-runner/tests/runner.rs`: shared results; a result whose files are gone runs again; entries record their time and their last use; three requests of two runs at the same moment run the task once; a failed task leaves its turn to the next; a lease whose holder is killed frees the task at once (with the `flock` tool of util-linux, which holds the lock the way a run does); waiting stops when the run is cancelled; a lookup finds results and writes nothing; clean-up keeps what named runs need, what was used lately and what took long, and everything newer than a run that goes on.
- `content-store`: leases in a store on this machine and in an object store, including one whose holder stopped renewing it; listing and removing objects; marks of use.
- `executor` (unit test of the pool): tasks that wait take their shares by priority, and a task that stops waiting leaves the queue.
- `run-database/tests/database.rs`: which runs go on, which started lately, what they refer to, the pins of real runs, the chains of steps with their times; eight runs that open a new database at the same moment, twenty times over (two races that made the second of two runs fail are fixed); four runs that record at the same moment.
- `iunctus/tests/cache.rs`, with Docker and `debian:stable-slim` (`cargo test -p iunctus --test cache -- --include-ignored --test-threads 1`): a module that two Nextflow pipelines vendor runs once, although its process has another name and sits in a subworkflow in the second; a CWL tool and an Argo template written alike share their task, while a Snakemake rule that does the same work runs; two `iunctus run` processes started at the same moment on one store run a task once (`[done]` in one, `[cached]` in the other); clean-up after real runs removes what no rule keeps and nothing that a kept run needs; the preview before and after real runs, in the notes, the summaries, the SVG and the run database; the longest chain first: tasks that each take the whole machine run one after another, and where the first run started three short tasks before a long chain, the second, with the times of the first, starts at most the one that found the machine free.
- `task-runner/src/digests.rs`: digests come from the cache until the file changes, also when the size stays the same; a file that changed just before it was read is not kept; a second process sees the same cache. `iunctus/tests/cache.rs` (Docker): with `-no-cache` a preview finds nothing in the cache and a real run runs every task.
- `content-store/tests/layers.rs`: new objects go to both layers; reads take what the folder lacks from the shared store and keep it, also a directory output of an object store; the shared store decides the first result of a task; leases live there. `task-runner/tests/runner.rs`: a result of another machine answers a task through the shared store, and a task that reads its output takes it into the folder. `iunctus/tests/cache.rs` (Docker): two machines with folders of their own in front of one shared store; the second runs nothing and keeps the entries it read.

## 11. Not yet

- **One layout of tasks for all languages.** Tasks of different languages share results only where their descriptions are equal. Writing paths inside the work directory the same way in every driver would make more of them equal.
- **Trust in a shared cache** ([roadmap](../roadmap.md), open question 2).
- **A preview that asks registries** for the digests of images that no real run used.
