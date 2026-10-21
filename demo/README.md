<!-- Running the uwe demo on a laptop and on the BCF cluster. -->
<!-- Copyright (c) 2026 The uwe Contributors -->
<!-- SPDX-License-Identifier: MIT or Apache-2.0 -->

# The uwe demo on the BCF cluster and on a laptop

## TL;DR

On the BCF cluster (binary and config are already in place):

```sh
ssh worm.intra                       # from outside: ssh -J uni-jump worm.intra
export PATH=$HOME/.local/bin:$PATH
cd /vol/cb/projects/uwe/no_backup/demo/nextflow/hello
uwe run main.nf                      # each task is a SLURM job with Apptainer, unless it ran before
uwe run main.nf                      # again: all tasks come from the cache
```

The same works in the other folders: `uwe run Snakefile` in `snakemake/tutorial`,
`uwe run scatter-workflow.cwl scatter-job.yml` in `cwl/workflows`, `uwe run
dag-diamond.yaml` in `argo/examples`. Each run writes `report.html`; copy it to your computer
and open it in a browser:

```sh
scp worm.intra:/vol/cb/projects/uwe/no_backup/demo/nextflow/hello/report.html .
```

On a laptop with Docker:

```sh
source demo/local-env.sh             # builds uwe, goes into demo/, prints the commands
uwe run nextflow/main.nf             # every task is a Docker container; writes report.html
```

`demo/local-env.sh` changes only the shell it is sourced in: it puts `target/release` first
on the PATH and makes [`local-config.toml`](local-config.toml) the config through
`UWE_SITE_CONFIG`.

The rest of this file explains each example, how it compares with the native tool, and what
was changed.

