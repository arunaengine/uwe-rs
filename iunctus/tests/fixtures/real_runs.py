#!/usr/bin/env python3
# Runs workflows for real, with iunctus and with the native tool, and compares what each
# delivers: the names and the SHA-256 digests of the result files. It records the comparison of
# each case in real_runs.txt next to this script.
#
# Run it from the repository root: iunctus/tests/fixtures/real_runs.py [case...]
# With cases, it runs only them and keeps the other records. It needs Docker, the release
# binary of iunctus (target/release/iunctus, or IUNCTUS), and the native tool of each case on
# the PATH: nextflow 26.04.6 for Nextflow, cwltool for CWL, snakemake (through uvx) for
# Snakemake. Argo has no native tool without a cluster: its cases compare with the files that
# the workflow documents. Each case runs in ~/.cache/iunctus/real-runs/<case> (REAL_RUNS).
#
# The native tool runs twice, into two folders, so that the files whose content differs
# between two native runs, like reports with dates, are compared by their names only. Names
# with a date and time, like `params_2026-09-30_12-02-36.json`, are compared with the date and
# time replaced by `<time>`. The native runs are kept and not repeated; `NATIVE=1` runs them
# again. At most 4 containers run at a time: Nextflow gets `executor.cpus = 4`, iunctus
# `-max-pending 4`.
#
# The cases that end in `-slurm` run on a SLURM cluster with Apptainer: run the script on a node
# that may submit jobs, not on a login node, with nextflow, java and apptainer on the PATH.
# Nextflow gets `queueSize = 4` and names its jobs `iunctus-native-...`, iunctus `-max-pending 4`.
#
# Copyright (c) 2026 The iunctus Contributors
# SPDX-License-Identifier: MIT or Apache-2.0

import hashlib
import json
import os
import re
import shutil
import subprocess
import sys
from pathlib import Path

ROOT = Path(os.environ.get("REAL_RUNS") or Path.home() / ".cache/iunctus/real-runs")
IUNCTUS = Path(os.environ.get("IUNCTUS") or "target/release/iunctus").resolve()
OUTPUT = Path(__file__).with_name("real_runs.txt")
TIMEOUT = 3600

# The config of the native Nextflow runs: at most 4 CPUs of tasks at once.
NEXTFLOW_LIMITS = """executor {
    cpus = 4
    memory = '16 GB'
}
"""

# Each case names its workflow, how to fetch it (`repository` at `revision` into
# `<ROOT>/sources/<name>`), and the commands of the native tool and of iunctus, where
# `{workflow}` is the fetched folder, `{out}` the folder of the results, and `{limits}` the
# config of the native Nextflow runs. `results` is the folder to compare, `{out}` unless
# given. `notes` says in plain words what differs on purpose.
# The config of the native Nextflow runs on SLURM: at most 4 jobs at once, named so that they
# can be told from other jobs of the user.
NEXTFLOW_SLURM = """process.executor = 'slurm'
executor {
    queueSize = 4
    jobName = { "iunctus-native-" + task.hash.toString().substring(0, 8) }
}
"""

CASES = [
    {
        "name": "nf-core-demo",
        "title": "nf-core/demo 555d145, -profile test",
        "repository": "https://github.com/nf-core/demo.git",
        "revision": "555d145",
        "native": ["nextflow", "run", "{workflow}", "-profile", "test,docker",
                   "--outdir", "{out}", "-c", "{limits}"],
        "iunctus": ["{iunctus}", "run", "{workflow}", "-profile", "test,docker",
                    "--outdir", "{out}", "-executor", "local", "-max-pending", "4"],
        "notes": "Nextflow's own reports (execution_report, execution_timeline, "
                 "execution_trace, pipeline_dag) are not built yet.",
    },
    {
        "name": "nf-core-demo-slurm",
        "title": "nf-core/demo 555d145, -profile test,apptainer, on SLURM",
        "repository": "https://github.com/nf-core/demo.git",
        "revision": "555d145",
        "limits": NEXTFLOW_SLURM,
        "native": ["nextflow", "run", "{workflow}", "-profile", "test,apptainer",
                   "--outdir", "{out}", "-c", "{limits}"],
        "iunctus": ["{iunctus}", "run", "{workflow}", "-profile", "test,apptainer",
                    "--outdir", "{out}", "-executor", "slurm", "-max-pending", "4"],
        "notes": "pipeline_dag is the graph page of iunctus, not the Mermaid page of Nextflow.",
    },
]

TIME = re.compile(r"\d{4}-\d{2}-\d{2}_\d{2}-\d{2}-\d{2}")


