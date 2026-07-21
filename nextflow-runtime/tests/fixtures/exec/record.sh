#!/usr/bin/env bash
# Records what Nextflow 26.04.6 publishes for main.nf into nextflow-26.04.6/, and what it
# prints besides its progress. It runs the processes on this machine without containers; needs no network.
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
