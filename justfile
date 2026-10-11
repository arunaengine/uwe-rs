# Local recipes for linting, testing and checking, as CI will run them.
# Copyright (c) 2026 The iunctus Contributors
# SPDX-License-Identifier: MIT or Apache-2.0

# Formatting and Clippy.
lint:
	cargo +nightly-2026-09-14 fmt --all -- --check
	cargo +nightly-2026-09-14 clippy --workspace --all-targets --all-features --locked -- -D warnings

# Static structure, naming, comment and header check from STYLE.md; no build.
style:
	python3 scripts/dev/check_style_tests.py
	python3 scripts/dev/check_style.py

# Workspace tests and doctests.
test:
	cargo test --workspace --all-targets --all-features --locked
	cargo test --workspace --all-features --locked --doc

# Compile every target.
check:
	cargo check --workspace --all-targets --all-features --locked

# Builds the portable Linux binary and keeps symbols for crash backtraces.
release-binary:
	rustup target add x86_64-unknown-linux-musl
	CC_x86_64_unknown_linux_musl=musl-gcc CARGO_INCREMENTAL=0 nice -n 19 cargo build --release --locked --target x86_64-unknown-linux-musl -p iunctus -j 8
	strip --strip-debug target/x86_64-unknown-linux-musl/release/iunctus
	file target/x86_64-unknown-linux-musl/release/iunctus

# Builds the image of Kubernetes object-store staging, which holds only the static iunctus
# binary, at /iunctus.
helper-image tag="iunctus-helper:dev": release-binary
	#!/usr/bin/env bash
	set -euo pipefail
	context="$(mktemp -d)"
	trap 'rm -rf "$context"' EXIT
	cp target/x86_64-unknown-linux-musl/release/iunctus "$context/iunctus"
	printf 'FROM scratch\nCOPY iunctus /iunctus\n' > "$context/Dockerfile"
	docker build -t "{{tag}}" "$context"

# Runs a workflow on a kind cluster with object-store staging (the uploaded binary, image volume,
# init container, and credentials with a cleanup), with kubedal staging and with shared paths on
# a claim; set KUBEDAL to a checkout of kubedal.
kind-staging-test: release-binary
	bash scripts/kind-staging.sh

# Runs four demos and checks failure, resume, cancellation and simulation with local Docker.
container-runs:
	nice -n 19 cargo build --offline --locked -p iunctus -j 8
	nice -n 19 python3 -I scripts/container-runs.py target/debug/iunctus

# Fetches the text-only nf-core test workflows at the commits the test-workflows tests expect.
test-workflows:
	#!/usr/bin/env bash
	set -euo pipefail
	root="${NF_CORE_TEST_WORKFLOWS:-$HOME/.cache/iunctus/test-workflows/nf-core-text}"
	grep -v '^#' nextflow-parser/tests/fixtures/nf_core_revisions.txt | while read -r name branch commit; do
		if [ "$name" = modules ]; then target="$root/modules"; else target="$root/pipelines/$name"; fi
		[ -d "$target" ] && continue
		work="$(mktemp -d)"
		git -C "$work" init -q
		git -C "$work" remote add origin "https://github.com/nf-core/$name.git"
		git -C "$work" fetch -q --depth 1 --filter=blob:limit=1m origin "$commit"
		git -C "$work" sparse-checkout set --no-cone '*.nf' '*.config' '*.groovy' 'nextflow_schema.json' '/CITATIONS.md'
		git -C "$work" checkout -q FETCH_HEAD
		# The files of assets/, bin/, conf/, docs/images/, tests/ and templates/ that runs read; files
		# over 1 MiB are not downloaded and stay empty, so that they exist.
		git -C "$work" ls-tree -r -l FETCH_HEAD | grep -E $'\t(assets|bin|conf|docs/images|tests)/|/templates/' | while IFS=$'\t' read -r meta path; do
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