def fetch(case):
    """The folder of the workflow of a case, cloned at its revision once."""
    folder = ROOT / "sources" / case["name"]
    if not folder.exists():
        folder.parent.mkdir(parents=True, exist_ok=True)
        subprocess.run(["git", "clone", "-q", case["repository"], str(folder)], check=True)
    subprocess.run(["git", "-C", str(folder), "checkout", "-q", case["revision"]], check=True)
    return folder


def run(case, command, folder):
    """Runs a command of a case in a fresh folder; the result has the exit code."""
    if folder.exists():
        shutil.rmtree(folder)
    folder.mkdir(parents=True)
    limits = folder / "limits.config"
    limits.write_text(case.get("limits", NEXTFLOW_LIMITS))
    values = {
        "workflow": str(fetch(case)),
        "out": str(folder / "out"),
        "limits": str(limits),
        "iunctus": str(IUNCTUS),
    }
    words = [word.format(**values) for word in command]
    environment = dict(os.environ, NXF_ANSI_LOG="false")
    with open(folder / "run.log", "w") as log:
        try:
            done = subprocess.run(words, cwd=folder, stdout=log, stderr=subprocess.STDOUT,
                                  env=environment, timeout=TIMEOUT)
            return done.returncode
        except subprocess.TimeoutExpired:
            return "timeout"


def manifest(folder):
    """Each file below a folder by its name, with dates and times in names replaced, and its
    digest. Links count as the files they point to."""
    files = {}
    if not folder.exists():
        return files
    for path in sorted(folder.rglob("*")):
        if path.is_dir():
            continue
        name = TIME.sub("<time>", str(path.relative_to(folder)))
        digest = hashlib.sha256()
        try:
            with open(path, "rb") as file:
                for part in iter(lambda: file.read(1 << 20), b""):
                    digest.update(part)
            files[name] = digest.hexdigest()
        except OSError as error:
            files[name] = f"unreadable: {error}"
    return files


def native_manifests(case):
    """The manifests of the two native runs, run unless recorded before."""
    manifests = []
    for number in (1, 2):
        folder = ROOT / case["name"] / f"native-{number}"
        record = folder / "manifest.json"
        if record.exists() and not os.environ.get("NATIVE"):
            manifests.append(json.loads(record.read_text()))
            continue
        code = run(case, case["native"], folder)
        results = Path(case.get("results", "{out}").format(out=folder / "out"))
        files = {"exit": code, "files": manifest(results)}
        record.write_text(json.dumps(files, indent=1))
        manifests.append(files)
    return manifests


def compare(case):
    """The record of a case: how many files match, and the names of those that do not."""
    first, second = native_manifests(case)
    folder = ROOT / case["name"] / "iunctus"
    code = run(case, case["iunctus"], folder)
    results = Path(case.get("results", "{out}").format(out=folder / "out"))
    ours = manifest(results)
    native = first["files"]
    varying = {name for name in native if second["files"].get(name) != native[name]}
    matching = [name for name in native if name not in varying and ours.get(name) == native[name]]
    differing = [name for name in native
                 if name not in varying and name in ours and ours[name] != native[name]]
    missing = [name for name in native if name not in ours]
    extra = [name for name in ours if name not in native]
    named = [name for name in varying if name in ours]
    lines = [
        f"## {case['name']}: {case['title']}",
        f"native: exit {first['exit']} and {second['exit']}, {len(native)} files, "
        f"{len(varying)} of them differ between two native runs",
        f"iunctus: exit {code}, {len(ours)} files: {len(matching)} with the same digest, "
        f"{len(named)} of the varying ones by name, {len(differing)} differ, "
        f"{len(missing)} missing, {len(extra)} extra",
    ]
    if case.get("notes"):
        lines.append(f"notes: {case['notes']}")
    for label, names in (("differs", differing), ("missing", missing), ("extra", extra)):
        lines.extend(f"{label}: {name}" for name in names)
    return "\n".join(lines) + "\n"


def main():
    wanted = sys.argv[1:]
    unknown = [name for name in wanted if name not in {case["name"] for case in CASES}]
    if unknown:
        sys.exit(f"unknown cases: {', '.join(unknown)}")
    records = {}
    if OUTPUT.exists():
        for block in OUTPUT.read_text().split("\n## ")[1:]:
            records[block.split(":", 1)[0]] = "## " + block.rstrip("\n") + "\n"
    for case in CASES:
        if not wanted or case["name"] in wanted:
            records[case["name"]] = compare(case)
            print(records[case["name"]], end="", flush=True)
    header = ("# Real runs of iunctus against the native tool, by the names and digests of the "
              "delivered files; written by real_runs.py.\n")
    text = header + "".join("\n" + records[case["name"]] for case in CASES
                            if case["name"] in records)
    OUTPUT.write_text(text)


if __name__ == "__main__":
    main()
