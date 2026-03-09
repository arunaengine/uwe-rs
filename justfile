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

# Fetches the text-only nf-core corpus at the commits the corpus tests expect.
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
		# Scripts, configs and schemas, and the small text files of assets/ that runs read.
		git -C "$work" sparse-checkout set --no-cone '*.nf' '*.config' 'nextflow_schema.json' \
			'/assets/**/*.json' '/assets/**/*.csv' '/assets/**/*.tsv' '/assets/**/*.yml' \
			'/assets/**/*.yaml' '/assets/**/*.txt'
		git -C "$work" checkout -q FETCH_HEAD
		rm -rf "$work/.git"
		find "$work" -path '*/assets/*' -type f -size +1M -delete
		mkdir -p "$(dirname "$target")"
		mv "$work" "$target"
	done

# Parses the nf-core corpus, compares the result with `nextflow lint`, and lowers every module.
corpus-test:
	NF_CORE_CORPUS="${NF_CORE_CORPUS:-$HOME/.cache/iunctus/corpus/nf-core-text}" cargo test --release --locked -p nextflow-parser --test corpus -- --ignored
	NF_CORE_CORPUS="${NF_CORE_CORPUS:-$HOME/.cache/iunctus/corpus/nf-core-text}" cargo test --release --locked -p nextflow-evaluator --test corpus -- --ignored

# Records `nextflow config -flat -profile test` of every nf-core pipeline for the config test.
config-oracle:
	#!/usr/bin/env bash
	set -euo pipefail
	corpus="${NF_CORE_CORPUS:-$HOME/.cache/iunctus/corpus/nf-core-text}/pipelines"
	oracle="${IUNCTUS_CONFIG_ORACLE:-$HOME/.cache/iunctus/oracle/config}"
	mkdir -p "$oracle"
	find "$corpus" -mindepth 1 -maxdepth 1 -type d -printf '%f\n' | sort | xargs -P 8 -I{} sh -c \
		"cd '$corpus/{}' && nextflow config -flat -profile test . > '$oracle/{}.txt' 2> '$oracle/{}.err'; echo \$? > '$oracle/{}.exit'"

# Compares `iunctus config` with the recorded `nextflow config` of every nf-core pipeline.
config-corpus-test:
	NF_CORE_CORPUS="${NF_CORE_CORPUS:-$HOME/.cache/iunctus/corpus/nf-core-text}" \
	IUNCTUS_CONFIG_ORACLE="${IUNCTUS_CONFIG_ORACLE:-$HOME/.cache/iunctus/oracle/config}" \
	cargo test --release --locked -p iunctus --test config_corpus -- --ignored

