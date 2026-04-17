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
		git -C "$work" fetch -q --depth 1 --filter=blob:limit=1m origin "$commit"
		git -C "$work" sparse-checkout set --no-cone '*.nf' '*.config' '*.groovy' 'nextflow_schema.json' '/CITATIONS.md'
		git -C "$work" checkout -q FETCH_HEAD
		# The files of assets/, bin/, docs/images/, tests/ and templates/ that runs read; files
		# over 1 MiB are not downloaded and stay empty, so that they exist.
		git -C "$work" ls-tree -r -l FETCH_HEAD | grep -E $'\t(assets|bin|docs/images|tests)/|/templates/' | while IFS=$'\t' read -r meta path; do
			read -r mode type sha size <<< "$meta"
			[ "$type" = blob ] || continue
			mkdir -p "$(dirname "$work/$path")"
			if [ "$size" = - ] || [ "$size" -gt 1048576 ]; then : > "$work/$path"; else git -C "$work" cat-file blob "$sha" > "$work/$path"; fi
			if [ "$mode" = 100755 ]; then chmod +x "$work/$path"; fi
		done
		rm -rf "$work/.git"
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

# Runs every nf-core pipeline of the corpus with the mock executor and compares the outcomes.
run-corpus-test:
	NF_CORE_CORPUS="${NF_CORE_CORPUS:-$HOME/.cache/iunctus/corpus/nf-core-text}" \
	cargo test --release --locked -p iunctus --test run_corpus -- --ignored

# Records the stub scripts of the pipelines in iunctus/tests/fixtures/oracle/stub with Nextflow
# and Docker; give the pipelines as arguments, like `just stub-oracle demo`.
stub-oracle +pipelines:
	iunctus/tests/fixtures/oracle/stub/record.sh "${NF_CORE_CORPUS:-$HOME/.cache/iunctus/corpus/nf-core-text}/pipelines" {{pipelines}}

# Runs the recorded pipelines with -stub and the mock executor and compares the task scripts.
stub-corpus-test:
	NF_CORE_CORPUS="${NF_CORE_CORPUS:-$HOME/.cache/iunctus/corpus/nf-core-text}" \
	cargo test --release --locked -p iunctus --test stub_corpus -- --ignored

# Fetches the text-only CWL corpus at the revisions the CWL corpus test expects.
cwl-corpus:
	#!/usr/bin/env bash
	set -euo pipefail
	root="${CWL_CORPUS:-$HOME/.cache/iunctus/corpus/cwl}"
	fixtures="$PWD/cwl-parser/tests/fixtures/corpus"
	paths="$(grep -v '^#' "$fixtures/revisions.txt" | cut -d' ' -f1)"
	mkdir -p "$root"
	# The paths of the files below a folder of the git tree of a WorkflowHub entry version.
	workflowhub_tree() {
		curl -sfL --retry 3 -H 'Accept: application/json' "$1/tree/$2" | jq -r '.tree[] | .type + " " + .path' \
			| while read -r type name; do
				if [ "$type" = tree ]; then workflowhub_tree "$1" "$name"; else printf '%s\n' "$name"; fi
			done
	}
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
			entry="https://workflowhub.eu/workflows/${path##*/}"
			if ! curl -sfL --retry 3 -o "$work.zip" "$entry/ro_crate?version=${ref#version-}"; then
				echo "skipped $path: cannot download it from WorkflowHub" >&2
				rm -rf "$work" "$work.zip"
				continue
			fi
			if unzip -tqq "$work.zip" >/dev/null 2>&1; then
				{ unzip -Z1 "$work.zip" | grep -iE '\.(cwl|ya?ml|json|js)$' | grep -vx 'ro-crate-metadata.json' || true; printf '%s\n' "$referenced"; } \
					| grep -v '^$' | xargs -r -d '\n' unzip -qo "$work.zip" -d "$work" || true
			else
				# Some entries give their web page instead of an RO-Crate; their git tree holds
				# the same files.
				git_tree="$entry/git/${ref#version-}"
				{ workflowhub_tree "$git_tree" "" | grep -iE '\.(cwl|ya?ml|json|js)$' | grep -vx 'ro-crate-metadata.json' || true; printf '%s\n' "$referenced"; } \
					| grep -v '^$' | while IFS= read -r file; do
						mkdir -p "$(dirname "$work/$file")"
						curl -sfL --retry 3 -o "$work/$file" "$git_tree/raw/${file// /%20}" || rm -f "$work/$file"
					done || true
			fi
			rm -f "$work.zip"
			if [ -z "$(ls -A "$work")" ]; then
				echo "skipped $path: WorkflowHub gives none of its files" >&2
				rm -rf "$work"
				continue
			fi
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
			# A source the network cannot reach is left out, so the rest still arrives.
			if ! git -C "$work" fetch -q --depth 1 --filter=blob:none origin "$commit"; then
				echo "skipped $path: cannot fetch $url" >&2
				rm -rf "$work"
				continue
			fi
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

