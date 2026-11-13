<!-- The job header and job log of a run of the real nf-gadi 1.4.0, which tests compare. -->
<!-- Copyright (c) 2026 The uwe Contributors -->
<!-- SPDX-License-Identifier: MIT or Apache-2.0 -->

# PBS fixtures

Recorded on 2026-11-06 at about 14:10 Europe/Berlin with Nextflow 26.04.6, its `pbspro`
executor and nf-gadi 1.4.0, with fake `qsub`, `qstat` and `qdel` programs. No PBS server ran.

`nf_gadi_header.txt` holds the `#PBS` lines that Nextflow wrote for a process with 2 CPUs,
1 GB, 1 hour, the queue `normal` and the cluster options `-P ab12 -l storage=gdata/ab12`.
`nf_gadi_job.log` is the job log of that task: the fake `qsub` appended a resource summary with
the labels that nf-gadi reads, in a layout modelled on Gadi's, not copied from a Gadi log.
`<probe>` stands for the recording folder.