# Fetches the Nextflow workflows of WorkflowHub at the versions of
# iunctus/tests/fixtures/workflowhub_revisions.txt, all their files as a clone has them, from the
# RO-Crate of each version, else from its git repository, else from the git tree that WorkflowHub
# keeps; files over 1 MiB stay empty.
workflowhub-test-workflows:
	#!/usr/bin/env bash
	set -euo pipefail
	root="${WORKFLOWHUB_TEST_WORKFLOWS:-$HOME/.cache/iunctus/test-workflows/nextflow-workflowhub}"
	mkdir -p "$root"
	# The paths of the files below a folder of the git tree of a WorkflowHub entry version.
	workflowhub_tree() {
		curl -sfL --retry 3 -H 'Accept: application/json' "$1/tree/$2" | jq -r '.tree[] | .type + " " + .path' \
			| while read -r type name; do
				if [ "$type" = tree ]; then workflowhub_tree "$1" "$name"; else printf '%s\n' "$name"; fi
			done
	}
	grep -v '^#' iunctus/tests/fixtures/workflowhub_revisions.txt | while read -r id version commit remote title; do
		target="$root/$id"
		[ -d "$target" ] && continue
		work="$(mktemp -d "$root/.fetch.XXXXXX")"
		if curl -sfL --retry 3 -o "$work.zip" "https://workflowhub.eu/workflows/$id/ro_crate?version=$version" \
			&& unzip -tqq "$work.zip" >/dev/null 2>&1; then
			unzip -qo "$work.zip" -d "$work" || true
		elif [ "$remote" != - ] && git -C "$work" init -q && git -C "$work" remote add origin "$remote" \
			&& git -C "$work" fetch -q --depth 1 --filter=blob:limit=1m origin "$commit"; then
			git -C "$work" ls-tree -r --name-only FETCH_HEAD \
				| while IFS= read -r path; do
					mkdir -p "$(dirname "$work/$path")"
					git -C "$work" cat-file blob "FETCH_HEAD:$path" > "$work/$path" 2>/dev/null || : > "$work/$path"
				done
			rm -rf "$work/.git"
		else
			rm -rf "$work"
			mkdir -p "$work"
			tree="https://workflowhub.eu/workflows/$id/git/$version"
			workflowhub_tree "$tree" "" | while IFS= read -r path; do
				mkdir -p "$(dirname "$work/$path")"
				curl -sfL --retry 3 -o "$work/$path" "$tree/raw/${path// /%20}" || rm -f "$work/$path"
			done || true
		fi
		rm -f "$work.zip"
		if [ -z "$(ls -A "$work")" ]; then
			echo "skipped $id: WorkflowHub gives none of its files" >&2
			rm -rf "$work"
			continue
		fi
		find "$work" -type f -size +1M -exec truncate -s 0 {} +
		# RO-Crates keep no file modes, and Nextflow runs the scripts of bin/ folders.
		find "$work" -type f -path '*/bin/*' -exec chmod +x {} +
		mv "$work" "$target"
	done

# Parses the nf-core test workflows, compares the result with `nextflow lint`, and lowers every
# module.
test-workflows-test:
	NF_CORE_TEST_WORKFLOWS="${NF_CORE_TEST_WORKFLOWS:-$HOME/.cache/iunctus/test-workflows/nf-core-text}" cargo test --release --locked -p nextflow-parser --test test_workflows -- --ignored
	NF_CORE_TEST_WORKFLOWS="${NF_CORE_TEST_WORKFLOWS:-$HOME/.cache/iunctus/test-workflows/nf-core-text}" cargo test --release --locked -p nextflow-evaluator --test test_workflows -- --ignored

