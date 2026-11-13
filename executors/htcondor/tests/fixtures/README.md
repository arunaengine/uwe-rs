<!-- The submit description of the real nf-ospool 0.1.1, which a test compares. -->
<!-- Copyright (c) 2026 The uwe Contributors -->
<!-- SPDX-License-Identifier: MIT or Apache-2.0 -->

# HTCondor fixtures

`nf_ospool_submit.condor` was recorded on 2026-11-06 at about 14:09 Europe/Berlin with
Nextflow 26.04.6 and nf-ospool 0.1.1, with fake `condor_submit`, `condor_q` and `condor_rm`
programs that kept each submit file and ran its executable on the recording machine. No
HTCondor pool ran.

The process asked for 2 CPUs, 4 GB of memory, 10 GB of disk and 1 hour, with the cluster
options `HasCHTCStaging=true; +ProjectName = "probe"`. `<probe>` stands for the recording
folder.
