#!/usr/bin/env bash
# Rebuilds the simulated RO-Crate fixture and checks required and recommended profiles.
# Needs rocrate-validator from `uv tool install roc-validator`.
# Copyright (c) 2026 The uwe Contributors
# SPDX-License-Identifier: MIT or Apache-2.0
set -euo pipefail
here="$(cd "$(dirname "$0")" && pwd)"
cd "$here/../../.."
RUN_CRATE_WRITE_FIXTURE=1 cargo test -p run-crate --test export --locked
# A copy in a fixed folder, so that the report is the same on every machine.
copy=/tmp/iunctus-crate-validation
rm -rf "$copy"
mkdir -p "$copy"
trap 'rm -rf "$copy"' EXIT
cp -r "$here/simulated" "$copy/"
validate() {
	rocrate-validator validate --profile-identifier provenance-run-crate-0.5 \
		--requirement-severity "$1" --verbose --no-paging --output-line-width 100 "$copy/simulated"
}
{
	rocrate-validator --version
	validate required
	validate recommended || true
} 2>&1 | sed "s|$copy/||g" > "$here/validation.txt"
