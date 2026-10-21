#!/usr/bin/env bash
# Records native Nextflow stub scripts for the selected nf-core pipelines.
# Copyright (c) 2026 The uwe Contributors
# SPDX-License-Identifier: MIT or Apache-2.0
set -euo pipefail
here="$(cd "$(dirname "$0")" && pwd)"
test_workflows="$1"
shift
for pipeline in "$@"; do
	run="$(mktemp -d)"
	(cd "$run" && nextflow run "$test_workflows/$pipeline" -profile test,docker -stub-run \
		--outdir /tmp/uwe-stub-out -ansi-log false > run.log 2>&1)
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
