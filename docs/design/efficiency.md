<!-- How iunctus asks for right-sized resources, runs short tasks together, and sends tasks to the site near their data. -->
<!-- Copyright (c) 2026 The iunctus Contributors -->
<!-- SPDX-License-Identifier: MIT or Apache-2.0 -->

# Efficiency: right-sized resources, short tasks together, tasks near their data

Status: sections 1 and 2 are implemented, 2026-09-30. Section 3 is a design; only its first part is implemented (section 3.4).

The run database knows what every task of a real run used: the CPU time and the peak of memory of its container, the time of its tool, and the time that the executor spent besides the tool ([runs](runs.md), section 6.1). Three features use it, so that runs take less compute and less waiting than with the native engines:

- **Right-sized resources.** A task asks for the CPUs and memory that the tasks of its step used in earlier runs, with a margin, instead of what the workflow names. So more tasks fit on a machine.
- **Short tasks together.** Short tasks of a step run one after another in one container, or one TES task, instead of one container each. So their start-up time is spent once.
- **Tasks near their data.** With several TES servers, a task goes to the site that is near its inputs.

| Crate | What it does here |
|---|---|
| `task-model` | `Measured`: what a task used; `right_sized`: the resources of a task from the measures of its step; `per_container`: how many short tasks share a container |
| `run-database` | `Database::measured`: the measures of each step in the latest five real runs of a workflow |
| `task-runner` | Applies them: `resources.rs` right-sizes the first attempt of a task, `together.rs` runs groups of short tasks |
| `iunctus` | Reads the measures before a run, turns the features off (`-no-right-sizing`, `-no-grouping`), and says what changed after the run |

## 1. Right-sized resources

Workflows ask for more than their tasks need, so that the largest input fits: nf-core modules ask for 6 GB for a process that needs 200 MB for most samples. On a machine or a cluster that reserves what tasks ask for, fewer tasks run at once than could.

**What is measured.** The wrapper of each task reads the cgroup of its container when the tool ends: the CPU time (`cpu.stat`), the peak of memory (`memory.peak`, page cache included) and the bytes of block I/O. The run database keeps them for each attempt (`attempts.cpu_usec`, `memory_peak`), with the tool's own start and end, and the size of each input file (`files.size`). Docker and Podman give each container its own cgroup; where a container has none, nothing is measured, and nothing changes.

**What a task asks for.** Before a real run, iunctus reads what the tasks of each step used in the latest five real runs of the same workflow (`Database::measured`): the attempts that ran and succeeded, not those from the cache. The first attempt of each task then asks for:

- **Memory:** the peak of the earlier task of its step with the smallest input that is at least as large as its own (by the bytes of its input files). When its input is larger than all earlier ones, the peak of the largest grows in step with the input, and it is never less than any earlier peak. Without sizes, the highest earlier peak. A quarter more goes on top, and at least 64 MiB.
- **CPUs:** the most CPUs that an earlier task of the step kept busy on average (CPU time over the time of the tool), a tenth more, rounded up, at least 1.

Neither ever goes above what the task asks for: that is the most its workflow allows, since the drivers apply limits like Nextflow's `resourceLimits` before. A task that asks for no memory keeps none, since a limit could stop a task that its workflow never limited. The time limit stays as it is.

**Retries.** A retry asks for what the task asks for, never less: the first attempt may have failed for lack of memory. A Nextflow `errorStrategy 'retry'` with `memory { 2.GB * task.attempt }` works as before.

**What the user sees.** After a run that asked for less, iunctus says so, per step, with the most that one task asked for before and after:

```text
iunctus: tasks asked for less than the workflow, from what earlier runs used (-no-right-sizing keeps what the workflow asks for):
iunctus:   NFCORE_DEMO:DEMO:FASTQC (12 tasks): 2 CPUs instead of 6 CPUs, 512.0 MB of memory instead of 6.4 GB
```

A simulated run says what a real run would ask for (`a real run would ask for less ...`), for the tasks that a real run would not take from the cache. `-no-right-sizing` keeps what the workflow asks for.

The resources are not part of the cache key, so a right-sized task shares its result with the same task that asked for more.

## 2. Short tasks together

Each task of the local executor costs about half a second to start and end its container besides its tool ([runs](runs.md), section 6.1), a TES task often several seconds, a SLURM job more. A step with hundreds of tasks that run for a tenth of a second each spends most of its time on that. Nextflow's `array` directive and Snakemake's job groups help where the workflow sets them; iunctus decides from measured times, in every language.

**When.** The measures of section 1 also hold, for each attempt, the time that the executor spent on it besides the tool: from the hand-over of the task to its result, less the time of the tool (`Measured::overhead`, from the `started` and `finished` events). A step is short when its tools ran, on average, for less time than the executor spent besides on the least of its tasks (the least, so that time spent waiting for a free CPU does not count). Then as many of its tasks may share a container as fit into that time, one after another, at most 100 (`per_container`): so a group ends at most about one start-up later than a task alone.

**How.** The runner of a task that may share a container hands its job to a group (`task-runner/src/together.rs`) instead of to the backend. The group waits 250 ms for more such jobs with the same image and the same resources, then spreads them over groups of at most `per_container` tasks, or more when there are more groups than the machine has room for at once (its CPUs over the CPUs of a task), so that they still run side by side. A group of one runs alone, as before.

A group is one job of the backend, so it works with every backend: one container for the local executor, one TES task, one Kubernetes Job, one SLURM job. Its work directory holds a folder per task, `m0`, `m1`, ..., each with the `work`, `control` and `home` folders that a task has alone, and its inputs below `work`. Its command is a script that writes the wrapper of each task into its folder and runs it, one after another, whatever the ones before gave. Each wrapper finds its folders from its own path, so the script works wherever the backend puts the work directory. The job returns each folder as an output, and TES copies them back.