# Records `nextflow config -flat -profile test` of every nf-core pipeline for the config test, four
# pipelines at a time.
config-oracle:
	#!/usr/bin/env bash
	set -euo pipefail
	test_workflows="${NF_CORE_TEST_WORKFLOWS:-$HOME/.cache/iunctus/test-workflows/nf-core-text}/pipelines"
	oracle="${IUNCTUS_CONFIG_ORACLE:-$HOME/.cache/iunctus/oracle/config}"
	mkdir -p "$oracle"
	find "$test_workflows" -mindepth 1 -maxdepth 1 -type d -printf '%f\n' | sort | xargs -P 4 -I{} sh -c \
		"cd '$test_workflows/{}' && nextflow config -flat -profile test . > '$oracle/{}.txt' 2> '$oracle/{}.err'; echo \$? > '$oracle/{}.exit'"

# Compares `iunctus config` with the recorded `nextflow config` of every nf-core pipeline.
config-test-workflows-test:
	NF_CORE_TEST_WORKFLOWS="${NF_CORE_TEST_WORKFLOWS:-$HOME/.cache/iunctus/test-workflows/nf-core-text}" \
	IUNCTUS_CONFIG_ORACLE="${IUNCTUS_CONFIG_ORACLE:-$HOME/.cache/iunctus/oracle/config}" \
	cargo test --release --locked -p iunctus --test config_test_workflows -- --ignored

# Runs every nf-core pipeline and every Nextflow workflow of WorkflowHub of the test workflows
# with the simulator and compares the outcomes.
run-test-workflows-test:
	NF_CORE_TEST_WORKFLOWS="${NF_CORE_TEST_WORKFLOWS:-$HOME/.cache/iunctus/test-workflows/nf-core-text}" \
	WORKFLOWHUB_TEST_WORKFLOWS="${WORKFLOWHUB_TEST_WORKFLOWS:-$HOME/.cache/iunctus/test-workflows/nextflow-workflowhub}" \
	cargo test --release --locked -p iunctus --test run_test_workflows -- --ignored

# Describes the inputs of every Nextflow workflow and CWL document of the test workflows as a run
# does before its first task (decision 24), without network and without running a task, and
# compares the counts with their fixtures. The Argo and Snakemake test-workflows tests record the
# inputs of their workflows too.
inputs-test-workflows-test:
	NF_CORE_TEST_WORKFLOWS="${NF_CORE_TEST_WORKFLOWS:-$HOME/.cache/iunctus/test-workflows/nf-core-text}" \
	WORKFLOWHUB_TEST_WORKFLOWS="${WORKFLOWHUB_TEST_WORKFLOWS:-$HOME/.cache/iunctus/test-workflows/nextflow-workflowhub}" \
	cargo test --release --locked -p iunctus --test inputs_test_workflows -- --ignored --exact describes_workflow_inputs
	CWL_TEST_WORKFLOWS="${CWL_TEST_WORKFLOWS:-$HOME/.cache/iunctus/test-workflows/cwl}" cargo test --release --locked -p cwl-runtime --test inputs_test_workflows -- --ignored

# Records the stub scripts of the pipelines in iunctus/tests/fixtures/oracle/stub with Nextflow
# and Docker; give the pipelines as arguments, like `just stub-oracle demo`.
stub-oracle +pipelines:
	iunctus/tests/fixtures/oracle/stub/record.sh "${NF_CORE_TEST_WORKFLOWS:-$HOME/.cache/iunctus/test-workflows/nf-core-text}/pipelines" {{pipelines}}

# Runs the recorded pipelines with -stub and the simulator and compares the task scripts.
stub-test-workflows-test:
	NF_CORE_TEST_WORKFLOWS="${NF_CORE_TEST_WORKFLOWS:-$HOME/.cache/iunctus/test-workflows/nf-core-text}" \
	cargo test --release --locked -p iunctus --test stub_test_workflows -- --ignored

