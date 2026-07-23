<!-- The design of the context of embedded code: the snakemake object of scripts, wrappers and notebooks. -->
<!-- Copyright (c) 2026 The iunctus Contributors -->
<!-- SPDX-License-Identifier: MIT or Apache-2.0 -->

# The context of embedded code

Status: done, 2026-09-30: Snakemake scripts, wrappers, notebooks and `run:` blocks (`snakemake-runtime/src/embedded/`), and Nextflow `exec:` sections (section 6, in `nextflow-runtime`).

## 1. The design in short

1. Snakemake gives the code of a job a `snakemake` object: input, output, params, wildcards, threads, resources, log, config, rule, `bench_iteration` and `scriptdir`. It writes it in front of the script as a preamble, in the script's language, into `.snakemake/scripts/`.
2. iunctus builds the same preamble while it plans the job (`embedded::Context`), and gives the task the script with the preamble as an input with literal content at `.snakemake/scripts/<name>`. The script then runs unchanged in the task's container (decision 11).
3. The preambles of R, R Markdown, Julia, Bash and Rust are the text that Snakemake 9.27.0 writes, byte for byte (section 4).
4. Python is the one difference: Snakemake pickles the object, and the container has no Snakemake to unpickle it. iunctus writes Python code that builds the object with a stand-in of the snakemake package (`embedded/snakemake/*.py`, only the standard library, Python 3.7 or later). The preamble keeps Snakemake's three lines, so line numbers in tracebacks shift as in Snakemake.
5. The stand-in gives what job code imports from snakemake: the `Namedlist` family and the object's class (`iocontainers`), `shell()` with Snakemake's formatting, `snakemake.utils` (`format`, the formatters, `makedirs`, `update_config`, `validate` with the image's jsonschema), `snakemake.io` (`expand`, `glob_wildcards`, the flags), `snakemake.logging.logger` and `snakemake.exceptions`. As in Snakemake 9.27, `from snakemake.script import snakemake` fails: the object is a global of the script.
6. The stand-in is one folder, written once per run into the scratch folder of the cache (`<scratch>/snakemake/python-<key>/`), which each Python task sees read-only at `.snakemake/iunctus`. So tasks do not each carry its modules.

## 2. What a task gets

| Input | Where the task sees it |
|---|---|
| the script with its preamble (literal content) | `.snakemake/scripts/<file name>` |
| the stand-in of the snakemake package (Python, Xonsh, Hy, Python notebooks) | `.snakemake/iunctus/snakemake/` |
| the folder of a local script, read-only | its path on this machine, which is `snakemake.scriptdir`; when that folder holds the working directory, only its files of the script's language |
| the folders of the job's outputs, logs and benchmark (empty, made) | their relative paths, as Snakemake makes them before a job |

The command is Snakemake's: `python`, `Rscript --vanilla`, `julia`, `bash`, `rust-script` with Snakemake's dependencies, `xonsh`, `hy`, and for R Markdown `rmarkdown::render(...)` with the output and working directory found in the container. Every task of a job also gets the job's threads in `OMP_NUM_THREADS` and the other variables that Snakemake's `shell()` sets, and its `tmpdir` resource in `TMPDIR`.

**Wrappers** (`embedded/wrapper.rs`) are found as Snakemake finds them: `v8.1.1/bio/bgzip` is the folder of that tag of snakemake-wrappers on `raw.githubusercontent.com`, other names are below Snakemake's default prefix, and `file:` and web addresses are taken as they are. The script is the first of `wrapper.py`, `wrapper.R`, `wrapper.Rmd` and `wrapper.jl`. Scripts on the web are downloaded once into the folder of downloads while the job is planned. The wrapper's `environment.yaml` names its Conda environment, as before.

**Notebooks** (`embedded/notebook.rs`) get the preamble as their first cell, tagged `snakemake-job-properties`; an earlier preamble cell goes. The kernel language comes from the notebook's metadata (Python or R). The command runs papermill when the image has it, else `jupyter-nbconvert --execute`, and writes the executed notebook to the log named `notebook`, as Snakemake does. Two additions: the preamble first moves a kernel that started in `.snakemake/scripts` to the working directory, and a container whose home is not writable (it runs as the user of the run) gets the temporary folder of IPython as its home.

## 3. Simulated runs and errors

A simulated run reads local scripts, but no script on the web. Snakemake's dry run makes no preamble, so a simulated run never stops on one: a script it cannot read, a value that a language cannot take, or an unknown language give the old task (the script staged as it is). A real run fails there as Snakemake's job fails, with Snakemake's messages, like `Unsupported value for conversion into R: ...` or `Object of type complex is not JSON serializable` for Rust.

Python gets every value: a value that is no plain data (a function, a module) becomes an object that raises a clear error when the script uses it. pandas tables and series and numpy arrays in params are stored as plain data and made again in the script, as Snakemake does. The numbers of `Paramspace.instance()` are numpy scalars in Snakemake (scripts call `.item()` on them); the Paramspace stand-in tags them with their numpy type, and the Python preamble makes them as numpy scalars. polars tables in params are not carried yet (the polars stand-in has no `to_dict`): scripts get an object that says so.

A real run now also checks, as Snakemake does, that a job that ended well made all its outputs (`MissingOutputException`), and makes the `touch()` outputs that the job did not make.

## 4. Checks

- `snakemake-runtime/tests/preambles.rs` compares the R, R Markdown, Julia, Bash and Rust scripts with the ones Snakemake 9.27.0 wrote for the same jobs (`tests/fixtures/embedded/preambles/`, recorded by `record.sh` there with `PYTHONHASHSEED=0`, since Snakemake orders memory and disk resources by a set).
- `snakemake-runtime/tests/fixtures/embedded/real/` has a job of every kind in images that are on this machine: `python:3-slim`, `debian:stable-slim`, an R image with rmarkdown and pandoc, and `pandas/pandas:pip-all` with nbconvert. `record.sh` recorded Snakemake's outputs of the Python, Bash and wrapper jobs in `expected-results/`. `iunctus run <copy> -executor local` gives the same files, and the R, R Markdown, R wrapper and notebook jobs give the right content.
- Unit tests in each module of `embedded/`.
- Snakemake's own tests that use scripts, notebooks or `run:` blocks and need no network, run for real with `-executor local -default-image pandas/pandas:pip-all` (2026-09-30): 24 of 41 give exactly the files of their `expected-results`. Of the others, 7 need options of Snakemake's test suite (like `--set-threads` or `--all-temp`), 2 differ only in the line ends of the repository's expected files, 1 needs polars in the image, and 7 fail outside embedded code: the engine stops running tasks at the first failure where Snakemake lets them end (test_github_issue1261), a task sees only the files that its job names (test_github_issue4003), the runner cannot store a file output inside a directory output of the same job (test_github_issue3687) or prepare a task whose output is already in the working directory (test_delete_output, test_protected_symlink_output, test_symlink_time_handling), and the parser rejects test_issue1256.

## 5. `run:` blocks

Snakemake runs a `run:` block in its own process, as a function that its parser makes of the block, `__rule_<rule>(input, output, params, wildcards, threads, resources, log, rule, ..., jobid, ..., __is_snakemake_rule_func=True)`, in the globals of the Snakefile. In iunctus the Snakefile runs in the native interpreter (decision 17), but a job is a task, and a task runs in a container (decision 11). Both hold with this design: the block runs in the task's container, in a module that iunctus writes while it plans the job (`embedded/run.rs`), at `.snakemake/scripts/<rule>.run.py`, run with `python3`:

1. The stand-in of the snakemake package on the module path, and the names that every Snakefile sees: the modules and names that Snakemake's `snakemake.workflow` imports (`os`, `Path`, `logger`, ...), `shell` and the functions of `snakemake.io` (`expand`, `temp`, ...).
2. `config`, the job's config, and the Snakefile's settings of `shell()` (executable, prefix and suffix).
3. The Snakefile's globals that the block uses (`embedded/globals.rs`), found by the names in its syntax tree, and in turn those that the carried functions use, each before what uses it: plain values as literals, pandas tables and series and numpy arrays as Snakemake stores tables in params, modules as imports, functions and lambdas of the workflow by their source text, functions and classes of library modules as imports from their module. A local module of the workflow (a `.py` file that the Snakefile imports) is staged at its path on this machine, or its top package, and its folder is put on the module path. An import that the image cannot do binds an object that says so when the block uses it.
4. The block as the body of the rule function, with the indentation it has in the Snakefile, so that texts over several lines stay as written.
5. The job's object, made as for scripts, and the call of the function with its values; `shell()` in the block finds the job's threads and resources among the function's locals, as in Snakemake, and runs its command in the same container.

The image of the task is the rule's `container:` (Snakemake runs the block's `shell()` commands in it; it must have `python3`), else the image of `-python-image`, else the executor's `-default-image`.