**Exact per task.** After the group ends, each task gets its folder back as its own scratch directory, and from there on everything happens as if it had run alone: its exit code, standard output and error, the tool's start and end, and the manifest of its outputs come from its own control folder; the runner stores its outputs and its cache entry, and the engine records its attempt. The counters of the container add up over the tasks, so each task gets what grew since the task before it (CPU time, block I/O); the peak of memory is the container's, so it is left out. A task that fails fails alone; the others of the group go on. The cache, the lease of a task that another run runs at the same moment, and the reproducibility check all happen per task before the group, as before.

**When a group fails.** A task that the group did not reach, as when its container stopped, fails as a failure of the machine, so the engine runs it again; a retry always runs alone. When every task of a group stops waiting, as on Ctrl-C, the group stops.

**What the user sees.** With `-executor local` and the other real executors, a line says when a group starts:

```text
[together] 12 short tasks run one after another in one container
```

`-no-grouping` runs each task in a container of its own. A task can join a group only when its work directory, inputs and outputs are relative to its work directory, it names an image and a command, and it changes no input where it is; the others run alone.

## 3. Tasks near their data

With data at several sites, like an S3 bucket in Hamburg and one in Heidelberg, each with a TES server next to it, moving a task is cheaper than moving its data. No native engine chooses a site by where the data is.

### 3.1 Sites in the config file

A site is a TES server and the addresses of the data that is near it: address prefixes of storage providers ([storage](storage.md)), or folders that the site shares with this machine.

```toml
[executor.tes]
server = "https://tes.example.org"            # tasks that are near no site

[executor.tes.sites.hamburg]
server = "https://tes.hamburg.example.org"
token = { environment = "HAMBURG_TES_TOKEN" }
near = ["s3://hh-sequencing/", "https://data.hamburg.example.org/"]

[executor.tes.sites.heidelberg]
server = "https://tes.heidelberg.example.org"
near = ["s3://hd-imaging/", "/mnt/heidelberg/"]
```

A prefix ends at a name, as for storage providers: `s3://hh` is near `s3://hh/a.txt`, not `s3://hh-sequencing/a.txt`.

### 3.2 The choice

Each task goes to the site that is near most of its input files: an input is near a site when its address, or its path on this machine, starts with a prefix of the site. The longest prefix wins, and a tie goes to the site named first in the file. A task near no site goes to the server of `[executor.tes]`. Outputs of earlier tasks are inputs by their place in the store, so a store at a site (`[store] location = "s3://hh-sequencing/iunctus"`) keeps the chain of tasks at that site. The runner sets the chosen site as the `executor` of the placement of the task, so that it is not part of the cache key: the same task at another site shares its result.

Counting files, not bytes, is a start: the sizes of remote files need a request per file before the run. A later version can weigh inputs by their size where the store or an earlier download knows it.

### 3.3 The data

Today a TES task reads its inputs as `file://` URLs of this machine, and writes its outputs back there ([executors](executors.md), section 3): the data goes through this machine, and every server must see its folders. For a remote site this is the part that must change, and it needs the network to test:

1. A remote input goes to the TES server as its address (`s3://...`, `https://...`), not as a download of this machine. The server fetches it with its own credentials, near the data. The cache key keeps the digest of the input; this machine asks the provider for the digest (a stored checksum, or the ETag of a single-part upload) instead of downloading the file, and downloads it only where no digest is known.
2. Outputs go to the store directly from the site, as TES output URLs of the store, when the store is at an address the site can write. The wrapper already lists each output with its size in the manifest; the digests are computed at the site by the wrapper, so this machine does not read the outputs.
3. Only the control folder (exit code, logs, manifest, digests) comes back to this machine.

### 3.4 What is implemented

The choice and the sending, which work offline: `[executor.tes.sites.<name>]` in the config file, the choice of a site for each task (`task_runner::SiteRule`), and a backend that runs each task on the TES server of its site (`task_runner::Sites`). The data still goes through this machine (section 3.3), so for now every site must see the folders of this machine, as Funnel servers on this machine do. The tests use two Funnel servers on this machine and folders as the data of each site.

## 4. Checks

- `task-model/src/resources.rs`: right-sized memory and CPUs, the margin, the least memory, never more than asked, memory by the size of the input, and how many short tasks share a container.
- `run-database/tests/insight.rs`: `the_measures_of_earlier_runs_come_by_step`.
- `task-runner/src/resources.rs` and `together.rs`: retries keep what the task asks for; the layout of a group and its script; tasks with absolute paths run alone.
- `iunctus/tests/efficiency.rs`, with Docker (`cargo test -p iunctus --test efficiency -- --ignored`): a second run gives its tasks the right-sized memory limit (read from the cgroup inside the container) and says so, `-no-right-sizing` keeps the workflow's; twelve short Nextflow tasks run in one container, each with its own outcome, log and output, one of them failing alone, and `-no-grouping` runs them alone. With `just funnel-server`: four short tasks run as one TES task.

## 5. Not yet

- Right-sizing by the size of each input separately, or by a fitted line, instead of the nearest earlier input.
- Raising the memory of a task whose earlier tasks needed more than it asks for, up to a maximum that the workflow names (like Nextflow's `resourceLimits`), so that fewer tasks fail for lack of memory.
- Short tasks of different steps with the same image in one group.
- Recording in the run database which tasks ran together, and the right-sized requests, for the page.
- The data of section 3.3, and weighing inputs by their size in the choice of a site.
