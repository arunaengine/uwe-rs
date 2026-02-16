# Local recipes for linting, testing and checking, as CI will run them.
# Copyright (c) 2026 The iunctus Contributors
# SPDX-License-Identifier: MIT or Apache-2.0

# Formatting and Clippy.
lint:
	cargo +nightly-2026-09-14 fmt --all -- --check
	cargo +nightly-2026-09-14 clippy --workspace --all-targets --all-features --locked -- -D warnings

# Workspace tests and doctests.
test:
	cargo test --workspace --all-targets --all-features --locked
	cargo test --workspace --all-features --locked --doc

# Compile every target.
check:
	cargo check --workspace --all-targets --all-features --locked

# Fetches the text-only nf-core corpus at the commits the corpus test expects.
corpus:
	#!/usr/bin/env bash
	set -euo pipefail
	root="${NF_CORE_CORPUS:-$HOME/.cache/iunctus/corpus/nf-core-text}"
	grep -v '^#' nextflow-parser/tests/fixtures/nf_core_revisions.txt | while read -r name branch commit; do
		if [ "$name" = modules ]; then target="$root/modules"; else target="$root/pipelines/$name"; fi
		[ -d "$target" ] && continue
		work="$(mktemp -d)"
		git -C "$work" init -q
		git -C "$work" remote add origin "https://github.com/nf-core/$name.git"
		git -C "$work" fetch -q --depth 1 --filter=blob:none origin "$commit"
		git -C "$work" sparse-checkout set --no-cone '*.nf' '*.config' 'nextflow_schema.json'
		git -C "$work" checkout -q FETCH_HEAD
		rm -rf "$work/.git"
		mkdir -p "$(dirname "$target")"
		mv "$work" "$target"
	done

# Parses the whole nf-core corpus and compares the result with `nextflow lint`.
corpus-test:
	NF_CORE_CORPUS="${NF_CORE_CORPUS:-$HOME/.cache/iunctus/corpus/nf-core-text}" cargo test --release --locked -p nextflow-parser --test corpus -- --ignored