**Limits.** Objects that only the native interpreter has cannot go to the container: the `workflow`, `rules` and `checkpoints` objects, instances of classes of the workflow, classes of the workflow (whose source the interpreter does not keep), objects of the Python container of decision 19 other than modules, and files that the workflow read into objects other than pandas tables. The module binds each to an object that raises, when the block uses it, an error that names it and says why. A block sees only the files of its job, like every task, not the whole working directory as in Snakemake. Job ids are those of iunctus, not of Snakemake. Tested: `tests/run_blocks.rs` checks what a module carries over; the `run_block` job of the real-run fixture gives the files that Snakemake 9.27.0 gives.

## 6. Nextflow `exec:` sections

Nextflow runs the `exec:` section of a process in its own process, as Groovy code, with a work folder for the task, `task.workDir`, where the code makes the files that the outputs collect. In iunctus the section is evaluation in the native interpreter (decision 17), not an external command, so decision 11 asks no container of it:

1. When a task of such a process is asked for, the runtime makes a folder for its attempt in the run's scratch folder (`<scratch>/nextflow-exec/<number of the iunctus process>-<run name>/<task id>-<attempt>/`), gives it to the task as `task.workDir`, and the interpreter runs the section, which sets the variables of `val` outputs and writes files there.
2. The runtime lists what the section made, and asks the engine for a task without image and command, which the engine finishes at once, so that the task stands in the graph with the files it read. When its outcome comes, the outputs bind to the files that the section made, as Nextflow collects them from the work folder, and `publishDir` delivers them.
3. A task that reads a file that an `exec:` section made lists that task in `after`, so the graph shows the order and the engine keeps it.

**Limits.** A section that fails stops the run, as Nextflow's default error strategy does; the failed attempt is not in the graph. The folders of `exec:` sections stay in the scratch folder of the cache after the run. As in Nextflow 26.04.6, the section reads its `path` inputs by their names; a section that reads their content works in iunctus, where Nextflow cannot find the files (they are relative to its launch folder). Tested: `nextflow-runtime/tests/exec.rs` runs `tests/fixtures/exec` with the simulator and compares the files that the sections make with what Nextflow 26.04.6 publishes (`record.sh`); a real run with the local executor publishes the same files and prints the same line.

## 7. Not built yet

- A default Python image with the packages that a script or block imports (uv, as decision 19 does), for Python code without a container. It needs the network.
