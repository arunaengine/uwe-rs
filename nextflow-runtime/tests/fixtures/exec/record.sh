#!/usr/bin/env bash
# Records Nextflow 26.04.6 exec results and printed output locally without containers or network.
# Copyright (c) 2026 The uwe Contributors
# SPDX-License-Identifier: MIT or Apache-2.0
set -euo pipefail
here="$(cd "$(dirname "$0")" && pwd)"
run="$(mktemp -d)"
trap 'rm -rf "$run"' EXIT
cp "$here/main.nf" "$run/"
cd "$run"
NXF_OFFLINE=true nextflow -q run main.nf -ansi-log false > printed.txt
rm -rf "$here/nextflow-26.04.6"
mkdir -p "$here/nextflow-26.04.6"
cp -r results "$here/nextflow-26.04.6/"
grep -Ev '^\[[A-Z]+(\]| )' printed.txt > "$here/nextflow-26.04.6/printed.txt"
