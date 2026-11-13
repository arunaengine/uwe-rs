<!-- The batch script headers of the real nf-slurm-no-memory 1.0.1, which a test compares. -->
<!-- Copyright (c) 2026 The uwe Contributors -->
<!-- SPDX-License-Identifier: MIT or Apache-2.0 -->

# SLURM fixtures

`nf_slurm_default.txt` and `nf_slurm_per_core_8.txt` were recorded on 2026-11-06 at about 13:30
Europe/Berlin with Nextflow 26.04.6 and nf-slurm-no-memory 1.0.1, with fake `sbatch`, `squeue`
and `scancel` programs that appended the `#SBATCH` lines of each script to the file and ran the
script on the recording machine. No SLURM cluster ran.

The pipeline had four processes: `BIG_MEMORY` (2 CPUs, 10 GB, 2 hours, queue `short`, cluster
option `--qos=normal`), `MANY_CPUS` (4 CPUs, 2 GB), `NO_MEMORY` (no resources) and `ODD_MEMORY`
(1 CPU, 4097 MB), with `process.executor = 'slurm_no_memory'` and the account `lab01`; the
second file adds `executor.$slurm_no_memory.memoryPerCore = '8'`. The pipeline ran twice for
each file, so each process has two blocks. `<probe>` stands for the recording folder; the task
folders differ per run.
