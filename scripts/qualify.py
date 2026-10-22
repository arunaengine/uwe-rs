#!/usr/bin/env python3
"""Measure simulated fanout/gather planning with deterministic shared reference inputs."""
# Copyright (c) 2026 The uwe Contributors
# SPDX-License-Identifier: MIT or Apache-2.0

import argparse
import hashlib
import json
import os
from pathlib import Path
import platform
import subprocess
import tempfile
import time


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("binary", type=Path)
    parser.add_argument("--tasks", type=positive, nargs="+", default=[100, 1000])
    parser.add_argument("--files", type=positive, default=8)
    parser.add_argument("--file-bytes", type=positive, default=1048576)
    parser.add_argument("--repeats", type=positive, default=2)
    parser.add_argument("--timeout", type=positive, default=120)
    parser.add_argument("--output", type=Path, default=Path("target/qualification"))
    args = parser.parse_args()
    binary = args.binary.resolve(strict=True)
    args.output.mkdir(parents=True, exist_ok=True)
    root = Path(tempfile.mkdtemp(prefix="scale-", dir=args.output)).resolve()
    reference = root / "reference"
    digest = make_reference(reference, args.files, args.file_bytes)
    report = {
        "format": 1, "complete": False,
        "scope": "simulator planning and graph qualification; no scientific tools executed",
        "cache": "fresh uwe cache per sample; operating-system page cache uncontrolled",
        "limitations": "no measured copy/hash/transfer bytes, peak RSS, real task throughput or upstream comparison",
        "binary": str(binary), "binary_sha256": file_digest(binary),
        "version": subprocess.check_output([str(binary), "--version"], text=True).strip(),
        "platform": platform.platform(), "cpu_count": os.cpu_count(),
        "reference_files": args.files, "reference_bytes": args.files * args.file_bytes,
        "reference_sha256": digest, "samples": [],
    }
    output = root / "results.json"
    output.write_text(json.dumps(report, indent=2) + "\n")
    for count in args.tasks:
        for repeat in range(args.repeats):
            folder = root / f"tasks-{count}-{repeat}"
            folder.mkdir()
            workflow = folder / "main.nf"
            workflow.write_text(source(count))
            command = [str(binary), "graph", str(workflow), "--reference", str(reference),
                       "-format", "json", "-no-cache", "-max-pending", "64"]
            environment = {key: value for key, value in os.environ.items() if not key.startswith("UWE_")}
            environment.update(XDG_CONFIG_HOME=str(folder / "config"),
                               XDG_CACHE_HOME=str(folder / "cache"), UWE_SITE_CONFIG="")
            with (folder / "graph.json").open("w") as graph, (folder / "stderr.txt").open("w") as error:
                started = time.monotonic()
                subprocess.run(command, cwd=folder, env=environment, stdout=graph, stderr=error,
                               timeout=args.timeout, check=True)
                elapsed = time.monotonic() - started
            graph = json.loads((folder / "graph.json").read_text())
            sample = counts(graph, count)
            sample.update(tasks=count, repeat=repeat, seconds=elapsed, command=command,
                          workflow_sha256=file_digest(workflow), graph_bytes=(folder / "graph.json").stat().st_size)
            report["samples"].append(sample)
            output.write_text(json.dumps(report, indent=2) + "\n")
    report["complete"] = True
    output.write_text(json.dumps(report, indent=2) + "\n")
    print(output)


def positive(value):
    number = int(value)
    if number <= 0:
        raise argparse.ArgumentTypeError("must be above zero")
    return number


def file_digest(path):
    with path.open("rb") as handle:
        return hashlib.file_digest(handle, "sha256").hexdigest()


def make_reference(folder, count, size):
    folder.mkdir()
    identity = hashlib.sha256()
    for index in range(count):
        block = hashlib.sha256(str(index).encode()).digest() * 32768
        path = folder / f"part-{index:06}.bin"
        with path.open("wb") as handle:
            remaining = size
            while remaining:
                chunk = block[:min(remaining, len(block))]
                handle.write(chunk)
                remaining -= len(chunk)
        identity.update(f"{path.name}\0{size}\0{file_digest(path)}\n".encode())
    return identity.hexdigest()


def source(count):
    values = ", ".join(str(index) for index in range(count))
    return '''process FANOUT {
    container 'debian:stable-slim'
    input:
    val index
    path reference
    output:
    path "part_${index}.txt"
    script:
    """
    echo ${index} > part_${index}.txt
    """
}
process GATHER {
    container 'debian:stable-slim'
    input:
    path parts
    output:
    path 'joined.txt'
    script:
    """
    cat ${parts} > joined.txt
    """
}
workflow {
    FANOUT(Channel.of(''' + values + '''), file(params.reference))
    GATHER(FANOUT.out.collect())
}
'''


def counts(graph, count):
    nodes = graph["nodes"]
    fanout = [node for node in nodes if node["task"]["step"] == "FANOUT"]
    gather = [node for node in nodes if node["task"]["step"] == "GATHER"]
    references = sum(item["directory"] for node in fanout for item in node["task"]["inputs"])
    if not graph.get("simulation") or len(nodes) != count + 1 or len(fanout) != count or len(gather) != 1:
        raise ValueError("unexpected simulated fanout/gather node counts")
    target = gather[0]["task"]["id"]
    sources = {node["task"]["id"] for node in fanout}
    incoming = {edge["from"] for edge in graph["edges"] if edge["to"] == target}
    if incoming != sources or references != count:
        raise ValueError("missing gather dependencies or shared reference inputs")
    return {"nodes": len(nodes), "edges": len(graph["edges"]),
            "gather_sources": len(incoming), "reference_bindings": references}


if __name__ == "__main__":
    main()
