#!/usr/bin/env bash
# Records the stub scripts of nf-core pipelines with Nextflow 26.04.6 and Docker:
# `sh record.sh <corpus>/pipelines <pipeline>...`. Each task's `.command.sh`, up to the capture
# block of `eval` outputs, goes to `<pipeline>/<task name>.sh`, with `:` and spaces in the name
# replaced by `_`.
set -euo pipefail
here="$(cd "$(dirname "$0")" && pwd)"
corpus="$1"
shift
for pipeline in "$@"; do
	run="$(mktemp -d)"
	(cd "$run" && nextflow run "$corpus/$pipeline" -profile test,docker -stub-run \
		--outdir /tmp/iunctus-stub-out -ansi-log false > run.log 2>&1)
	rm -rf "$here/$pipeline"
	mkdir -p "$here/$pipeline"
	for header in "$run"/work/*/*/.command.run; do
		name="$(sed -n "s/^### name: '\(.*\)'$/\1/p" "$header")"
		file="$(printf '%s' "$name" | tr ': ()' '____' | tr -s '_' | sed 's/_$//')"
		awk '/^# capture process environment$/ {exit} {print}' "$(dirname "$header")/.command.sh" \
			| sed -e :a -e '/^\n*$/{$d;N;ba' -e '}' > "$here/$pipeline/$file.sh"
	done
	rm -rf "$run"
done
