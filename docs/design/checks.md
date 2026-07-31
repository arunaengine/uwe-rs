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
iunctus: no container engine answers at /var/run/docker.sock or /run/user/1000/podman/podman.sock; iunctus runs every task in a container, so start Docker, or turn on the socket of Podman with `systemctl --user enable --now podman.socket`, or name a socket with IUNCTUS_ENGINE
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
| 1 | The workflow failed or cannot run | a syntax error, a missing include or input file, inputs that do not fit the schema, a task that failed, a result that cannot be delivered |
| 2 | The command line is wrong | an unknown command or option, an option without its value, an option of another command, no workflow named |
| 3 | This machine or its config is not ready | a config file with a mistake, a secret whose environment variable is not set, no container engine, a store or run database that cannot be opened, a TES server that does not answer, SLURM tools that are missing, a cluster that failed a task again and again |
| 33 | The workflow uses a feature that iunctus does not support | a DSL1 Nextflow script (decision 14), an Argo resource template for Kubernetes objects (decision 18), an address that no storage provider serves |
| 130 | The run was stopped with Ctrl-C | |

33 is the code that CWL runners give for unsupported requirements. A run that fails for several reasons gives the code of the first one it meets.
