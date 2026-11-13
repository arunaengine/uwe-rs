<!-- Contracts for job planning, embedded code, workflow sources, and corpus validation. -->
<!-- Copyright (c) 2026 The uwe Contributors -->
<!-- SPDX-License-Identifier: MIT or Apache-2.0 -->

# Source contracts

## src/benchmark.rs

The benchmark file of a Snakemake job (`benchmark:`), which Snakemake writes after the job
with the time it took and what it used: tab-separated values under a header, or one JSON
line when the name ends with `.jsonl` (Snakemake 9.27, `benchmark.py`). uwe fills it
from the tool's own start and end and from what the cgroup of the container counted;
the values that it does not know (`max_vms`, `max_uss`, `max_pss`) are `NA`, as Snakemake
writes them when it could not sample the job.

## src/commands.rs

Commands that workflow code runs while the workflow is planned, like `shell()` or
`subprocess.run`. They never run on the machine of uwe (decision 11): each becomes a
container task that the engine runs like the task of a job. The image is the workflow's
global `container:`, else a small default image. The task runs in the working directory of
the workflow, which it changes where it is, as Snakemake runs the command there: the files
that the command writes stay for the workflow code and the jobs after it. So such a task
never comes from the cache.

The session answers each request of the engine once. A command hands its task to the engine
as the answer to the request that the session is handling, and waits for the outcome of that
task. Outcomes of other tasks that come meanwhile get an empty answer and wait until the
session has handled the request that ran the command.

## src/embedded/data.rs

The values of a job as plain data, which each language's preamble writes in its own
syntax. Snakemake hands the values to a script by pickling them (Python) or by writing them
as code (R, Julia, Bash, Rust); uwe writes code for every language, so it first turns
the interpreter's values into this plain form.

## src/embedded/globals.rs

The Snakefile's globals that the code of a `run:` block uses, carried over from the
native interpreter as Python code: plain values as literals, tables as Snakemake stores
them, modules as imports (local ones with their files), functions of the workflow by their
source text after what they use, and objects that only the interpreter has as objects that
raise an error when the block uses them.

## src/embedded/mod.rs

The context of embedded code: what the code of a job gets besides its files. Snakemake
gives scripts, wrappers and notebooks a `snakemake` object with the job's input, output,
params, wildcards, threads, resources, log, config and rule, through a preamble in each
language, and it runs `run:` blocks as functions with these names and the Snakefile's
globals. uwe builds the same preambles while it plans the job, so that the code runs
unchanged in the task's container (decision 11).

## src/embedded/python.rs

Python: the preamble of scripts, wrappers and notebooks, which makes the `snakemake` object
as Snakemake's `PythonScript.generate_preamble` does. Snakemake pickles the object; the
container of a task has no Snakemake to unpickle it with, so uwe writes the object as
Python code that builds it with the stand-in package (`snakemake/iocontainers.py`).

## src/embedded/run.rs

`run:` blocks. Snakemake runs a block as a function of the Snakefile's module, with the
job's values as its arguments and the Snakefile's globals around it. In uwe the
Snakefile runs in the native interpreter (decision 17), and a job runs in a container
(decision 11). So the block runs in the task's container, in a module that uwe writes:
the rule function of Snakemake with the block as its body, and the Snakefile's globals that
the block uses, carried over from the interpreter: plain values as literals, modules as
imports, and functions of the workflow by their source text, with what they use in turn.
Live objects of the interpreter, like the `workflow` object, cannot go: using one raises
an error that says so.

## src/embedded/script.rs

Scripts and wrappers: the script with its preamble in front, as Snakemake's `script()`
writes it to `.snakemake/scripts`, the command that runs it, and the files it needs besides
the job's inputs: the stand-in of the snakemake package for Python, and the folder of the
script, whose other files scripts import or source.

## src/embedded/snakemake/__init__.py

The stand-in of the snakemake package that uwe gives the code of jobs in their
containers: the `snakemake` object of scripts, wrappers and notebooks, `shell()`, and the
helpers that such code imports from snakemake. It needs only the Python standard library, and runs
with Python 3.7 or later, as the scripts of Snakemake 9.27 do.

