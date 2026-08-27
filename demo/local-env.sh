# Sets up this shell for the demo on a laptop or workstation with Docker or Podman.
# Source it, from bash or zsh, anywhere:
#
#   source demo/local-env.sh
#
# It builds the release binary of iunctus (cargo does nothing when it is up to date), puts it
# first on the PATH and makes demo/local-config.toml the config of iunctus for this shell
# only: tasks run in containers on this machine, and each run writes report.html.
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
  echo "iunctus: $(command -v iunctus)"
  echo "config:  $IUNCTUS_SITE_CONFIG"
  echo "Try:     cp -r $iunctus_demo ~/iunctus-demo && cd ~/iunctus-demo/nextflow/hello && iunctus run main.nf"
  echo "Results and report.html land in the folder of the example; demo/README.md says more."
else
  echo "The build of iunctus failed; nothing was changed." >&2
fi
unset iunctus_demo iunctus_root
