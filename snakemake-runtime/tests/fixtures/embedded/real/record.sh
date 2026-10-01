#!/usr/bin/env bash
# Records real embedded-code workflow results with Snakemake.
# Copyright (c) 2026 The iunctus Contributors
# SPDX-License-Identifier: MIT or Apache-2.0
set -euo pipefail
here="$(cd "$(dirname "$0")" && pwd)"
run="$(mktemp -d)"
trap 'rm -rf "$run"' EXIT
cp -r "$here"/. "$run/"
rm -rf "$run/expected-results"
cd "$run"
targets=(python/A.txt bash/A.txt wrapper/A.txt run/A.txt run/A.json)
uvx --from snakemake==9.27.0 snakemake "${targets[@]}" --cores 2 --quiet all > /dev/null
mkdir -p "$here/expected-results"
for target in "${targets[@]}" logs/python/A.log logs/bash/A.log logs/wrapper/A.log; do
    mkdir -p "$here/expected-results/$(dirname "$target")"
    cp "$target" "$here/expected-results/$target"
done