## src/embedded/wrapper.rs

Wrappers: where a wrapper's script is, as Snakemake's `wrapper.get_script` finds it. A name
like `v8.1.1/bio/bgzip` is a folder of the snakemake-wrappers repository at that tag, other
names are below the default prefix, and `file:` or web addresses are taken as they are. A
`wrapper-prefix` may be a web address, a local folder, or a `git+file:` repository, whose
names start with the tag or branch to read. The
script is the first of `wrapper.py`, `wrapper.R`, `wrapper.Rmd` and `wrapper.jl` that
exists. Scripts on the web are downloaded once into the folder of downloads; a simulated
run reads none of them and takes `wrapper.py`.

## src/inputs.rs

The inputs of a Snakemake workflow as one JSON Schema (decision 24): the schema of
`validate(config, ...)` where the workflow calls it, else its config files with the types of
their values. A table that the workflow validates, like its sample sheet, is the content of
the config value that names its file, as an array of rows, and a PEP project is the input
`pepfile`. Texts that name existing files or directories are marked as such. The values of a
run are its config, and the config keys that the code reads are found in its Snakefiles and
scripts.

## src/iofile.rs

The attributes that Snakemake's file objects have beyond those of `str`. Snakemake gives
workflow code the files of rules as `_IOFile`, a subclass of `str`, and the folder of the
current Snakefile as a `SourceFile`. Here they are texts tagged with `rule` or
`source_file`, which behave like texts and get these attributes on top.

## src/lib.rs

Runs Snakemake workflows: evaluates the Snakefile natively, plans the job graph backwards
from the targets, and asks for one task per job. The Python of Snakefiles runs in
`python-evaluator`; `script:`, `wrapper:`, `notebook:` and `run:` jobs become tasks that
run their code in a container, and so do the Python packages without a stand-in that the
Snakefile imports (decision 19).

## src/metadata.rs

The records that Snakemake keeps in `.snakemake/metadata` of the working directory: for each
output, the checksums of the inputs that made it. Snakemake's `mtime` trigger does not run
a job again for an input that is newer than an output but has the checksum that the record
of each output names (Snakemake 9.27, `DAG.is_same_checksum`).

## src/output_schema.rs

The outputs of a Snakemake workflow as one JSON Schema (decision 24): the files of its target
rules, which are the first rule unless the launch names targets. A rule with outputs gives
them; a rule without, like `all`, gives the inputs that it asks for. A named group of files is
an output by that name, and the other files are the output `targets`. Each output holds the
files for the config of the launch, as `const`.

## src/pathvars.rs

Path variables: names in angle brackets, like `<results>`, that the files of rules may
contain and that the workflow, a module, the config or a rule sets, as Snakemake 9.12
introduced them. A definition from a narrower place wins: a rule over a module or the
config, those over the workflow's `pathvars:`, and that over the defaults.

## src/pep.rs

PEP projects: `pepfile:` loads a project as peppy 0.40 does, and `pep` in the Snakefile is
the project. peppy reads the project config, the sample table and the subsample tables as
texts, applies the sample modifiers (remove, append, duplicate, imply, derive), merges
samples of the same name and the rows of subsample tables, and gives the samples as a table
indexed by their names.

Tables in Excel files (`.xlsx`) are read with calamine, a reader in pure Rust, as pandas'
`read_excel` reads their first sheet with peppy's settings.

## src/plan.rs

Plans the jobs of a workflow backwards from its targets, as Snakemake's DAG does: each
input file is made by the job of a rule whose output pattern matches it, or is a file that
exists. A simulated run may assume that input files that no rule can make exist, since raw data
is not part of a workflow.

## src/profile.rs

