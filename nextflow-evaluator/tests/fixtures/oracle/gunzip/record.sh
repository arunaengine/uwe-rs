#!/usr/bin/env bash
# Records real and stub GUNZIP commands with Nextflow 26.04.6; see the crate README for usage.
# Copyright (c) 2026 The uwe Contributors
# SPDX-License-Identifier: MIT or Apache-2.0
set -euo pipefail
modules=$1
output=$(cd "$2" && pwd)
work=$(mktemp -d)
cd "$work"
printf '@r1\nACGT\n+\nIIII\n' | gzip > sample.fastq.gz
cat > main.nf <<NF
include { GUNZIP } from '$modules/modules/nf-core/gunzip/main.nf'
workflow {
    GUNZIP(channel.of([[id: 'S1'], file('sample.fastq.gz')]))
}
NF
for mode in script stub; do
    rm -rf work
    flags=()
    if [ "$mode" = stub ]; then flags=(-stub); fi
    NXF_OFFLINE=true nextflow -q run main.nf "${flags[@]}" -ansi-log false
    find work -name .command.sh -exec cp {} "$output/$mode.command.sh" \;
done
rm -rf "$work"