# Fetches the text-only CWL test workflows at the revisions the CWL test-workflows test expects.
cwl-test-workflows:
	#!/usr/bin/env bash
	set -euo pipefail
	root="${CWL_TEST_WORKFLOWS:-$HOME/.cache/iunctus/test-workflows/cwl}"
	fixtures="$PWD/cwl-parser/tests/fixtures/test-workflows"
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
		# The test workflows leave out files over 1 MiB, as their first download did.
		find "$work" -name .git -prune -o -type f -size +1M -print0 | xargs -r -0 rm -f
		mkdir -p "$(dirname "$target")"
		rmdir "$target" 2>/dev/null || true
		mv "$work" "$target"
	done

# Loads all CWL test workflows and compares the result with `cwltool --validate`.
cwl-test-workflows-test:
	CWL_TEST_WORKFLOWS="${CWL_TEST_WORKFLOWS:-$HOME/.cache/iunctus/test-workflows/cwl}" cargo test --release --locked -p cwl-parser --test test_workflows -- --ignored

# Fetches the data files of the CWL conformance tests (inputs like `whale.txt`), which the
# text-only test workflows leave out, into a folder of their own, so that the test workflows stay
# as `cwltool --validate` saw them. The trees come from the checkouts of the test workflows, the
# files from raw.githubusercontent.com; files over 1 MiB are left out, as in the test workflows.
cwl-conformance-data:
	#!/usr/bin/env -S uv run -q --with dulwich python3
	import os, subprocess, urllib.parse
	from dulwich.object_store import iter_tree_contents
	from dulwich.repo import Repo
	home = os.path.expanduser("~/.cache/iunctus/test-workflows")
	test_workflows = os.environ.get("CWL_TEST_WORKFLOWS") or f"{home}/cwl"
	data = os.environ.get("CWL_CONFORMANCE_DATA") or f"{home}/cwl-conformance-data"
	for line in open("cwl-parser/tests/fixtures/test-workflows/revisions.txt"):
	    if line.startswith("#") or not line.startswith("conformance/"):
	        continue
	    path, url, ref, commit, *prefixes = line.split()
	    folders = [prefix for prefix in prefixes if prefix.endswith("/")]
	    repo = Repo(os.path.join(test_workflows, path))
	    for entry in iter_tree_contents(repo.object_store, repo[commit.encode()].tree):
	        name = entry.path.decode()
	        target = os.path.join(data, path, name)
	        if not any(name.startswith(folder) for folder in folders):
	            continue
	        if os.path.lexists(os.path.join(test_workflows, path, name)) or os.path.lexists(target):
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

# Runs every CWL conformance test through the driver and the simulator and compares the
# outcomes with the recorded ones; `just cwl-conformance-data` fetches the test data first.
cwl-conformance-test:
	CWL_TEST_WORKFLOWS="${CWL_TEST_WORKFLOWS:-$HOME/.cache/iunctus/test-workflows/cwl}" \
	CWL_CONFORMANCE_DATA="${CWL_CONFORMANCE_DATA:-$HOME/.cache/iunctus/test-workflows/cwl-conformance-data}" \
	cargo test --release --locked -p cwl-runtime --test conformance -- --ignored

# Builds the execution graph of every CWL test workflow that cwltool validates, with
# placeholder inputs, and compares the outcomes with the recorded ones.
cwl-graph-test-workflows-test:
	CWL_TEST_WORKFLOWS="${CWL_TEST_WORKFLOWS:-$HOME/.cache/iunctus/test-workflows/cwl}" cargo test --release --locked -p cwl-runtime --test graph_test_workflows -- --ignored

# Runs the real-world CWL test workflows with their example job files, with the
# simulator, and compares the outcomes with cwl-runtime/tests/fixtures/job_test_workflows.txt.
cwl-job-test-workflows-test:
	CWL_TEST_WORKFLOWS="${CWL_TEST_WORKFLOWS:-$HOME/.cache/iunctus/test-workflows/cwl}" cargo test --release --locked -p cwl-runtime --test job_test_workflows -- --ignored