The workflow profile: settings in `profiles/default/profile.yaml` (or `config.yaml`) that
Snakemake reads by itself as default command-line options. The ones that change how the
workflow is planned are kept: config files and values, default resources, and resources
and threads per rule. `cores`, `max-threads`, `jobs` and `resources` set the global resources
of the run, as Snakemake's command line does: threads are capped at `max-threads` and at the
cores, and a local run without `cores` takes `jobs` as its cores.

The settings that change what runs work as on Snakemake's command line: `notemp` keeps
`temp()` outputs, `wrapper-prefix` says where wrappers are, `groups` and `group-components`
form group jobs, `prioritize` adds targets whose jobs start first, `set-scatter` overrides
`scattergather:`, `consider-ancient` marks inputs `ancient()`, `allowed-rules` limits the
rules that make files, and `batch` runs one part of the inputs of a rule. `envvars` gives
the jobs the named variables of this environment, by name only in the task, `nolock` leaves
the working directory unlocked, and `rerun-incomplete` makes outputs with an incomplete
record again instead of stopping.

## src/python.rs

The Python container of a run (decision 19): the Python packages without a stand-in that
workflow code imports run in one container, which the local container engine starts. uv
installs them from PyPI into an environment that is kept for the next runs with the same
packages.

## src/remote.rs

Workflow files on the web, like the Snakefile of a module from `github(...)` or an
`include:` of a web address. Snakemake clones the git repository or downloads the file.
uwe mirrors each repository at its reference, or each web server, into a folder of
downloads, and downloads a file the first time the workflow reads it. So paths relative to
a remote Snakefile, like its includes, config schemas and scripts, work as local ones do.

`gitfile(repo, path, tag=..., ref=..., commit=...)` names a Snakefile in a git repository
at a tag, a ref or a commit (src/gitfile.rs). A ref is any revision that `git show` takes,
like a branch, a short commit ID or `main~1`. As in Snakemake, it also names a script, a
Conda environment or the schema of `validate`, but not a `configfile:`. A repository of this machine is read with gix, without the git
program. A repository at an `https://` address is fetched by the Git client of the run,
once per run, signed in with the account of an https storage provider of the config file.
Either is written out at its reference into the folder of downloads.

## src/resources.rs

The resources of a job, as Snakemake gives them to jobs that run on a remote executor:
the defaults for memory, disk and the temporary folder, under the `default-resources` of the
workflow profile, under the rule's `resources:`, under the profile's `set-resources`. Memory
and disk are kept in megabytes, and shown also as a human-readable size and in mebibytes.

## src/scheduling.rs

The global resources of a run, as Snakemake's scheduler shares them: a job starts only when
its threads (`_cores`), its count (`_nodes`) and its integer resources fit beside the jobs
that run. The others wait until a running job ends. As in Snakemake, `_cores` limits the
threads of all jobs only when `_nodes` does not limit their number. Each integer resource of a
job is capped at the global resource of its name.

## src/services.rs

Service jobs: a job with a `service()` output provides something, like a socket, that the
jobs that read it use while it runs. Snakemake runs such a job in one group with the jobs
that use it, all at once, and ends it with SIGTERM once they ended; the group succeeds when
they do. uwe runs the group as one task in one container: the service starts first in
the background, each job that uses it starts once the service made its output, and the
service gets SIGTERM when they ended. The output of a service is no file that a run keeps
or checks.

## src/sources.rs

The source files of a workflow, which every job sees at their paths in its work directory,
as Snakemake's jobs see them in the working directory.

A task sees only the files that it gets (decision 11). Snakemake runs jobs in the working
directory, where shell commands also call scripts and packages of the workflow that their
rule does not declare. So every task also gets the files that Snakemake deploys to jobs
that run without a shared file system (`DAG.get_sources` of Snakemake 9): the files that
git tracks below the working directory, the folders of the rules' scripts and notebooks,
the conda files of the rules and the config files, each at most 10 MB and below the working
directory. The Snakefiles are left out: a job runs the code of its own rule, which its task
has already. Each source is a read-only input with its digest in the cache key, so the
result of a task stays tied to what it could read.

## src/storage.rs

