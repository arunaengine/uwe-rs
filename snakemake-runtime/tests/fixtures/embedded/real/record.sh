#!/usr/bin/env bash
# Records in expected-results/ what Snakemake 9.27.0 makes for the jobs of this folder that
# need no image to run on the host: the Python script, the Bash script and the Python
# wrapper (Snakemake runs them without containers unless it is told to use them). It needs uv
# with snakemake 9.27.0 in its cache or on PyPI (UV_OFFLINE=1 uses only the cache).
set -euo pipefail
here="$(cd "$(dirname "$0")" && pwd)"
run="$(mktemp -d)"
trap 'rm -rf "$run"' EXIT
cp -r "$here"/. "$run/"
rm -rf "$run/expected-results"
cd "$run"
targets=(python/A.txt bash/A.txt wrapper/A.txt)
uvx --from snakemake==9.27.0 snakemake "${targets[@]}" --cores 2 --quiet all > /dev/null
mkdir -p "$here/expected-results"
for target in "${targets[@]}" logs/python/A.log logs/bash/A.log logs/wrapper/A.log; do
    mkdir -p "$here/expected-results/$(dirname "$target")"
    cp "$target" "$here/expected-results/$target"
done
