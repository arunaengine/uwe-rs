#!/bin/sh
# Records nf-core-utils 0.5.0 and 0.7.0 probe output with Nextflow 26.04.6 into expected/.
# Copyright (c) 2026 The uwe Contributors
# SPDX-License-Identifier: MIT or Apache-2.0
# Recorded 2026-11-06 (Europe/Berlin): the run's folder became <probe>, and fake_servers.py,
# a local SMTP and HTTP recorder, gave mail-*.txt and slack-post.txt. The configs pin the
# plugin and hold the settings that tests/nf_core_utils.rs writes as COMMON and the servers.
python3 -I fake_servers.py received 1200 &
NXF_OFFLINE=true nextflow run core.nf -c v0.5.0.config -c fake.config -ansi-log false > expected/core-0.5.0.txt
NXF_OFFLINE=true nextflow run core.nf -c v0.7.0.config -c fake.config -ansi-log false > expected/core-0.7.0.txt
TOWER_WORKFLOW_ID=abc NXF_OFFLINE=true nextflow run new07.nf -c v0.7.0.config -c fake.config \
    -ansi-log false -profile standard > expected/new07.txt
cp results/pipeline_info/pipeline_report.txt expected/report.txt
