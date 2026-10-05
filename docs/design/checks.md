<!-- How iunctus tells what went wrong: the one form of every error, the exit codes, and the checks of a workflow and of the machine before a run. -->
<!-- Copyright (c) 2026 The iunctus Contributors -->
<!-- SPDX-License-Identifier: MIT or Apache-2.0 -->

# Errors, exit codes and checks

Status: implemented, 2026-09-30.

iunctus should be easy to set up: a laptop needs the binary and Docker or Podman, nothing else. When something is missing, iunctus says in plain words what to install or set. This page describes the form of every error, the exit codes, and the two checks that find problems before a run.

## 1. The form of an error

Every error has one form, whichever language, executor or part of iunctus finds it:

```text
iunctus: <where>: <what went wrong>; <what to do>
```

- **Where** is a place in a file (`main.nf:12:5`, `config.toml:7`), a task (`task FASTQC (1)`), an input (``the input `samples` ``) or an address (`s3://lab-data/a.fq`).
- **What went wrong** is one sentence in plain words.
- **What to do** comes after a `;` when there is a next step that the message can name: the option to set, the program to start, the file to fix.

```text
iunctus: main.nf:1:11: the included file /home/me/pipeline/modules/b.nf does not exist
iunctus: task A: the input /home/me/pipeline/data/samples.csv does not exist
iunctus: config.toml:4: `[storage.lab]` has no setting `regoin`; it knows `type`, `prefixes`, `endpoint`, `region`, ...
iunctus: cannot reach a Docker or Podman engine: no container engine answers at /var/run/docker.sock or /run/user/1000/podman/podman.sock; iunctus runs every task in a container, so start Docker, or turn on the socket of Podman with `systemctl --user enable --now podman.socket`, or name a socket with IUNCTUS_ENGINE; use `-executor simulator` for a run without containers, or `iunctus check-machine` to check the setup
iunctus: the TES server at http://localhost:8000 does not answer: Connection refused; start it, or name another with -tes-server or `server` in [executor.tes] of the config file
iunctus: -executor slurm needs the tools of SLURM, but `sbatch`, `squeue` and `scancel` are not on the PATH; run iunctus on a node of the SLURM cluster, like its login node
iunctus: unknown option `-bogus`; `iunctus help` lists the commands and their options
```

The binary gives every line of an error the prefix `iunctus: `, so that the user knows who speaks, also when a message comes from the code of a language, like a Nextflow exception. Lines that continue the line before them, which start with a space, keep their place. The numbers of system errors, like `(os error 2)` after `No such file or directory`, are left out; they say nothing more. Messages of the workflow itself, like a Nextflow `error()` or a Snakemake `WorkflowError`, keep the words of the reference tool, so that users can look them up.

The code: `iunctus/src/failure.rs` (`Failure`, the exit codes, and how a message is shown).

## 2. Exit codes

| Code | Meaning | Examples |
|---|---|---|
| 0 | The command did what it was asked | a run whose tasks all succeeded; a check that found no problem |
| 1 | The workflow failed or cannot run | a syntax error, a missing include or input file, inputs that do not fit the schema, an image that its registry does not have, a task that failed, a result that cannot be delivered |
| 2 | The command line is wrong | an unknown command or option, an option without its value, an option of another command, no workflow named |
| 3 | This machine or its config is not ready | a config file with a mistake, a secret whose environment variable is not set, no container engine, a store or run database that cannot be opened, a TES server that does not answer, SLURM tools that are missing, a cluster that failed a task again and again |
| 33 | The workflow uses a feature that iunctus does not support | a DSL1 Nextflow script (decision 14), an Argo resource template for Kubernetes objects (decision 18), an address that no storage provider serves |
| 130 | The run was stopped with Ctrl-C | |

33 is the code that CWL runners give for unsupported requirements. A run that fails for several reasons gives the code of the first one it meets.

## 3. Checking a workflow before a run

```sh
iunctus check <workflow file or directory> [-default-image <image>] [the options of iunctus run]
```

`iunctus check` finds everything that would stop a real run with the same options, at once and without running a task, in all four languages:

- **Unsupported features**, which stop a run with exit code 33, like DSL1 scripts or Argo resource templates.
- **Syntax and load errors** of every file of the workflow, and **broken references**: includes, modules and subworkflows, CWL `run` documents, Argo templates and template references. The Nextflow loader reports the errors of all its files at once; the other languages stop at the first error of their loader.
- **Inputs** against the JSON Schema of the workflow (`iunctus inputs`), as a real run checks them: a required input without a value, a value of the wrong type, a file or directory that does not exist.
- **Local files that tasks read and that do not exist**, and that no task makes, for every task of the plan, not only the first one that a run would reach.
- **Tasks without a container image**, which a real run cannot start unless `-default-image` gives one (decision 11).

The check plans the run with the simulator, as `iunctus graph` does, so that the plan goes past the files that are missing. It keeps the made-up files in a scratch folder that it removes, and it records nothing in the run database. What the workflow prints goes to standard error; the report goes to standard output, the worst problems first, then what the check could not look at:

```text
error: step COPY reads data/samples.csv, which does not exist
error: step COPY: 1 task names no container image; iunctus runs every task in a container, so name an image in the workflow or give one with `-default-image <image>`
main.nf (Nextflow): 2 errors would stop a real run.
```

