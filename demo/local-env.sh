# Sets up this shell for the demo on a laptop or workstation with Docker or Podman.
# Source it, from bash or zsh, anywhere:
#
#   source demo/local-env.sh
#
# It builds the release binary of iunctus (cargo does nothing when it is up to date), puts it
# first on the PATH, makes demo/local-config.toml the config of iunctus for this shell only
# and goes into demo/. Then it prints the commands of the demo, to run from there: tasks run
# in containers on this machine, and each run writes report.html into demo/.
# A config file in ~/.config/iunctus/ still wins over it. Nothing outside this shell changes.

if [ -n "${ZSH_VERSION:-}" ]; then
  iunctus_demo=${${(%):-%x}:A:h}
else
  iunctus_demo=$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)
fi
iunctus_root=$(dirname "$iunctus_demo")

if (cd "$iunctus_root" && cargo build --release -p iunctus); then
  export PATH="$iunctus_root/target/release:$PATH"
  export IUNCTUS_SITE_CONFIG="$iunctus_demo/local-config.toml"
  cd "$iunctus_demo" || return
  cat <<'EOF'

The demo is ready in this shell. Run these here, in demo/:

  iunctus run nextflow/hello/main.nf
  iunctus run nextflow/hello/main.nf          # again: all tasks come from the cache
  iunctus run cwl/quick-start/hello_world.cwl cwl/quick-start/hello_world-job.json
  iunctus run cwl/workflows/scatter-workflow.cwl cwl/workflows/scatter-job.yml
  iunctus run argo/examples/hello-world.yaml
  iunctus run argo/examples/dag-diamond.yaml
  iunctus run argo/examples/artifact-passing.yaml
  iunctus run argo/examples/loops.yaml

Then look at a run:

  iunctus runs                                # the runs, newest first
  iunctus time <run>                          # where the time went
  iunctus page <run>                          # the live page of a run
  iunctus graph nextflow/hello/main.nf -format svg > graph.svg

Each run writes report.html here; open it in a browser.
The Snakemake tutorial and nf-core/demo need a download first: see demo/README.md.
EOF
else
  echo "The build of iunctus failed; nothing was changed." >&2
fi
unset iunctus_demo iunctus_root