Snakemake's storage: files in remote storage, like `storage.http(url)` or
`storage("s3://bucket/key")`. Snakemake's storage plugins download such an input into
`.snakemake/storage/<provider>/...` before the job runs, and the job sees that local path.
uwe gives the job the same local path, and its task gets the file from the storage
address, which the executor downloads. While the workflow is planned, a file in storage
counts as present, as uwe does not look into the storage itself.

## tests/names.rs

Names of inputs, outputs, logs and params that Snakemake's named lists keep for themselves,
like `items`, fail as they do in Snakemake 9.27.0, whose `snakemake -n` says:
`AttributeError in file "Snakefile", line 1: invalid name for input, output, wildcard,
params or log: items is reserved for internal use`.

## tests/run_test_workflows.rs

Runs every workflow of the Snakemake test workflows with the simulator and compares the
outcome with `tests/fixtures/run_test_workflows.txt`: `ok` and the number of tasks, or the exit
code (33 for unsupported features) and the first line of the error. Each workflow runs
the way `tests/fixtures/run_test_workflows_launches.txt` says, which `launches.py` next to it
writes: as its CI tests it, or as Snakemake's own tests run it. Input files that no rule
makes are assumed to exist, since the test workflows hold no large data files. Up to eight
tasks are pending at once and finish in a shuffled order, whose seed
`SNAKEMAKE_TEST_WORKFLOWS_SEED` sets (1 by default), so that the driver meets the orders of
a real run. Each workflow runs
in a child process, so that a crash or a run of more than two minutes (exit code 124)
stays with its workflow. Run with `just snakemake-run-test-workflows-test`; on a difference the
test prints the lines of the new fixture. `SNAKEMAKE_TEST_WORKFLOWS_FILTER` runs only the
workflows whose name contains it; with `UPDATE_FIXTURE=1` it updates only their lines.

Before each run, the test describes the inputs of the workflow, as the binary does (decision
24), and compares the counts of its schema and of a check of the simulated run with
`tests/fixtures/run_test_workflows_inputs.txt`, with the totals of their outputs as the line
`(outputs)`; every schema must be valid JSON Schema.
`UPDATE_FIXTURE=1` writes that file too.

Workflows that import a Python package without a stand-in while they are planned need the
Python container of decision 19. Without a container runtime they stop with an error that
says so, and `tests/fixtures/run_test_workflows_containers.txt` must list them. With
`UWE_CONTAINER_TESTS=1`, the test runs only these workflows, with the container, and
compares their outcomes with that file; each may then take up to fifteen minutes.

## tests/sources.rs

Every job sees the source files of its workflow at their paths in its work directory, as
Snakemake's jobs see them in the working directory: the files that git tracks, the folders
of scripts, conda files and config files, but not the Snakefiles, and not where the job's
own files go.

## tests/workflows.rs

Runs each workflow of `tests/fixtures/workflows` with the simulator and compares the
graph and the tasks with the folder's `expected.txt`. A folder may have a `targets` file
with one target per line, and a `snakemake.txt` with the shell commands that
`snakemake --dry-run --printshellcmds` (Snakemake 9.27.0) prints for it, which the tasks
must run. `UPDATE_EXPECTED=1` rewrites the expected files.

## tests/fixtures/embedded/preambles/record.sh

Records the scripts with preambles that Snakemake 9.27.0 writes for the jobs of this folder
into snakemake-9.27.0/, with the folder of the run as `{folder}`. It needs uv with
snakemake 9.27.0 in its cache or on PyPI (UV_OFFLINE=1 uses only the cache); R, Julia and rust-script need not be installed,
as Snakemake writes each script before it runs it.

## tests/fixtures/embedded/real/record.sh

Records in expected-results/ what Snakemake 9.27.0 makes for the jobs of this folder that
need no image to run on the host: the Python script, the Bash script, the Python wrapper
and the run: block (Snakemake runs them without containers unless it is told to use them).
It needs uv with snakemake 9.27.0 in its cache or on PyPI (UV_OFFLINE=1 uses only the
cache).