The official examples of the four languages that uwe runs, each run with plain
`uwe run <file>`. On the BCF cluster every task is a SLURM job with its own Apptainer
container. On a laptop or workstation every task is a Docker container: see
[On a laptop with Docker](#on-a-laptop-with-docker).

| Folder | Example | Source |
|---|---|---|
| File | Example | Source |
|---|---|---|
| `nextflow/main.nf` | `nextflow run hello`, the first example of nextflow.io | [nextflow-io/hello](https://github.com/nextflow-io/hello) 3c2cdc98 |
| `nextflow/nf-core-demo/` (downloaded) | nf-core/demo 1.2.0 with its test profile | [nf-core/demo](https://github.com/nf-core/demo) 1.2.0, 32893afe |
| `snakemake/hello.smk` | hello world, written for this demo | this repository |
| `snakemake/Snakefile` | the workflow of the Snakemake tutorial (basics, step 7 and summary); its `data/` is downloaded | [snakemake/snakemake](https://github.com/snakemake/snakemake) docs/tutorial/basics.rst at 5bf5e21b; data [snakemake/snakemake-tutorial-data](https://github.com/snakemake/snakemake-tutorial-data) b21b0248 |
| `cwl/hello_world.cwl` | "Hello World" of the CWL user guide's quick start | [common-workflow-language/user_guide](https://github.com/common-workflow-language/user_guide) 96a5e947, `src/_includes/cwl/` |
| `cwl/scatter-workflow.cwl` | the first scatter workflow of the user guide | the same, `src/_includes/cwl/workflows/` |
| `argo/hello-world.yaml`, `argo/dag-diamond.yaml` | two of Argo's examples | [argoproj/argo-workflows](https://github.com/argoproj/argo-workflows) v4.1.4, b5b4d665, `examples/` |

The files are copies of the originals. The few changes are listed below, with the reason.

The copy on the cluster, `/vol/cb/projects/uwe/no_backup/demo`, still has the earlier
folders: `nextflow/hello`, `nextflow/nf-core-demo`, `snakemake/tutorial`, `cwl/quick-start`,
`cwl/workflows` and `argo/examples`. The commands for the cluster below use them.

## Where to run it

Run it on `worm.intra`, not on the login node `cli.intra`: from the BCF network
`ssh worm.intra`, from outside `ssh -J uni-jump worm.intra`. uwe itself runs there and
only waits; every task goes to SLURM with `sbatch`.

## Once: the binary and the config file

The binary is `/vol/cb/projects/uwe/no_backup/bin/uwe`, and
`~/.local/bin/uwe` points to it. `~/.local/bin` is not on the PATH of a login shell on
worm.intra, so add it once (or add this line to `~/.bashrc`):

```sh
export PATH=$HOME/.local/bin:$PATH
```

The config file `~/.config/uwe/config.toml` is a copy of [`bcf-config.toml`](bcf-config.toml):

```sh
mkdir -p ~/.config/uwe
cp /vol/cb/projects/uwe/no_backup/demo/bcf-config.toml ~/.config/uwe/config.toml
```

It says: run tasks on SLURM with Apptainer, leave out the node `rs700-bcf-3` (it does not
see `/vol`), keep the cache under `/vol/cb/projects/uwe/no_backup/cache`, and reach the
internet through `proxy.intra:3128` (the nodes cannot reach the image registries without it).
With it, `uwe run` needs no options. Check the setup with `uwe check-machine`.
It also makes every run write its report, `report.html`, into the folder it ran in (see
[The report](#the-report)).

## Run the examples

All folders are under `/vol/cb/projects/uwe/no_backup/demo`. The options after the file
are the ones that the example itself documents.

```sh
cd /vol/cb/projects/uwe/no_backup/demo

cd nextflow/hello && uwe run main.nf; cd -
cd nextflow/nf-core-demo && uwe run . -profile test,apptainer --outdir results; cd -
cd snakemake/tutorial && uwe run Snakefile; cd -
cd cwl/quick-start && uwe run hello_world.cwl && uwe run hello_world.cwl hello_world-job.json; cd -
cd cwl/workflows && uwe run scatter-workflow.cwl scatter-job.yml; cd -
cd argo/examples && for f in hello-world dag-diamond artifact-passing loops; do uwe run $f.yaml; done; cd -
```

What each one shows:

- **hello**: four tasks of one process, run from a channel; `view` prints `Bonjour world!`
  and the others. The image `quay.io/nextflow/bash` comes from its `nextflow.config`.
- **nf-core/demo**: a real nf-core pipeline: test data from the web, FastQC, seqtk, cowpy and
  MultiQC, with their results in `results/`. nf-core documents
  `-profile test,<docker/singularity/.../institute> --outdir <OUTDIR>`; with uwe on
  SLURM, `-profile test` alone works too.
- **Snakemake tutorial**: bwa, samtools and bcftools on three small read files, then a Python
  script that plots the quality of the variant calls: `calls/all.vcf` and `plots/quals.svg`.
- **CWL hello world**: one tool; with the job file, it says `こんにちは世界`.
- **CWL scatter**: the same tool once per message of `scatter-job.yml`, four tasks at once.
- **Argo**: one container; a diamond of four tasks; a file passed from one step to the next
  (`results/.../hello-art`); a loop over two items.

The text that tools print goes to the logs of their tasks: `uwe page <run>` shows them.

A second run takes about a second: every task comes from the cache. Two exceptions, as in
the native tools:

- Snakemake sees that its results are there and does nothing. Remove them to see them come
  from the cache: `rm -r mapped_reads sorted_reads calls plots`.
- nf-core/demo takes its tasks from the cache with `-resume`, as Nextflow does. The images of
  nf-core modules are SIF files at web addresses, which uwe cannot pin to a digest, so
  only a resumed run reuses their results. MultiQC runs again even then: its input holds the
  time of the run.

## Times on worm.intra

| Example | First run | Second run |
|---|---|---|
| hello | 21 s (with the image pull) | 0.6 s, 4 of 4 from the cache |
| nf-core/demo | 1 min 54 s | 18 s with `-resume`, 7 of 8 from the cache |
| Snakemake tutorial | 3 min 10 s (with 3 image pulls, about 470 MB) | 1.1 s, 8 of 8 from the cache |
| CWL hello world | 2.8 s, with the job file 5.6 s | 0.7 s |
| CWL scatter | 4.8 s | 0.8 s |
| Argo | 3 to 18 s each | 0.7 to 1.1 s |

## What differs from the published examples, and why

- **Snakemake: one `container:` line per rule** (10 lines, see the history of the Snakefile).
  The tutorial installs bwa, samtools, bcftools, matplotlib and pysam with Conda
  (`environment.yaml`). uwe runs every task in a container and cannot build an image from
  Conda yet. So each rule names a published BioContainers image: bwa 0.7.17 with samtools
  1.19.2, bcftools 1.19, and matplotlib 3.1.2 with pysam 0.15. The last is the only published
  image with both; its pysam is older than the tutorial's 0.22, which the script does not
  notice. Snakemake itself uses the same lines with `--sdm apptainer`.
- **Snakemake: the index files are inputs, and `workdir: workflow.basedir`.** bwa reads
  index files next to `data/genome.fa` (`genome.fa.bwt` and others), and bcftools reads
  `genome.fa.fai`; the tutorial declares neither. Natively, a job sees the whole folder.
  uwe gives a job the files that its rule declares and the files that git tracks
  (decision 11); the downloaded `data/` is not tracked. So `bwa_map` and `bcftools_call`
  declare these files. The `workdir:` line lets `uwe run snakemake/Snakefile` work from
  `demo/`, as Snakemake otherwise looks for `data/` in the folder it starts in. On the
  cluster, the folder is a git checkout of the data instead, where the index files are
  tracked.
- **CWL: `scatter-two-steps.cwl` is left out.** As published, it asks
  `hello_world_to_stdout.cwl` for an output `echo_out` that the tool does not have (the
  contents of the two `hello_world` files in the guide's `workflows` folder are swapped).
  cwltool rejects it the same way. The first scatter example, `scatter-workflow.cwl`, is used.
- **CWL: one `hello_world.cwl`.** The guide's `workflows` folder has its own `hello_world.cwl`
  with an output that the scatter workflow does not use. Here the scatter workflow runs the
  quick start's `hello_world.cwl`, so that `cwl/` holds one file of that name.
- **CWL: `hello_world.cwl` names no image.** cwltool runs it on the host; uwe runs every
  task in a container, here `debian:stable-slim`.
- **Nextflow hello: the image is used.** `nextflow run hello` without `-with-docker` or
  `-with-apptainer` ignores `process.container`; uwe always runs it.
- **nf-core/demo is not in this repository**, as it is a whole pipeline of about 50 files.
  uwe cannot fetch a pipeline by its name yet, so `local-env.sh` clones it, and on the
  cluster it was cloned by hand:
  `git clone --branch 1.2.0 https://github.com/nf-core/demo nf-core-demo`.

## Compared with the native tools

| Example | Native tool | Result |
|---|---|---|
| hello | Nextflow 26.04.6 in a SLURM job | the same four lines; their order varies from run to run in both |
| nf-core/demo | Nextflow 26.04.6, `-profile test,apptainer`, in a SLURM job | the same 76 files: 41 with the same digest (trimmed reads, FastQC reports, cowpy); 30 MultiQC files differ only by work paths, dates and random ids in the SVG files; 5 `pipeline_info` files, named by the date, compared by name |
| Snakemake tutorial | Snakemake 9.27.0 on a laptop, the same images through Docker | `mapped_reads/` and `sorted_reads/` (6 files) have the same digests; `calls/all.vcf` differs only in the date that bcftools writes into its header; `plots/quals.svg` by name only (that Snakemake has no matplotlib) |
| CWL | cwltool 3.3.20260925135507 | the same output objects (`{}`) and the same text: `Hello World`, `こんにちは世界` |
| Argo | `argo lint --offline` 4.1.4 (no Kubernetes here) | all four valid; the output is what Argo's docs show: `hello world`; `A` to `D`; `hello world` passed on; `hello world` and `goodbye world` |

## The report

The cluster has no browser, and its page cannot be reached without a tunnel. So uwe can
write the report of a run: one HTML file that shows what the page shows, and needs no server
and no network. Copy it to your computer and open it there:

```sh
scp worm.intra:/vol/cb/projects/uwe/no_backup/demo/nextflow/hello/report.html .
```

It shows the summary of the run (workflow, language, executor, start, end, time, how many
tasks ran, came from the cache or failed), the graph with the state of each task (drag to
move, Ctrl and the wheel to zoom), where the time went, a timeline of the tasks, the CPU time,
memory and I/O of each task, where the results went, the steps and the tasks. A click on a task
shows its command, script, exit code, inputs and outputs, and the last 4 KB of its standard
output and error. The reports of the demo are 135 to 255 KB.

Three ways to get it:

- `[report] file = "report.html"` in the config file: every `uwe run` writes it when the
  run ends, into the folder it ran in (`bcf-config.toml` and `local-config.toml` do this). A
  relative path starts in the folder of the run; each run replaces the file of the run before.
- `uwe run <workflow> -report <file>`: the same for one run; it wins over the config file.
- `uwe report <run> [-output <file>]`: the report of any past run, by default
  `<run>-report.html`. `uwe runs` lists the runs.

The summary at the end of a run names the file, like `Report: report.html`.

## On a laptop with Docker

The same examples run on a laptop or workstation with Docker (or Podman). Each task is a
container on that machine, and nothing else is needed: no SLURM, no Apptainer, no Nextflow,
Snakemake or cwltool.

### Once: the binary and the config file

Run the examples in this folder, `demo/`. What they write is in [`.gitignore`](.gitignore), so
nothing lands in the repository. [`local-env.sh`](local-env.sh) builds uwe, puts
`target/release` first on the PATH, makes [`local-config.toml`](local-config.toml) the config
and goes into `demo/`:

```sh
source demo/local-env.sh             # or, in demo/: ./local-env.sh, which opens a new shell
```

`local-config.toml` runs tasks on this machine (`[executor] default = "local"`) and writes
`report.html` after each run. Without it, `uwe run` still uses local Docker or Podman containers.
Use `-executor simulator` for made-up outputs without task containers.

The first time, `local-env.sh` also downloads what is not in this repository: nf-core/demo
into `nextflow/nf-core-demo/` and the data of the Snakemake tutorial into `snakemake/data/`.

### Run the examples

In `demo/`, a simple and a complex example of each language:

```sh
uwe run nextflow/main.nf
uwe run snakemake/hello.smk
uwe run cwl/hello_world.cwl cwl/hello_world-job.json
uwe run argo/hello-world.yaml

uwe run nextflow/nf-core-demo -profile test,docker --outdir results
uwe run snakemake/Snakefile
uwe run cwl/scatter-workflow.cwl cwl/scatter-job.yml
uwe run argo/dag-diamond.yaml
```

Each run writes `report.html` into the folder it ran in, so `demo/` has that of the last run
there; `uwe report <run>` writes any other. `uwe page <run>` serves the live page on
this machine.

The second run of each comes from the cache, as on the cluster. Snakemake again does nothing
while its results are there: `rm -r snakemake/{mapped_reads,sorted_reads,calls,plots}` first.
nf-core/demo takes 7 of its 8 tasks from the cache without `-resume` here: Docker images are
pinned to their digests. MultiQC runs again, as its input holds the time of the run.

### Times on a laptop

Measured on 2026-10-01 on the machine of the developer (32 cores, Docker 29), with an empty
cache folder and the images already pulled. The images that were not there yet were
`quay.io/nextflow/bash` (30 MB to download), `busybox:latest` (2 MB) and the matplotlib and
pysam image of the Snakemake tutorial (361 MB); bwa, samtools, bcftools and the images of
nf-core/demo were there from earlier work.

| Example | First run | Second run |
|---|---|---|
| hello | 0.5 s | 0.1 s, 4 of 4 from the cache |
| nf-core/demo | 20 s (with the download of its test data) | 10 s, 7 of 8 from the cache (MultiQC runs) |
| Snakemake tutorial | 5.6 s | 0.1 s, 8 of 9 from the cache after `rm -r` of the results (the rule `all` counts as a task that ran) |
| CWL hello world | 0.2 s, with the job file 0.2 s | 0.1 s |
| CWL scatter | 0.2 s | 0.1 s |
| Argo | 0.2 to 1.5 s each | 0.1 s |

## More to look at

- **The runs.** `uwe runs` lists the runs, with their state, executor and number of
  tasks. `uwe time <run>` shows where the time went. `uwe why <run> <task>` tells why
  a task ran instead of coming from the cache.
- **The page.** `uwe page <run> -port 8080` serves the page of a run: its graph, its tasks,
  their logs and files. It listens on worm.intra only, so open a tunnel from your computer,
  `ssh -L 8080:localhost:8080 worm.intra` (from outside with `-J uni-jump`), then open
  `http://localhost:8080`.
- **The graph.** `uwe graph main.nf -format svg > graph.svg` draws the graph of the
  tasks. It simulates the run, so it needs no cluster and changes no results.

## Licenses

The examples keep their licenses: nextflow-io/hello and Argo Workflows under the Apache
License 2.0 (`LICENSE` in their folders), the Snakemake tutorial under the MIT license of
Snakemake (`snakemake/LICENSE.md`), the CWL user guide under CC BY 4.0 with its
example programs under the Apache License 2.0 (`cwl/LICENSE.md`; Copyright the Common
Workflow Language project, https://www.commonwl.org/). nf-core/demo is under the MIT license.

## `demo/bcf-config.toml`

The config file of uwe for the BCF cluster. Copy it to ~/.config/uwe/config.toml.
Then `uwe run <workflow>` runs each task as a SLURM job with Apptainer, with no options.
Options on the command line still win, like `-executor local`.
Run real tasks on SLURM. Without this, `uwe run` uses local Docker or Podman containers. `uwe graph`
simulates by default.

## `demo/local-config.toml`

The config file of uwe for a laptop or workstation with Docker. Pass it to a command with
`-uwe-config demo/local-config.toml`, or copy it to ~/.config/uwe/config.toml.
Then `uwe run <workflow>` runs each task in a Docker container on this machine.
Options on the command line still win, like `-executor simulator`.
Run real tasks with Docker (or Podman) on this machine. Local execution is also the built-in default. `uwe graph` simulates by default.

## `demo/local-env.sh`

Sets up this shell for the demo on a laptop or workstation with Docker or Podman.
Source it, from bash or zsh, anywhere:

  source demo/local-env.sh

Or run it, `./local-env.sh` in demo/: then it opens a new shell (your $SHELL, zsh or bash)
with the same set-up; `exit` goes back.

It builds the release binary of uwe (cargo does nothing when it is up to date), puts it
first on the PATH, makes demo/local-config.toml the config of uwe for this shell only
and goes into demo/. Then it prints the commands of the demo, to run from there: tasks run
in containers on this machine, and each run writes report.html into demo/.
A config file in ~/.config/uwe/ still wins over it. Nothing outside this shell changes.
