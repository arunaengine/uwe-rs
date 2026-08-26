# The iunctus demo on the BCF cluster

The official examples of the four languages that iunctus runs, each run with plain
`iunctus run <file>`. Every task is a SLURM job with its own Apptainer container.

| Folder | Example | Source |
|---|---|---|
| `nextflow/hello/` | `nextflow run hello`, the first example of nextflow.io | [nextflow-io/hello](https://github.com/nextflow-io/hello) 3c2cdc98 |
| `nextflow/nf-core-demo/` (cluster only) | nf-core/demo 1.2.0 with its test profile | [nf-core/demo](https://github.com/nf-core/demo) 1.2.0, 32893afe |
| `snakemake/tutorial/` | the workflow of the Snakemake tutorial (basics, step 7 and summary) | [snakemake/snakemake](https://github.com/snakemake/snakemake) docs/tutorial/basics.rst at 5bf5e21b; data [snakemake/snakemake-tutorial-data](https://github.com/snakemake/snakemake-tutorial-data) b21b0248 |
| `cwl/quick-start/` | "Hello World" of the CWL user guide's quick start | [common-workflow-language/user_guide](https://github.com/common-workflow-language/user_guide) 96a5e947, `src/_includes/cwl/` |
| `cwl/workflows/` | the first scatter workflow of the user guide | the same, `src/_includes/cwl/workflows/` |
| `argo/examples/` | `hello-world`, `dag-diamond`, `artifact-passing` and `loops` | [argoproj/argo-workflows](https://github.com/argoproj/argo-workflows) v4.1.4, b5b4d665, `examples/` |

The files are copies of the originals. The few changes are listed below, with the reason.

## Where to run it

Run it on `worm.intra`, not on the login node `cli.intra`: from the BCF network
`ssh worm.intra`, from outside `ssh -J uni-jump worm.intra`. iunctus itself runs there and
only waits; every task goes to SLURM with `sbatch`.

## Once: the binary and the config file

The binary is `/vol/cb/projects/iunctus/no_backup/bin/iunctus`, and
`~/.local/bin/iunctus` points to it. `~/.local/bin` is not on the PATH of a login shell on
worm.intra, so add it once (or add this line to `~/.bashrc`):

```sh
export PATH=$HOME/.local/bin:$PATH
```

The config file `~/.config/iunctus/config.toml` is a copy of [`bcf-config.toml`](bcf-config.toml):

```sh
mkdir -p ~/.config/iunctus
cp /vol/cb/projects/iunctus/no_backup/demo/bcf-config.toml ~/.config/iunctus/config.toml
```

It says: run tasks on SLURM with Apptainer, leave out the node `rs700-bcf-3` (it does not
see `/vol`), keep the cache under `/vol/cb/projects/iunctus/no_backup/cache`, and reach the
internet through `proxy.intra:3128` (the nodes cannot reach the image registries without it).
With it, `iunctus run` needs no options. Check the setup with `iunctus check-machine`.

## Run the examples

All folders are under `/vol/cb/projects/iunctus/no_backup/demo`. The options after the file
are the ones that the example itself documents.

```sh
cd /vol/cb/projects/iunctus/no_backup/demo

cd nextflow/hello && iunctus run main.nf; cd -
cd nextflow/nf-core-demo && iunctus run . -profile test,apptainer --outdir results; cd -
cd snakemake/tutorial && iunctus run Snakefile; cd -
cd cwl/quick-start && iunctus run hello_world.cwl && iunctus run hello_world.cwl hello_world-job.json; cd -
cd cwl/workflows && iunctus run scatter-workflow.cwl scatter-job.yml; cd -
cd argo/examples && for f in hello-world dag-diamond artifact-passing loops; do iunctus run $f.yaml; done; cd -
```

What each one shows:

- **hello**: four tasks of one process, run from a channel; `view` prints `Bonjour world!`
  and the others. The image `quay.io/nextflow/bash` comes from its `nextflow.config`.
- **nf-core/demo**: a real nf-core pipeline: test data from the web, FastQC, seqtk, cowpy and
  MultiQC, with their results in `results/`. nf-core documents
  `-profile test,<docker/singularity/.../institute> --outdir <OUTDIR>`; with iunctus on
  SLURM, `-profile test` alone works too.
- **Snakemake tutorial**: bwa, samtools and bcftools on three small read files, then a Python
  script that plots the quality of the variant calls: `calls/all.vcf` and `plots/quals.svg`.
- **CWL hello world**: one tool; with the job file, it says `こんにちは世界`.
- **CWL scatter**: the same tool once per message of `scatter-job.yml`, four tasks at once.
- **Argo**: one container; a diamond of four tasks; a file passed from one step to the next
  (`results/.../hello-art`); a loop over two items.

The text that tools print goes to the logs of their tasks: `iunctus page <run>` shows them.

A second run takes about a second: every task comes from the cache. Two exceptions, as in
the native tools:

- Snakemake sees that its results are there and does nothing. Remove them to see them come
  from the cache: `rm -r mapped_reads sorted_reads calls plots`.
- nf-core/demo takes its tasks from the cache with `-resume`, as Nextflow does. The images of
  nf-core modules are SIF files at web addresses, which iunctus cannot pin to a digest, so
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
  (`environment.yaml`). iunctus runs every task in a container and cannot build an image from
  Conda yet. So each rule names a published BioContainers image: bwa 0.7.17 with samtools
  1.19.2, bcftools 1.19, and matplotlib 3.1.2 with pysam 0.15. The last is the only published
  image with both; its pysam is older than the tutorial's 0.22, which the script does not
  notice. Snakemake itself uses the same lines with `--sdm apptainer`.
- **Snakemake: the folder is a git checkout of the data.** bwa reads index files next to
  `data/genome.fa` (`genome.fa.bwt` and others) that no rule declares. Natively, a job sees
  the whole folder. iunctus gives a job the files that its rule declares and the files that
  Snakemake hands to jobs without a shared file system: those that git tracks (decision 11).
  In a checkout of the data, the index files are tracked. The tutorial's Gitpod setup is
  such a checkout. To set it up elsewhere:
  `git clone https://github.com/snakemake/snakemake-tutorial-data tutorial`, then copy
  `Snakefile` and `scripts/` of this folder into it.
- **CWL: `scatter-two-steps.cwl` is left out.** As published, it asks
  `hello_world_to_stdout.cwl` for an output `echo_out` that the tool does not have (the
  contents of the two `hello_world` files in the guide's `workflows` folder are swapped).
  cwltool rejects it the same way. The first scatter example, `scatter-workflow.cwl`, is used.
- **CWL: `hello_world.cwl` names no image.** cwltool runs it on the host; iunctus runs every
  task in a container, here `debian:stable-slim`.
- **Nextflow hello: the image is used.** `nextflow run hello` without `-with-docker` or
  `-with-apptainer` ignores `process.container`; iunctus always runs it.
- **nf-core/demo is not in this repository**, as it is a whole pipeline of about 50 files.
  iunctus cannot fetch a pipeline by its name yet, so it was cloned on the cluster:
  `git clone --branch 1.2.0 https://github.com/nf-core/demo nf-core-demo`.

## Compared with the native tools

| Example | Native tool | Result |
|---|---|---|
| hello | Nextflow 26.04.6 in a SLURM job | the same four lines; their order varies from run to run in both |
| nf-core/demo | Nextflow 26.04.6, `-profile test,apptainer`, in a SLURM job | the same 76 files: 41 with the same digest (trimmed reads, FastQC reports, cowpy); 30 MultiQC files differ only by work paths, dates and random ids in the SVG files; 5 `pipeline_info` files, named by the date, compared by name |
| Snakemake tutorial | Snakemake 9.27.0 on a laptop, the same images through Docker | `mapped_reads/` and `sorted_reads/` (6 files) have the same digests; `calls/all.vcf` differs only in the date that bcftools writes into its header; `plots/quals.svg` by name only (that Snakemake has no matplotlib) |
| CWL | cwltool 3.3.20260925135507 | the same output objects (`{}`) and the same text: `Hello World`, `こんにちは世界` |
| Argo | `argo lint --offline` 4.1.4 (no Kubernetes here) | all four valid; the output is what Argo's docs show: `hello world`; `A` to `D`; `hello world` passed on; `hello world` and `goodbye world` |

## More to look at

- **The runs.** `iunctus runs` lists the runs, with their state, executor and number of
  tasks. `iunctus time <run>` shows where the time went. `iunctus why <run> <task>` tells why
  a task ran instead of coming from the cache.
- **The page.** `iunctus page <run> -port 8080` serves the page of a run: its graph, its tasks,
  their logs and files. It listens on worm.intra only, so open a tunnel from your computer,
  `ssh -L 8080:localhost:8080 worm.intra` (from outside with `-J uni-jump`), then open
  `http://localhost:8080`.
- **The graph.** `iunctus graph main.nf -format svg > graph.svg` draws the graph of the
  tasks. It simulates the run, so it needs no cluster and changes no results.

## Licenses

The examples keep their licenses: nextflow-io/hello and Argo Workflows under the Apache
License 2.0 (`LICENSE` in their folders), the Snakemake tutorial under the MIT license of
Snakemake (`snakemake/tutorial/LICENSE.md`), the CWL user guide under CC BY 4.0 with its
example programs under the Apache License 2.0 (`cwl/LICENSE.md`; Copyright the Common
Workflow Language project, https://www.commonwl.org/). nf-core/demo is under the MIT license.
