#!/usr/bin/env bash
# Builds and selects the demo binary and config for this shell.
# Copyright (c) 2026 The uwe Contributors
# SPDX-License-Identifier: MIT or Apache-2.0

if [ -n "${ZSH_VERSION:-}" ]; then
  uwe_demo=${${(%):-%x}:A:h}
else
  uwe_demo=$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)
fi
uwe_root=$(dirname "$uwe_demo")

if (cd "$uwe_root" && cargo build --release -p uwe); then
  export PATH="$uwe_root/target/release:$PATH"
  export UWE_SITE_CONFIG="$uwe_demo/local-config.toml"
  cd "$uwe_demo" || return
  # The complex examples are not in the repository: the first time, download them.
  if [ ! -d nextflow/nf-core-demo ]; then
    echo "Downloading nf-core/demo 1.2.0 into nextflow/nf-core-demo ..."
    git -c advice.detachedHead=false clone -q --depth 1 --branch 1.2.0 \
      https://github.com/nf-core/demo nextflow/nf-core-demo \
      || echo "The download of nf-core/demo failed; source this file again to retry." >&2
  fi
  if [ ! -d snakemake/data ]; then
    echo "Downloading the data of the Snakemake tutorial into snakemake/data ..."
    uwe_data=https://raw.githubusercontent.com/snakemake/snakemake-tutorial-data/b21b0248ab28d3fc1973789ef8c8e68f29c11d61/data
    rm -rf snakemake/data.part
    if (for file in genome.fa genome.fa.amb genome.fa.ann genome.fa.bwt genome.fa.fai \
          genome.fa.pac genome.fa.sa samples/A.fastq samples/B.fastq; do
          curl -fsSL --create-dirs -o "snakemake/data.part/$file" "$uwe_data/$file" || exit
        done); then
      mv snakemake/data.part snakemake/data
    else
      rm -rf snakemake/data.part
      echo "The download of the Snakemake data failed; source this file again to retry." >&2
    fi
    unset uwe_data
  fi
  cat <<'EOF'

The demo is ready in this shell. Run these here, in demo/. First the simple ones:

  uwe run nextflow/main.nf
  uwe run nextflow/main.nf                    # again: all tasks come from the cache
  uwe run snakemake/hello.smk
  uwe run cwl/hello_world.cwl cwl/hello_world-job.json
  uwe run argo/hello-world.yaml

Then the complex ones:

  uwe run nextflow/nf-core-demo -profile test,docker --outdir results
  uwe run snakemake/Snakefile
  uwe run cwl/scatter-workflow.cwl cwl/scatter-job.yml
  uwe run argo/dag-diamond.yaml

Then look at a run:

  uwe runs                                    # the runs, newest first
  uwe time <run>                              # where the time went
  uwe page <run>                              # the live page of a run
  uwe graph snakemake/Snakefile -format svg > graph.svg

Each run writes report.html here; open it in a browser.
EOF
  # Run, not sourced: what it set ends with it, so it goes on in a new shell. That shell reads
  # its startup files, which may set PATH again, so a startup file of the script reads them
  # first, then puts uwe first on the PATH. It removes itself.
  if [ -z "${ZSH_VERSION:-}" ] && [ "${BASH_SOURCE[0]}" = "$0" ]; then
    echo "This is a new shell for the demo; \`exit\` goes back."
    uwe_start=$(mktemp -d)
    case ${SHELL:-} in
      *zsh)
        cat > "$uwe_start/.zshenv" <<EOF
ZDOTDIR=${ZDOTDIR:-$HOME}
[ -f "\$ZDOTDIR/.zshenv" ] && source "\$ZDOTDIR/.zshenv"
export PATH="$uwe_root/target/release:\$PATH"
rm -rf "$uwe_start"
EOF
        ZDOTDIR=$uwe_start exec "$SHELL" -i ;;
      *)
        cat > "$uwe_start/bashrc" <<EOF
[ -f ~/.bashrc ] && source ~/.bashrc
export PATH="$uwe_root/target/release:\$PATH"
rm -rf "$uwe_start"
EOF
        exec bash --rcfile "$uwe_start/bashrc" -i ;;
    esac
  fi
else
  echo "The build of uwe failed; nothing was changed." >&2
fi
unset uwe_demo uwe_root
