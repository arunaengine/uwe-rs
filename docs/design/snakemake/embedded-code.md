<!-- The design of the context of embedded code: the snakemake object of scripts, wrappers and notebooks. -->
<!-- Copyright (c) 2026 The iunctus Contributors -->
<!-- SPDX-License-Identifier: MIT or Apache-2.0 -->

# The context of embedded code

Status: scripts, wrappers and notebooks done, 2026-09-30. `run:` blocks and Nextflow `exec:` sections are not built yet (section 5). The code is in `snakemake-runtime/src/embedded/`.

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

Python gets every value: a value that is no plain data (a function, a module) becomes an object that raises a clear error when the script uses it. pandas tables and series and numpy arrays in params are stored as plain data and made again in the script, as Snakemake does.

A real run now also checks, as Snakemake does, that a job that ended well made all its outputs (`MissingOutputException`), and makes the `touch()` outputs that the job did not make.

## 4. Checks

- `snakemake-runtime/tests/preambles.rs` compares the R, R Markdown, Julia, Bash and Rust scripts with the ones Snakemake 9.27.0 wrote for the same jobs (`tests/fixtures/embedded/preambles/`, recorded by `record.sh` there with `PYTHONHASHSEED=0`, since Snakemake orders memory and disk resources by a set).
- `snakemake-runtime/tests/fixtures/embedded/real/` has a job of every kind in images that are on this machine: `python:3-slim`, `debian:stable-slim`, an R image with rmarkdown and pandoc, and `pandas/pandas:pip-all` with nbconvert. `record.sh` recorded Snakemake's outputs of the Python, Bash and wrapper jobs in `expected-results/`. `iunctus run <copy> -executor local` gives the same files, and the R, R Markdown, R wrapper and notebook jobs give the right content.
- Unit tests in each module of `embedded/`.

## 5. Not built yet

- **`run:` blocks.** The plan: the block runs as a task in a Python container, as a function with Snakemake's parameters (`input`, `output`, `params`, `wildcards`, `threads`, `resources`, `log`, `rule`, `jobid` and the others), in a generated module that holds the Snakefile's globals that the block uses: plain values as literals, imports as imports, functions and lambdas of the workflow by their source text (with what they use, in turn), and `shell()` of the stand-in, which runs in the same container. Values that are live objects of the native interpreter (a pandas table read while planning, objects of the Python container of decision 19, the `workflow` object) cannot go: they become objects that raise a clear error when the block uses them. The image: the rule's `container:`, else `-python-image`, else the executor's `-default-image`.
- **Nextflow `exec:` sections** (unsupported today, exit 33). The plan: the driver evaluates the section in the native interpreter when the task's inputs are there (decision 17: evaluation, not a command), with `task.workDir` a folder of the run's scratch, collects the outputs by their patterns there, and asks the engine for a task without image and command (a no-op, decision 11 needs no container for it), so that the task is in the graph; the tasks that read its outputs list it in `after`.
- A default Python image with the packages that a script or block imports (uv, as decision 19 does), for Python code without a container. It needs the network.
