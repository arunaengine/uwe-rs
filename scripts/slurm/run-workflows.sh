#!/usr/bin/env bash
# Runs a CWL workflow and nf-core/demo with the SLURM executor on a one-node SLURM cluster in a
# container, whose jobs start the task containers on the Docker engine of this machine.
# Copyright (c) 2026 The uwe Contributors
# SPDX-License-Identifier: MIT or Apache-2.0
#
# Needs docker, network access and the static binary of `just release-binary` (UWE). The data
# folder (WORK, default a new temporary folder) has the same path inside the container and on
# this machine, so the task containers see the files of the jobs (shared-path staging).
# WORKFLOWS picks some of cwl and nf-core-demo; SLURM_CONTAINER names the container (default
# uwe-qualify-slurm); KEEP_CLUSTER=1 keeps the container and the data folder after the run.
set -euo pipefail

repository="$(cd "$(dirname "$0")/../.." && pwd)"
binary="$(realpath "${UWE:-$repository/target/x86_64-unknown-linux-musl/release/uwe}")"
name="${SLURM_CONTAINER:-uwe-qualify-slurm}"
image="$name:test"
workflows="${WORKFLOWS:-cwl nf-core-demo}"
work="$(realpath "${WORK:-$(mktemp -d)}")"
mkdir -p "$work/home" "$work/cache"
cleanup() {
    if [ "${KEEP_CLUSTER:-0}" != 1 ]; then
        # Task containers may leave files of other users; the container removes them.
        docker exec "$name" rm -rf "$work/cache" "$work/home" > /dev/null 2>&1 || true
        docker rm -f "$name" > /dev/null 2>&1 || true
        rm -rf "$work"
    fi
}
trap cleanup EXIT

echo "=== SLURM container"
docker build -q -t "$image" "$repository/scripts/slurm" > /dev/null
# The node name is the host name that slurm.conf of the image expects.
docker run -d --name "$name" --hostname uwe-qualify-slurm \
    -v /var/run/docker.sock:/var/run/docker.sock -v "$work:$work" "$image" > /dev/null
docker cp "$binary" "$name:/usr/local/bin/uwe"
# uwe runs as the user of this machine, in the group of the Docker socket.
docker exec "$name" bash -c "groupadd -o -g $(stat -c %g /var/run/docker.sock) docker-host \
    && useradd -o -u $(id -u) -g docker-host -M -d $work/home uwe"
for _ in $(seq 60); do
    docker exec "$name" sinfo -h -o %t 2> /dev/null | grep -q idle && break
    sleep 1
done
printf '[executor.slurm]\ncontainer = "docker"\n' > "$work/uwe.toml"

# Runs uwe in the folder $work/$1 of the container, with the remaining arguments.
run() {
    local folder="$work/$1"
    shift
    mkdir -p "$folder"
    docker exec -u uwe -w "$folder" -e HOME="$work/home" -e XDG_CACHE_HOME="$work/cache" \
        -e XDG_CONFIG_HOME="$work/home" "$name" \
        uwe run "$@" -executor slurm -uwe-config "$work/uwe.toml"
}

status=0
for workflow in $workflows; do
    echo "=== $workflow"
    started=$SECONDS
    case "$workflow" in
        cwl)
            cp "$repository/demo/cwl/scatter-workflow.cwl" "$repository/demo/cwl/hello_world.cwl" \
                "$repository/demo/cwl/scatter-job.yml" "$work/"
            run cwl "$work/scatter-workflow.cwl" "$work/scatter-job.yml" \
                -default-image debian:stable-slim
            ;;
        nf-core-demo)
            run nf-core-demo nf-core/demo -r 32893afef8076a03a2767a020b3f0cab2e0b40b2 \
                -profile test,docker --outdir "$work/nf-core-demo/results" \
                && [ -s "$work/nf-core-demo/results/multiqc/multiqc_report.html" ]
            ;;
        *)
            echo "unknown workflow $workflow" >&2
            false
            ;;
    esac && result=passed || { result=FAILED; status=1; }
    echo "=== $workflow: $result after $((SECONDS - started)) s"
done
exit "$status"