# Fetches the text-only Snakemake test workflows at the commits their test expects.
snakemake-test-workflows:
	#!/usr/bin/env bash
	set -euo pipefail
	root="${SNAKEMAKE_TEST_WORKFLOWS:-$HOME/.cache/iunctus/test-workflows/snakemake}"
	grep -v '^#' snakemake-parser/tests/fixtures/test_workflows_revisions.txt | while read -r repository branch commit; do
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

# Adds the files that Snakemake runs read while they plan to the folders of `just snakemake-test-workflows`:
# text files up to 1 MiB of each workflow (config files, sample sheets, schemas, helper modules,
# and files without an extension, like Snakefiles and sample maps)
# and every file up to 1 MiB of the Snakemake tests (inputs and expected results). Other files
# are made empty, so that they exist. A folder is done once it holds `.iunctus-data`.
snakemake-test-workflows-data:
	#!/usr/bin/env bash
	set -euo pipefail
	root="${SNAKEMAKE_TEST_WORKFLOWS:-$HOME/.cache/iunctus/test-workflows/snakemake}"
	grep -v '^#' snakemake-parser/tests/fixtures/test_workflows_revisions.txt | while read -r repository branch commit; do
		if [ "$repository" = snakemake/snakemake ]; then
			target="$root/snakemake-tests"
			scope='^tests/'
			text='.'
		else
			target="$root/catalog/${repository/\//__}"
			scope='.'
			text='\.(ya?ml|json|tsv|csv|txt|py|cfg|ini|toml|tab|list|fai|bed|smk|snakefile)$|(^|/)[^/.]+$'
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

# Parses all Snakemake test workflows and compares the result with Snakemake's parser.py.
snakemake-test-workflows-test:
	SNAKEMAKE_TEST_WORKFLOWS="${SNAKEMAKE_TEST_WORKFLOWS:-$HOME/.cache/iunctus/test-workflows/snakemake}" cargo test --release --locked -p snakemake-parser --test test_workflows -- --ignored --nocapture

# Runs every Snakemake test workflow with the simulator and compares the outcomes
# with snakemake-runtime/tests/fixtures/run_test_workflows.txt; `UPDATE_FIXTURE=1` rewrites the fixture.
snakemake-run-test-workflows-test:
	SNAKEMAKE_TEST_WORKFLOWS="${SNAKEMAKE_TEST_WORKFLOWS:-$HOME/.cache/iunctus/test-workflows/snakemake}" cargo test --release --locked -p snakemake-runtime --test run_test_workflows -- --ignored --exact matches_recorded_workflows

# Fetches the text-only Argo test workflows at the commits the Argo test-workflows test
# expects. The checkouts keep their `.git`, which marks the repository whose templates a workflow
# may reference.
argo-test-workflows:
	#!/usr/bin/env bash
	set -euo pipefail
	root="${ARGO_TEST_WORKFLOWS:-$HOME/.cache/iunctus/test-workflows/argo}"
	mkdir -p "$root"
	grep -v '^#' argo-parser/tests/fixtures/test-workflows/revisions.txt | while read -r repository branch commit patterns; do
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

# Runs every Argo document of the test workflows that has an entrypoint with the simulator, checks
# the others as Argo does when they are created, and compares the outcomes with
# argo-runtime/tests/fixtures/test-workflows/outcomes.txt.
argo-test-workflows-test:
	ARGO_TEST_WORKFLOWS="${ARGO_TEST_WORKFLOWS:-$HOME/.cache/iunctus/test-workflows/argo}" cargo test --release --locked -p argo-runtime --test test_workflows -- --ignored --nocapture

