#!/usr/bin/env bash
# Records Snakemake 9.27.0 preambles with normalized folders using uv and a fixed hash seed.
# Copyright (c) 2026 The iunctus Contributors
# SPDX-License-Identifier: MIT or Apache-2.0
set -euo pipefail
here="$(cd "$(dirname "$0")" && pwd)"
run="$(mktemp -d)"
trap 'rm -rf "$run"' EXIT
cp -r "$here/Snakefile" "$here/config.yaml" "$here/in.txt" "$here/in2.txt" "$here/scripts" "$run/"
cd "$run"
# Snakemake orders the default resources memory and disk by a set, whose order depends on
# Python's hash seed; seed 0 gives memory first, as iunctus orders them.
PYTHONHASHSEED=0 uvx --from snakemake==9.27.0 snakemake --cores 2 --default-resources --keep-going \
    --skip-script-cleanup --quiet > /dev/null 2>&1 || true
mkdir -p "$here/snakemake-9.27.0"
for ext in R sh jl rs Rmd; do
    sed "s|$run|{folder}|g" .snakemake/scripts/tmp*.test.$ext > "$here/snakemake-9.27.0/test.$ext"
done