# Fetches the data files of the CWL conformance tests (inputs like `whale.txt`), which the
# text-only corpus leaves out, into a folder of their own, so that the corpus stays as
# `cwltool --validate` saw it. The trees come from the corpus checkouts, the files from
# raw.githubusercontent.com; files over 1 MiB are left out, as in the corpus.
cwl-conformance-data:
	#!/usr/bin/env -S uv run -q --with dulwich python3
	import os, subprocess, urllib.parse
	from dulwich.object_store import iter_tree_contents
	from dulwich.repo import Repo
	home = os.path.expanduser("~/.cache/iunctus/corpus")
	corpus = os.environ.get("CWL_CORPUS") or f"{home}/cwl"
	data = os.environ.get("CWL_CONFORMANCE_DATA") or f"{home}/cwl-conformance-data"
	for line in open("cwl-parser/tests/fixtures/corpus/revisions.txt"):
	    if line.startswith("#") or not line.startswith("conformance/"):
	        continue
	    path, url, ref, commit, *prefixes = line.split()
	    folders = [prefix for prefix in prefixes if prefix.endswith("/")]
	    repo = Repo(os.path.join(corpus, path))
	    for entry in iter_tree_contents(repo.object_store, repo[commit.encode()].tree):
	        name = entry.path.decode()
	        target = os.path.join(data, path, name)
	        if not any(name.startswith(folder) for folder in folders):
	            continue
	        if os.path.lexists(os.path.join(corpus, path, name)) or os.path.lexists(target):
	            continue
	        raw = url.replace("https://github.com/", "https://raw.githubusercontent.com/")
	        body = subprocess.run(["curl", "-sfL", "--retry", "3", f"{raw}/{commit}/{urllib.parse.quote(name)}"],
	                              capture_output=True, check=True).stdout
	        if len(body) > 1 << 20:
	            continue
	        os.makedirs(os.path.dirname(target), exist_ok=True)
	        if entry.mode == 0o120000:
	            os.symlink(body.decode(), target)
	            continue
	        with open(target, "wb") as file:
	            file.write(body)
	        if entry.mode & 0o111:
	            os.chmod(target, 0o755)
	    repo.close()

# Runs every CWL conformance test through the driver and the mock executor and compares the
# outcomes with the recorded ones; `just cwl-conformance-data` fetches the test data first.
cwl-conformance-test:
	CWL_CORPUS="${CWL_CORPUS:-$HOME/.cache/iunctus/corpus/cwl}" \
	CWL_CONFORMANCE_DATA="${CWL_CONFORMANCE_DATA:-$HOME/.cache/iunctus/corpus/cwl-conformance-data}" \
	cargo test --release --locked -p cwl-runtime --test conformance -- --ignored

# Builds the execution graph of every CWL document of the corpus that cwltool validates, with
# placeholder inputs, and compares the outcomes with the recorded ones.
cwl-graph-corpus-test:
	CWL_CORPUS="${CWL_CORPUS:-$HOME/.cache/iunctus/corpus/cwl}" cargo test --release --locked -p cwl-runtime --test graph_corpus -- --ignored

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