# Runs Funnel (a GA4GH TES server) from ~/.cache/iunctus/tools/funnel in the foreground, at
# http://localhost:18000, with the config of the TES tests: tasks get a memory limit and no
# network, and images are never pulled. It may read and write /tmp and the cache of iunctus.
funnel-server:
	mkdir -p /tmp/iunctus-funnel
	cd /tmp/iunctus-funnel && ~/.cache/iunctus/tools/funnel/funnel server run \
		--config {{justfile_directory()}}/tes-executor/tests/fixtures/funnel.yaml \
		--BoltDB.Path /tmp/iunctus-funnel/funnel.db --Worker.WorkDir /tmp/iunctus-funnel/work \
		--LocalStorage.AllowedDirs /tmp --LocalStorage.AllowedDirs "$HOME/.cache/iunctus"

# Runs the TES tests against the server from `just funnel-server`.
tes-test:
	IUNCTUS_TES_SERVER=http://localhost:18000 cargo test -p tes-executor --test funnel -- --ignored

# Qualifies object staging with isolated MinIO, Funnel and Docker, then removes its resources.
tes-qualification *arguments:
	python3 -I scripts/tes-qualification.py {{arguments}}

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

# Runs the S3 tests of content-store and of object-store staging against the server from
# `just s3-server`.
s3-test:
	AWS_ACCESS_KEY_ID=rustfsadmin AWS_SECRET_ACCESS_KEY=rustfsadmin AWS_REGION=us-east-1 \
	IUNCTUS_S3_BUCKET=iunctus-test IUNCTUS_S3_ENDPOINT=http://127.0.0.1:9000 \
	cargo test -p content-store --test store -- --ignored
	AWS_ACCESS_KEY_ID=rustfsadmin AWS_SECRET_ACCESS_KEY=rustfsadmin AWS_REGION=us-east-1 \
	IUNCTUS_S3_BUCKET=iunctus-test IUNCTUS_S3_ENDPOINT=http://127.0.0.1:9000 \
	IUNCTUS_STAGING_STORAGE='storage.test = { type = "s3", prefixes = ["s3://iunctus-test/"], endpoint = "http://127.0.0.1:9000", region = "us-east-1", access_key_id = "rustfsadmin", secret_access_key = "rustfsadmin" }' \
	cargo test -p executor --lib -- --ignored presigned_round_trip credentials_round_trip sends_large_outputs \
		presigns_binary_download

