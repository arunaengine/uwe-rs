#!/usr/bin/env bash
# Builds and selects the demo binary and config for this shell.
# Copyright (c) 2026 The uwe Contributors
# SPDX-License-Identifier: MIT or Apache-2.0

if [ -n "${ZSH_VERSION:-}" ]; then
  iunctus_demo=${${(%):-%x}:A:h}
else
  iunctus_demo=$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)
fi
iunctus_root=$(dirname "$iunctus_demo")

if (cd "$iunctus_root" && cargo build --release -p uwe); then
  export PATH="$iunctus_root/target/release:$PATH"
  export IUNCTUS_SITE_CONFIG="$iunctus_demo/local-config.toml"
  cd "$iunctus_demo" || return
  # The complex examples are not in the repository: the first time, download them.
  if [ ! -d nextflow/nf-core-demo ]; then
    echo "Downloading nf-core/demo 1.2.0 into nextflow/nf-core-demo ..."
    git -c advice.detachedHead=false clone -q --depth 1 --branch 1.2.0 \
      https://github.com/nf-core/demo nextflow/nf-core-demo \
      || echo "The download of nf-core/demo failed; source this file again to retry." >&2
  fi
  if [ ! -d snakemake/data ]; then
    echo "Downloading the data of the Snakemake tutorial into snakemake/data ..."
    iunctus_data=https://raw.githubusercontent.com/snakemake/snakemake-tutorial-data/b21b0248ab28d3fc1973789ef8c8e68f29c11d61/data
    rm -rf snakemake/data.part
    if (for file in genome.fa genome.fa.amb genome.fa.ann genome.fa.bwt genome.fa.fai \
          genome.fa.pac genome.fa.sa samples/A.fastq samples/B.fastq; do
          curl -fsSL --create-dirs -o "snakemake/data.part/$file" "$iunctus_data/$file" || exit
        done); then
      mv snakemake/data.part snakemake/data
    else
      rm -rf snakemake/data.part
      echo "The download of the Snakemake data failed; source this file again to retry." >&2
    fi
    unset iunctus_data
  fi
  cat <<'EOF'

The demo is ready in this shell. Run these here, in demo/. First the simple ones:

  iunctus run nextflow/main.nf
  iunctus run nextflow/main.nf                # again: all tasks come from the cache
  iunctus run snakemake/hello.smk
  iunctus run cwl/hello_world.cwl cwl/hello_world-job.json
  iunctus run argo/hello-world.yaml

Then the complex ones:

  iunctus run nextflow/nf-core-demo -profile test,docker --outdir results
  iunctus run snakemake/Snakefile
  iunctus run cwl/scatter-workflow.cwl cwl/scatter-job.yml
  iunctus run argo/dag-diamond.yaml

Then look at a run:

  iunctus runs                                # the runs, newest first
  iunctus time <run>                          # where the time went
  iunctus page <run>                          # the live page of a run
  iunctus graph snakemake/Snakefile -format svg > graph.svg

Each run writes report.html here; open it in a browser.
EOF
  # Run, not sourced: what it set ends with it, so it goes on in a new shell. That shell reads
  # its startup files, which may set PATH again, so a startup file of the script reads them
  # first, then puts iunctus first on the PATH. It removes itself.
  if [ -z "${ZSH_VERSION:-}" ] && [ "${BASH_SOURCE[0]}" = "$0" ]; then
    echo "This is a new shell for the demo; \`exit\` goes back."
    iunctus_start=$(mktemp -d)
    case ${SHELL:-} in
      *zsh)
        cat > "$iunctus_start/.zshenv" <<EOF
ZDOTDIR=${ZDOTDIR:-$HOME}
[ -f "\$ZDOTDIR/.zshenv" ] && source "\$ZDOTDIR/.zshenv"
export PATH="$iunctus_root/target/release:\$PATH"
rm -rf "$iunctus_start"
EOF
        ZDOTDIR=$iunctus_start exec "$SHELL" -i ;;
      *)
        cat > "$iunctus_start/bashrc" <<EOF
[ -f ~/.bashrc ] && source ~/.bashrc
export PATH="$iunctus_root/target/release:\$PATH"
rm -rf "$iunctus_start"
EOF
        exec bash --rcfile "$iunctus_start/bashrc" -i ;;
    esac
  fi
else
  echo "The build of iunctus failed; nothing was changed." >&2
fi
unset iunctus_demo iunctus_root