# Fetches the text-only CWL corpus at the revisions the CWL corpus test expects.
cwl-corpus:
	#!/usr/bin/env bash
	set -euo pipefail
	root="${CWL_CORPUS:-$HOME/.cache/iunctus/corpus/cwl}"
	fixtures="$PWD/cwl-parser/tests/fixtures/corpus"
	paths="$(grep -v '^#' "$fixtures/revisions.txt" | cut -d' ' -f1)"
	mkdir -p "$root"
	grep -v '^#' "$fixtures/revisions.txt" | while read -r path url ref commit prefixes; do
		target="$root/$path"
		if [ -d "$target/.git" ] || { [[ "$url" == https://workflowhub.eu/* ]] && [ -d "$target" ]; }; then
			continue
		fi
		# Referenced files go to the innermost checkout that holds them.
		referenced="$(grep "^$path/" "$fixtures/referenced.txt" | while read -r file; do
			owner="$(printf '%s\n' "$paths" | awk -v file="$file" 'index(file, $0 "/") == 1 { print length($0), $0 }' | sort -n | tail -1 | cut -d' ' -f2)"
			if [ "$owner" = "$path" ]; then printf '%s\n' "${file#"$path"/}"; fi
		done || true)"
		work="$(mktemp -d "$root/.fetch.XXXXXX")"
		if [[ "$url" == https://workflowhub.eu/* ]]; then
			curl -sfL --retry 3 -o "$work.zip" "https://workflowhub.eu/workflows/${path##*/}/ro_crate?version=${ref#version-}"
			{ unzip -Z1 "$work.zip" | grep -iE '\.(cwl|ya?ml|json|js)$' | grep -vx 'ro-crate-metadata.json' || true; printf '%s\n' "$referenced"; } \
				| grep -v '^$' | xargs -r -d '\n' unzip -qo "$work.zip" -d "$work" || true
			rm -f "$work.zip"
		else
			git -C "$work" init -q
			git -C "$work" remote add origin "$url"
			{
				for extension in cwl CWL yml YML yaml YAML json JSON js JS; do
					if [ -z "$prefixes" ]; then echo "*.$extension"; fi
					for prefix in $prefixes; do
						case "$prefix" in */) echo "/$prefix**/*.$extension" ;; esac
					done
				done
				for prefix in $prefixes; do case "$prefix" in */) ;; *) echo "/$prefix" ;; esac; done
				printf '%s\n' "$referenced" | grep -v '^$' | sed 's|^|/|' || true
			} | git -C "$work" sparse-checkout set --no-cone --stdin
			git -C "$work" fetch -q --depth 1 --filter=blob:none origin "$commit"
			git -C "$work" checkout -q FETCH_HEAD
		fi
		# The corpus leaves out files over 1 MiB, as its first download did.
		find "$work" -name .git -prune -o -type f -size +1M -print0 | xargs -r -0 rm -f
		mkdir -p "$(dirname "$target")"
		rmdir "$target" 2>/dev/null || true
		mv "$work" "$target"
	done

# Loads the whole CWL corpus and compares the result with `cwltool --validate`.
cwl-corpus-test:
	CWL_CORPUS="${CWL_CORPUS:-$HOME/.cache/iunctus/corpus/cwl}" cargo test --release --locked -p cwl-parser --test corpus -- --ignored

# Fetches the text-only Snakemake corpus at the commits the corpus test expects.
snakemake-corpus:
	#!/usr/bin/env bash
	set -euo pipefail
	root="${SNAKEMAKE_CORPUS:-$HOME/.cache/iunctus/corpus/snakemake}"
	grep -v '^#' snakemake-parser/tests/fixtures/corpus_revisions.txt | while read -r repository branch commit; do
		if [ "$repository" = snakemake/snakemake ]; then
			target="$root/snakemake-tests"
			patterns=('/tests/**/Snakefile' '/tests/**/snakefile' '/tests/**/*.smk' '/tests/**/*.snakefile')
		else
			target="$root/catalog/${repository/\//__}"
			patterns=('Snakefile' 'snakefile' '*.smk' '*.snakefile')
		fi
		[ -d "$target" ] && continue
		work="$(mktemp -d)"
		git -C "$work" init -q
		git -C "$work" remote add origin "https://github.com/$repository.git"
		git -C "$work" fetch -q --depth 1 --filter=blob:none origin "$commit"
		git -C "$work" sparse-checkout set --no-cone "${patterns[@]}"
		git -C "$work" checkout -q FETCH_HEAD
		rm -rf "$work/.git"
		mkdir -p "$(dirname "$target")"
		mv "$work" "$target"
	done

# Parses the whole Snakemake corpus and compares the result with Snakemake's parser.py.
snakemake-corpus-test:
	SNAKEMAKE_CORPUS="${SNAKEMAKE_CORPUS:-$HOME/.cache/iunctus/corpus/snakemake}" cargo test --release --locked -p snakemake-parser --test corpus -- --ignored --nocapture

# Starts a local RustFS (S3-compatible) with the bucket `iunctus-test` for the S3 tests.
# `docker stop iunctus-s3` ends it.
s3-server:
	#!/usr/bin/env bash
	set -euo pipefail
	docker run -d --rm --name iunctus-s3 -p 127.0.0.1:9000:9000 \
		-e RUSTFS_ALLOW_INSECURE_DEFAULT_CREDENTIALS=true rustfs/rustfs:latest >/dev/null
	until curl -sf http://127.0.0.1:9000/health >/dev/null; do sleep 1; done
	uv run -q --with boto3 python -c "import boto3; boto3.client('s3', \
		endpoint_url='http://127.0.0.1:9000', aws_access_key_id='rustfsadmin', \
		aws_secret_access_key='rustfsadmin', region_name='us-east-1').create_bucket(Bucket='iunctus-test')"

# Runs the S3 tests of content-store against the server from `just s3-server`.
s3-test:
	AWS_ACCESS_KEY_ID=rustfsadmin AWS_SECRET_ACCESS_KEY=rustfsadmin AWS_REGION=us-east-1 \
	IUNCTUS_S3_BUCKET=iunctus-test IUNCTUS_S3_ENDPOINT=http://127.0.0.1:9000 \
	cargo test -p content-store --test store -- --ignored
