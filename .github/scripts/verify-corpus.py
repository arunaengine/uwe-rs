"""Verify the versioned bundle before explicitly invoking external corpus gates."""
# Copyright (c) 2026 The iunctus Contributors
# SPDX-License-Identifier: MIT or Apache-2.0

import hashlib
import json
import pathlib
import sys


def digest(path):
    with path.open("rb") as source:
        return hashlib.file_digest(source, "sha256").hexdigest()


root = pathlib.Path(sys.argv[1])
manifest = json.loads((root / "manifest.json").read_text())
if manifest.get("format") != 1 or manifest.get("nextflow_version") != "26.04.6":
    raise SystemExit("requires bundle format 1 and Nextflow 26.04.6 oracle")
fixtures = [
    "nextflow-parser/tests/fixtures/nf_core_revisions.txt",
    "snakemake-parser/tests/fixtures/test_workflows_revisions.txt",
    "conda-environment/tests/fixtures/workflow_rejections.json",
]
expected_fixtures = {name: digest(pathlib.Path(name)) for name in fixtures}
if manifest.get("fixtures") != expected_fixtures:
    raise SystemExit("bundle fixture identities do not match the checked-out revision")
expected = manifest.get("files")
if not isinstance(expected, dict) or not expected:
    raise SystemExit("bundle has no complete file inventory")
actual = {
    path.relative_to(root).as_posix(): digest(path)
    for path in root.rglob("*")
    if path.is_file() and path != root / "manifest.json"
}
if actual != expected:
    raise SystemExit("bundle file inventory or content digest mismatch")
for folder in ["nf-core-text/pipelines", "config-oracle", "snakemake"]:
    if not (root / folder).is_dir():
        raise SystemExit(f"missing corpus prerequisite: {folder}")
pipelines = sorted(
    line.split()[0]
    for line in pathlib.Path(fixtures[0]).read_text().splitlines()
    if line and not line.startswith("#") and line.split()[0] != "modules"
)
if sorted(path.name for path in (root / "nf-core-text/pipelines").iterdir()) != pipelines:
    raise SystemExit("bundle does not contain the exact pinned pipeline inventory")
for name in pipelines:
    for suffix in ["exit", "txt", "err"]:
        if not (root / "config-oracle" / f"{name}.{suffix}").is_file():
            raise SystemExit(f"missing oracle outcome: {name}.{suffix}")
print(f"Verified {len(actual)} bundle files and {len(pipelines)} pinned pipelines")