# Adds the files that Snakemake runs read while they plan to the folders of `just snakemake-corpus`:
# text files up to 1 MiB of each workflow (config files, sample sheets, schemas, helper modules)
# and every file up to 1 MiB of the Snakemake tests (inputs and expected results). Other files
# are made empty, so that they exist. A folder is done once it holds `.iunctus-data`.
snakemake-corpus-data:
	#!/usr/bin/env bash
	set -euo pipefail
	root="${SNAKEMAKE_CORPUS:-$HOME/.cache/iunctus/corpus/snakemake}"
	grep -v '^#' snakemake-parser/tests/fixtures/corpus_revisions.txt | while read -r repository branch commit; do
		if [ "$repository" = snakemake/snakemake ]; then
			target="$root/snakemake-tests"
			scope='^tests/'
			text='.'
		else
			target="$root/catalog/${repository/\//__}"
			scope='.'
			text='\.(ya?ml|json|tsv|csv|txt|py|cfg|ini|toml|tab|list|fai|bed|smk|snakefile)$|(^|/)[Ss]nakefile$'
		fi
		[ -d "$target" ] && [ ! -e "$target/.iunctus-data" ] || continue
		work="$(mktemp -d)"
		git -C "$work" init -q
		git -C "$work" config gc.auto 0
		git -C "$work" remote add origin "https://github.com/$repository.git"
		# One pack brings every file up to 1 MiB; a source the network cannot reach is left
		# out, so the rest still arrives.
		if ! git -C "$work" fetch -q --depth 1 --filter=blob:limit=1048577 origin "$commit"; then
			echo "skipped $repository: cannot fetch" >&2
			rm -rf "$work"
			continue
		fi
		listing="$(mktemp)"
		# Larger files were not fetched, and their size shows as `BAD` instead of a number.
		git -C "$work" --no-lazy-fetch -c core.quotePath=false ls-tree -r -l FETCH_HEAD | grep -E $'^100[67]' | while IFS=$'\t' read -r meta path; do
			read -r mode type sha size <<< "$meta"
			if [[ "$path" =~ $scope ]] && [[ "$size" =~ ^[0-9]+$ ]] && [[ "$path" =~ $text ]]; then
				printf 'fetch\t%s\n' "$path"
			elif [[ "$path" =~ $scope ]]; then
				printf 'empty\t%s\n' "$path"
			fi
		done > "$listing" || true
		# Sparse checkout patterns match like .gitignore, so special characters are escaped.
		grep $'^fetch\t' "$listing" | cut -f2- | sed -e 's/[][*?\\]/\\&/g' -e 's|^|/|' \
			| git -C "$work" sparse-checkout set --no-cone --stdin
		git -C "$work" checkout -q FETCH_HEAD
		grep $'^empty\t' "$listing" | cut -f2- | while IFS= read -r path; do
			mkdir -p "$(dirname "$work/$path")"
			: > "$work/$path"
		done
		rm -rf "$work/.git" "$listing"
		cp -r --update=none "$work/." "$target/"
		rm -rf "$work"
		touch "$target/.iunctus-data"
	done

# Parses the whole Snakemake corpus and compares the result with Snakemake's parser.py.
snakemake-corpus-test:
	SNAKEMAKE_CORPUS="${SNAKEMAKE_CORPUS:-$HOME/.cache/iunctus/corpus/snakemake}" cargo test --release --locked -p snakemake-parser --test corpus -- --ignored --nocapture

# Runs every workflow of the Snakemake corpus with the mock executor and compares the outcomes
# with snakemake-runtime/tests/fixtures/run_corpus.txt; `UPDATE_FIXTURE=1` rewrites the fixture.
snakemake-run-corpus-test:
	SNAKEMAKE_CORPUS="${SNAKEMAKE_CORPUS:-$HOME/.cache/iunctus/corpus/snakemake}" cargo test --release --locked -p snakemake-runtime --test run_corpus -- --ignored --exact workflows_run_as_recorded

# Fetches the text-only Argo corpus at the commits the Argo corpus test expects. The checkouts
# keep their `.git`, which marks the repository whose templates a workflow may reference.
argo-corpus:
	#!/usr/bin/env bash
	set -euo pipefail
	root="${ARGO_CORPUS:-$HOME/.cache/iunctus/corpus/argo}"
	mkdir -p "$root"
	grep -v '^#' argo-parser/tests/fixtures/corpus/revisions.txt | while read -r repository branch commit patterns; do
		target="$root/${repository#*/}"
		[ -d "$target" ] && continue
		work="$(mktemp -d "$root/.fetch.XXXXXX")"
		git -C "$work" init -q
		git -C "$work" remote add origin "https://github.com/$repository.git"
		{
			for extension in yaml yml json YAML YML JSON; do
				if [ -z "$patterns" ]; then echo "*.$extension"; fi
				for pattern in $patterns; do
					case "$pattern" in */) echo "/$pattern**/*.$extension" ;; esac
				done
			done
			for pattern in $patterns; do case "$pattern" in '!'*) echo "$pattern" ;; esac; done
		} | git -C "$work" sparse-checkout set --no-cone --stdin
		# A source the network cannot reach is left out, so the rest still arrives.
		if ! git -C "$work" fetch -q --depth 1 --filter=blob:none origin "$commit"; then
			echo "skipped $repository: cannot fetch it" >&2
			rm -rf "$work"
			continue
		fi
		git -C "$work" checkout -q FETCH_HEAD
		mv "$work" "$target"
	done

# Runs every Argo document of the corpus that has an entrypoint with the mock executor and
# compares the outcomes with argo-runtime/tests/fixtures/corpus/outcomes.txt.
argo-corpus-test:
	ARGO_CORPUS="${ARGO_CORPUS:-$HOME/.cache/iunctus/corpus/argo}" cargo test --release --locked -p argo-runtime --test corpus -- --ignored --nocapture

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