# Runs the tests of presigned requests of object-store staging against local emulators of
# Google Cloud Storage (fake-gcs-server) and Azure Blob Storage (Azurite), which stop at the end.
emulator-test:
	#!/usr/bin/env bash
	set -euo pipefail
	work="$(mktemp -d)"
	trap 'docker stop iunctus-gcs iunctus-azurite > /dev/null 2>&1 || true; rm -rf "$work"' EXIT
	docker run -d --rm --name iunctus-gcs -p 127.0.0.1:4443:4443 fsouza/fake-gcs-server:1.56.1 \
		-scheme http -port 4443 -public-host 127.0.0.1:4443 > /dev/null
	docker run -d --rm --name iunctus-azurite -p 127.0.0.1:10000:10000 \
		mcr.microsoft.com/azure-storage/azurite azurite-blob --blobHost 0.0.0.0 --skipApiVersionCheck \
		> /dev/null
	until curl -sf -X POST -H 'Content-Type: application/json' -d '{"name": "iunctus-test"}' \
		'http://127.0.0.1:4443/storage/v1/b?project=test' > /dev/null; do sleep 1; done
	until curl -s http://127.0.0.1:10000/ > /dev/null; do sleep 1; done
	# A made-up service account signs the requests to Google Cloud Storage, which the emulator
	# does not check; Azurite checks the shared access signature of its well-known account.
	openssl genrsa -out "$work/key.pem" 2048 2> /dev/null
	python3 -I -c 'import json, sys; json.dump({"type": "service_account", "project_id": "test", "private_key_id": "1", "private_key": open(sys.argv[1] + "/key.pem").read(), "client_email": "test@test.iam.gserviceaccount.com", "client_id": "1", "token_uri": "http://127.0.0.1:4443/token"}, open(sys.argv[1] + "/account.json", "w"))' "$work"
	sas="$(uv run -q --with azure-storage-blob python -I -c 'import datetime; from azure.storage.blob import BlobServiceClient, ContainerSasPermissions, generate_container_sas; key = "Eby8vdM02xNOcqFlqUwJPLlmEtlCDXJ1OUzFT50uSRZ6IFsuFq2UVErCz4I6tq/K1SZFPTOtr/KBHBeksoGMGw=="; BlobServiceClient("http://127.0.0.1:10000/devstoreaccount1", {"account_name": "devstoreaccount1", "account_key": key}).create_container("iunctus-test"); print(generate_container_sas("devstoreaccount1", "iunctus-test", account_key=key, permission=ContainerSasPermissions(read=True, add=True, create=True, write=True, delete=True, list=True), expiry=datetime.datetime.now(datetime.timezone.utc) + datetime.timedelta(days=1)))')"
	export IUNCTUS_GCS_ROOT=gs://iunctus-test/staging IUNCTUS_AZURE_ROOT=az://iunctus-test/staging
	export IUNCTUS_TEST_STORAGE="[storage.gcs]
	type = \"google-cloud-storage\"
	prefixes = [\"gs://iunctus-test/\"]
	endpoint = \"http://127.0.0.1:4443\"
	credential_file = \"$work/account.json\"
	[storage.azure]
	type = \"azure-blob-storage\"
	prefixes = [\"az://iunctus-test/\"]
	account = \"devstoreaccount1\"
	endpoint = \"http://127.0.0.1:10000/devstoreaccount1\"
	sas_token = \"$sas\""
	cargo test --locked -p executor --lib -- --ignored gcs_presigned_requests azure_round_trip

# Runs the tests of two S3 storage providers against two local RustFS servers, one for inputs
# (bucket `inputs`) and one for the store (bucket `cache`), each with its own credentials, and
# a real run with Docker that reads its inputs from the first and keeps its store on the
# second. The servers use only the local image and stop at the end.
storage-test:
	#!/usr/bin/env bash
	set -euo pipefail
	export IUNCTUS_TEST_INPUTS_ENDPOINT=http://127.0.0.1:9100 IUNCTUS_TEST_INPUTS_KEY=inputs-user \
		IUNCTUS_TEST_INPUTS_SECRET=inputs-secret-5031 IUNCTUS_TEST_STORE_ENDPOINT=http://127.0.0.1:9101 \
		IUNCTUS_TEST_STORE_KEY=store-user IUNCTUS_TEST_STORE_SECRET=store-secret-8264
	trap 'docker stop iunctus-storage-inputs iunctus-storage-store >/dev/null 2>&1 || true' EXIT
	serve() {
		docker run -d --rm --pull=never --name "iunctus-storage-$1" --memory 1g -p "127.0.0.1:$2:9000" \
			-e RUSTFS_ACCESS_KEY="$3" -e RUSTFS_SECRET_KEY="$4" rustfs/rustfs:latest >/dev/null
		# The server answers its health check before it takes requests, so the bucket is made
		# once it can be.
		for _ in $(seq 60); do
			curl -sf --aws-sigv4 aws:amz:us-east-1:s3 --user "$3:$4" -X PUT \
				"http://127.0.0.1:$2/$5" >/dev/null 2>&1 && return
			sleep 1
		done
		echo "the RustFS server $1 did not start" >&2
		return 1
	}
	serve inputs 9100 "$IUNCTUS_TEST_INPUTS_KEY" "$IUNCTUS_TEST_INPUTS_SECRET" inputs
	serve store 9101 "$IUNCTUS_TEST_STORE_KEY" "$IUNCTUS_TEST_STORE_SECRET" cache
	cargo test --locked -p storage --test s3 -- --ignored
	cargo test --locked -p iunctus --test storage -- --ignored
