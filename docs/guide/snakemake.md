<!-- Running Snakemake rules with profiles, target controls and container environments. -->
<!-- Copyright (c) 2026 The iunctus Contributors -->
<!-- SPDX-License-Identifier: MIT or Apache-2.0 -->

# Snakemake

Give `run` a Snakefile, a `.smk` file, a project folder or a repository.
Repositories can use `workflow/Snakefile`.
From the iunctus checkout, this small example uses a Debian container:

```sh
iunctus run demo/snakemake/hello.smk
cat demo/snakemake/hello/all.txt
```

The file contains `Hello world!`, `Hola world!`, `Bonjour world!` and `Ciao world!`,
in that order. Existing current outputs can make a later run do no rule work.
Use a separate copy for simulations, which create made-up files.

## Profiles and configuration

`-profile NAME` finds a profile folder in the working folder or under `profiles/`
there or beside the Snakefile. An explicit profile path or YAML file also works.
Profiles use `profile.yaml` or `config.yaml`. A workflow's `profiles/default`
is discovered automatically. Later selected profiles replace supplied settings.

Profiles support `configfile`, `config`, `default-resources`, `set-resources`,
`set-threads`, `retries` (also `restart-times`) and `rerun-triggers`.
Display settings and settings that only choose how jobs run are ignored.
For example, profile `cores`, `jobs`, `executor` and `use-conda` do not select
iunctus resources or its executor. Other unsupported settings fail with code 33.

`-configfile config.yaml` loads workflow config. `--config name=value` sets
workflow values. Engine flags normally use one hyphen.

## Select and rerun jobs

Give rule names or concrete output paths after the workflow to select targets.
The following options accept comma-separated names or repeated options:

| Option | Effect |
| --- | --- |
| `-until rules,files` | Keep selected jobs and their dependencies. Drop downstream and independent jobs. |
| `-omit-from rules,files` | Drop selected jobs and their downstream jobs. Keep their dependencies and independent branches. |
| `-forcerun rules,files` | Force selected jobs and their downstream jobs. Run dependencies when needed. |
| `-forceall` | Force every job needed by the selected targets, within the selection limits. |

Forced jobs bypass task-cache and resume reuse. Independent jobs keep normal reuse.
Rules with wildcards select matching jobs already in the target graph.
Rules without wildcards and files given to `-until` or `-forcerun` also add targets.
Files given to `-omit-from` do not add targets.
These controls are refused for other languages.

```sh
iunctus run demo/snakemake/hello.smk -until collect -executor simulator
iunctus run demo/snakemake/hello.smk -omit-from collect -executor simulator
iunctus run demo/snakemake/hello.smk -forcerun collect -executor simulator
iunctus run demo/snakemake/hello.smk -forceall -executor simulator
```

## Choose freshness checks

`-rerun-triggers mtime,params,input,software-env,code` is the default set.
Choose a subset, such as `-rerun-triggers mtime`, to override the profile.
The checks compare input times, evaluated params, input paths, software references
and supported rule or script source changes. Missing outputs, failed attempts,
forced jobs and changed upstream jobs still require execution.
The shared task cache still includes software and input content.

## Conda and pip

Configure [the Conda target](configuration.md#conda) to prepare local `conda:`
rules as reusable images. No host Conda or pip installation is needed.
Use a pinned base, its platform and an explicit file describing its capabilities.
The base for Conda-only rules must contain micromamba.
Online mode fetches metadata and packages. Offline mode needs cached metadata and
package bytes for a cold build. Builds install packages inside containers.

Environment YAML can include `pip:` requirements. Named packages support version
constraints, extras and target markers. The native resolver pins compatible
artifacts and their hashes. Editable installs, local paths, direct URLs, VCS
requirements, custom indexes and dynamic source metadata are refused.
Source builds need their build requirements in the locked Conda environment.
See [the environment guide](../design/conda.md) for the full limits.

With both `conda:` and `container:`, configured preparation builds the environment
on the rule image. Its manifest digest must match the configured base.
A pinned micromamba installer is added during this build.
Without `[conda]`, a combined rule uses its container alone.
Remote executors require `[conda] registry` and a local engine to build and push
the image. Workers receive the published manifest digest and need pull access.

## Groups, shadow and hooks

Same-image connected jobs with the same `group:` run in one container, in
dependency order. The whole group retries on failure. Different-image groups
fall back to separate tasks and emit a debug log. Supported `service()` groups
retain dependency order. Same-image FIFO `pipe()` jobs run concurrently in one
container; a failure or retry affects the whole group. Several consumers,
cycles, different images and `run:` pipe jobs are refused.

`shadow: "minimal"` stages declared inputs and task code. `"shallow"` links
top-level working-folder entries. `"full"` creates scratch folders and links
files recursively. Shallow outputs under an existing directory link are refused;
use full shadow for that layout. Outputs must stay inside task scratch.
`copy-minimal` is unsupported.

`onstart`, `onsuccess` and `onerror` run supported native Python.
Their shell and supported subprocess commands run uncached in a local container.
They use the global container or default image and mount the workflow folder
writable. Hooks stay local even with a remote rule executor.
Simulation skips hook commands. Host-only programs are unavailable.

## Replace engine flags

| Rejected Snakemake flag | iunctus equivalent |
| --- | --- |
| `--cores`, `--jobs`, `-j` | `-max-pending N` limits concurrent tasks. Rule threads define task CPUs. |
| `--dry-run`, `-n` | `-executor simulator` |
| `--profile`, `--workflow-profile` | `-profile NAME` or `-profile PATH` |
| `--snakefile` | Give the Snakefile as the workflow argument. |
| `--directory` | Start iunctus in the desired working folder. |
| `--use-conda` | Configure `[conda]`. |
| `--use-singularity`, `--use-apptainer` | `-executor slurm -slurm-container apptainer` |
| `--until`, `--omit-from`, `--forcerun`, `--forceall` | Corresponding single-hyphen options above. |
| `-R`, `-F` | `-forcerun` and `-forceall` |
| `--rerun-triggers` | `-rerun-triggers` with comma-separated values. |

Read [support boundaries](../support.md#workflow-languages) for remaining limits.
Next: [Executors](executors.md). Return to [First run](first-run.md).
