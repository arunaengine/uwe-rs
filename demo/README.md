# The iunctus demo on the BCF cluster

Four small workflows that do the same thing, one in each language that iunctus runs:
Nextflow, Snakemake, CWL and Argo Workflows. Each one greets three names. Each greeting is
made by its own task and shouted by another, and each task is a SLURM job with its own
container. A last task, with Python, joins the greetings into one report. So each run makes
seven tasks (Snakemake adds its rule `all`), with a fan-out and a join.

| Folder | Command | Results |
|---|---|---|
| `nextflow/` | `iunctus run main.nf` | `results/report.txt`, `results/greetings/` |
| `snakemake/` | `iunctus run Snakefile` | `results/report.txt`, `results/greetings/`, `results/loud/` |
| `cwl/` | `iunctus run workflow.cwl` | `report.txt` and the greetings, in the folder of the run |
| `argo/` | `iunctus run workflow.yaml` | `results/<step>/report` |

The images are small: `debian:stable-slim` and `python:3.13-slim`. Each report says which
node greeted and which node shouted, so you can see that the tasks ran as SLURM jobs on
different nodes.

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
see `/vol`), keep the cache under `/vol/cb/projects/iunctus/no_backup/cache`, and ask the image
registries through `proxy.intra:3128`. With it, `iunctus run <file>` needs no options. Check
the setup with `iunctus check-machine`.

## Run the demos

```sh
cd /vol/cb/projects/iunctus/no_backup/demo/nextflow
iunctus run main.nf
cat results/report.txt
```

The same for the others: `cd ../snakemake; iunctus run Snakefile`,
`cd ../cwl; iunctus run workflow.cwl`, `cd ../argo; iunctus run workflow.yaml`.

While a run goes, iunctus shows it as `nextflow run` does, the same for every language: one
line per step, with the task that last started or ended and how many of the step's tasks are
done, drawn again in place. At the end it says how long the run took, how many tasks ran or
came from the cache, where the results went, and where the files of every task are. A first
run takes 15 to 30 seconds, when SLURM has free slots:

```text
iunctus 0.1.0: running main.nf (Nextflow) on slurm, run 1a0f6e5836d-f0a73
The iunctus demo in Nextflow: 3 greetings

HELLO, ADA!    greeted on rs500-bcf-01.intra, shouted on rs500-bcf-01.intra
HELLO, GRACE!  greeted on rs500-bcf-01.intra, shouted on rs500-bcf-01.intra
HELLO, LINUS!  greeted on rs500-bcf-01.intra, shouted on rs500-bcf-01.intra

executor >  slurm (7 tasks, 0 running, 18s)
[2] GREET (2) | 3 of 3 ✔
[6] SHOUT (3) | 3 of 3 ✔
[7] REPORT    | 1 of 1 ✔

Done in 18s: 7 tasks, 7 ran, 0 from the cache, 0 failed
Results:
  results/greetings/ Ada.txt, Grace.txt, Linus.txt
  results/ report.txt
Files of every task: /vol/cb/projects/iunctus/no_backup/cache/store
More: `iunctus runs`, `iunctus page 1a0f6e5836d-f0a73`, `iunctus time 1a0f6e5836d-f0a73`
```

The lines between the first line and the steps are what the workflow prints itself, here
Nextflow's `view` of the report. When the output goes to a file instead of a terminal, each
task prints a line when it ends, like `[done] GREET (1)`, and the steps come at the end.

## What to look at

- **The cache.** Run the same command again: every task comes from the cache, in a second or
  two, and the results are delivered again. Snakemake sees that its results are there and
  does nothing; remove `results/` first (`rm -r results`) to see them come from the cache.
  Change a name in a workflow, and only the tasks of that name run again.
- **The runs.** `iunctus runs` lists the runs, with their state, executor and number of
  tasks. `iunctus time <run>` shows where the time went.
- **The page.** `iunctus page <run> -port 8080` serves the page of a run: its graph, its tasks,
  their logs and files. It listens on worm.intra only, so open a tunnel from your computer,
  `ssh -L 8080:localhost:8080 worm.intra` (from outside with `-J uni-jump`), then open
  `http://localhost:8080`. `iunctus run main.nf -page` serves the page while the run goes.
- **The graph.** `iunctus graph main.nf -format svg > graph.svg` draws the graph of the
  tasks. It simulates the run, so it needs no cluster and changes no results.
- **The same commands for every language**: `iunctus run`, `graph`, `runs`, `page` and
  `time` work the same for `main.nf`, `Snakefile`, `workflow.cwl` and `workflow.yaml`.