```text
unsupported: main.nf:1:1: DSL1 scripts are not supported (decision 14): this script has processes that read `from` channels and write `into` channels, which only DSL1 has, and Nextflow 22.12 and later do not run it either
main.nf (Nextflow): 1 unsupported feature would stop a real run.
```

```text
main.nf (Nextflow): nothing would stop a real run of 2 tasks in 2 steps.
```

The exit code is 33 when a feature is not supported, else 1 when something would stop a real run, else 0; warnings do not change it. A config file with a mistake stops the check at once with exit code 3, as it stops every command; `iunctus check-machine` names the mistake together with the rest of the machine. A missing file is named once, with every step that reads it; many missing files that the same steps read are counted.

The plan is the one of `iunctus graph`: a Snakemake workflow that imports a Python package without a stand-in starts its Python container while it plans (decision 19), as a simulated run does.

**Limits.** A plan with made-up files can differ from a real run where real data decides which tasks follow (the markers of [the graphs of a run](graph.md), section 3); the report says how many such points the plan has, since it cannot see the problems of tasks that only real data makes. Files at remote addresses are not fetched, and the report counts them. A load error stops the plan, so the problems of the tasks after it show once it is fixed. Snakemake, CWL and Argo report the first load error of their loader only.

The code: `iunctus/src/check.rs`; tests in `iunctus/tests/check.rs`.

## 4. Checking the machine

```sh
iunctus check-machine [-executor <executor>] [-store <folder or address>] [-iunctus-config <file>] [the options of the executors]
```

`iunctus check-machine` says what is ready on this machine for real runs, and what is missing, with what to install or set. It looks at:

| Part | Ready when | What it says when it is missing |
|---|---|---|
| config file | every config file is valid; it names the files and their storage providers | the file, the line and the mistake |
| secret | the environment variable of each `{ environment = "NAME" }` of the config file is set | the file, the line and the variable to set |
| container engine | Docker or Podman answers at its socket; it gives its version | the sockets it tried, and how to start Docker or turn on the socket of Podman (`systemctl --user enable --now podman.socket`); a socket that the user may not use gets `sudo usermod -aG docker $USER`; with `-executor tes`, `slurm` or `kubernetes` it is `not set up` instead, as only Snakefiles that import Python packages without a stand-in need it then |
| store | the folder can be written, with its free space, or the store at an address answers | why not, and where to name another store; less than 1 GB free counts as missing |
| cache folder | where tasks run and downloads go, with its free space, when it is on another file system than the store | as for the store |
| run database | it opens; it counts the runs | why it cannot open, and `-database` |
| executor tes | the TES server of `-tes-server` or `[executor.tes]` answers | the server and why it does not answer, or that it refuses the token |
| executor slurm | `sbatch`, `squeue`, `sacct` and `scancel` are on the PATH, and the container runtime of `-slurm-container` too; it names the scratch folder and the store, which the nodes must see at the same paths | the tools that are missing, and to run iunctus on a node of the cluster; or the runtime that is missing |
| executor kubernetes | the kubeconfig of `-kubeconfig` or `[executor.kubernetes]` is readable; it names the namespace of the Jobs | the file and why it cannot be read |

An executor that neither the config file nor the command line sets up is `not set up`, which is no problem; only the SLURM tools are looked for anyway, as that costs nothing. The check makes no request outside this machine, except to the servers that the config file or the command line names, each once: the store at an address and the TES server. It never asks a Kubernetes cluster, and it reads no kubeconfig that is not named.

```text
not set up config file: none, so public files need no provider and the store is a folder in the cache; /home/me/.config/iunctus/config.toml can name storage providers and executors
ready      container engine: Docker 29.8.1 at /var/run/docker.sock
ready      store: /home/me/.cache/iunctus/store, 23.2 GB free
ready      run database: /home/me/.cache/iunctus/runs.sqlite, 54 runs
not set up executor tes: no TES server named; name one with -tes-server or `server` in [executor.tes]
not set up executor slurm: the tools of SLURM are not on the PATH; -executor slurm runs on a node of a SLURM cluster
not set up executor kubernetes: no kubeconfig named; name one with -kubeconfig or `kubeconfig` in [executor.kubernetes]; iunctus never takes the default one
This machine is ready for real runs.
```

```text
ready      config file: lab.toml; no storage providers
missing    secret: lab.toml:3: the environment variable TES_TOKEN, which holds this secret, is not set; set it before a run
missing    container engine: no container engine answers at /var/run/docker.sock, /run/user/1000/docker.sock, /run/user/1000/podman/podman.sock or /run/podman/podman.sock; iunctus runs every task in a container, so start Docker, or turn on the socket of Podman with `systemctl --user enable --now podman.socket`, or name a socket with IUNCTUS_ENGINE
ready      store: /home/me/.cache/iunctus/store, 23.2 GB free
ready      run database: /home/me/.cache/iunctus/runs.sqlite, 0 runs
missing    executor tes: the TES server at http://127.0.0.1:8000 does not answer: Connection refused; start it, or name another with -tes-server or `server` in [executor.tes] of the config file
3 things are missing for real runs; the lines that start with `missing` say what to do.
```

The exit code is 3 when something is missing, else 0. A real run makes the same checks for what it needs before its first task: the config file and the store for every command, the container engine for `-executor local`, one request to the TES server for `-executor tes`, the SLURM tools for `-executor slurm`, and the kubeconfig for `-executor kubernetes`. Each stops the run with exit code 3 and the same words.

The code: `iunctus/src/machine.rs`; tests in `iunctus/tests/machine.rs`.
