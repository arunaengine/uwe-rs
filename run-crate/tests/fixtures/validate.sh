#!/usr/bin/env bash
# Writes the fixture crate of a simulated run again (tests/export.rs) and checks it with
# rocrate-validator against the Provenance Run Crate profile 0.5: first at the severity REQUIRED,
# which must pass, then at RECOMMENDED, whose report lists what the crate could still add. Both
# go to validation.txt next to this script. Needs the validator in user space:
# `uv tool install roc-validator`.
# Copyright (c) 2026 The iunctus Contributors
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