## tests/fixtures/launches.py

Writes run_test_workflows_launches.txt: how the run test-workflows test launches each workflow of
the Snakemake test workflows. Run it from the repository root after `just snakemake-test-workflows-data`:

    uv run -q --with pyyaml --with snakemake==9.27.0 python workflows/snakemake/runtime/tests/fixtures/launches.py

A catalog workflow runs plainly in its repository, unless real Snakemake does better with
another launch (run_test_workflows_oracle.txt from oracle.py says so): the way its CI tests
it, or with the mandatory flags of its catalog entry. Better means a dry run that succeeds,
else a Snakefile that Snakemake evaluates. The CI launch is the
first step of its GitHub workflows that runs Snakemake on it, with the
snakemake-github-action or a `snakemake` command, other than linting, reports, unit test
generation and environment setup, and whose folders and files exist. The catalog launch
takes the config files and values of the mandatory flags. A test of Snakemake runs the way
the first test function of tests.py that uses its folder runs it, preferring runs that
should not fail.

## tests/fixtures/oracle.py

Writes run_test_workflows_oracle.txt: what real Snakemake 9.27.0 does with each workflow of the
Snakemake test workflows and each launch that launches.py considers for it. Each run is
`snakemake --dry-run --printshellcmds` in a copy of the workflow folder under the temporary
directory, so that Snakemake also formats the shell command of each job, as uwe does
when it builds the task of a job. Run it from the repository root after
`just snakemake-test-workflows-data`, then run launches.py:

    uv run -q --with snakemake==9.27.0 --with pyyaml --with pandas --with numpy --with peppy \
        --with biopython --with psutil --with requests --with openpyxl --with pyarrow \
        --with ruamel.yaml --with snakemake-storage-plugin-http \
        --with snakemake-storage-plugin-s3 --with snakemake-executor-plugin-slurm \
        python workflows/snakemake/runtime/tests/fixtures/oracle.py

With `--only FILE`, it runs only the workflows that FILE names, one per line, and replaces
their lines in the oracle file, keeping them even when Snakemake evaluates their first
launch: the evidence for the workflows that uwe cannot run.

The workflows of run_test_workflows_containers.txt import Python packages that run in the
Python container of decision 19. Their evidence comes from a run with those packages too:

    grep -v '^#' workflows/snakemake/runtime/tests/fixtures/run_test_workflows_containers.txt \
        | cut -d' ' -f1 > /tmp/containers.txt
    uv run -q --with snakemake==9.27.0 ... (the packages above) --with giturlparse \
        --with yatiml --with pyfaidx --with earth-osm --with pydantic --with python-magic \
        --with universal-pathlib --with snakebids --with bionumpy --with wget --with cyvcf2 \
        --with pybedtools --with google-auth --with pillow --with utils \
        python workflows/snakemake/runtime/tests/fixtures/oracle.py --only /tmp/containers.txt

Snakemake is only the reference here; uwe never runs it (decision 1).

## src/inputs.rs:354

The config keys that the code of the workflow reads, found in its Snakefiles and in the
scripts and notebooks of its folder: `config["key"]`, `config.get("key")`, `"key" in
config`, `{config[key]}` in a shell command, `snakemake.config["key"]` of a script,
`snakemake@config[["key"]]` of R, and `dpath="key/..."` of `lookup`.

## src/workflow.rs:469

Includes a Snakefile: parses it and runs its code with the workflow's globals.
The Snakefile of a meta-wrapper of the Snakemake wrapper repository, like
`v1.0.0/meta/bio/bwa_mapping`, and the tag that replaces `master/` in the names of its
wrappers for meta-wrappers older than 8.0.0.

## tests/run_test_workflows.rs:355

The problems of the records of workflows that do not run: a failing workflow without a
record, a record of a workflow that runs, a reason that is not one of the three, and an
`invalid` or `missing` workflow that real Snakemake evaluated with the same launch. A
workflow that needs the Python container needs no record when it has none, but a line in
`run_test_workflows_containers.txt`.

