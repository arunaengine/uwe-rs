#!/usr/bin/env python3
"""Qualify TES object staging against isolated Funnel, MinIO and Docker services."""
# Copyright (c) 2026 The iunctus Contributors
# SPDX-License-Identifier: MIT or Apache-2.0

import argparse
import hashlib
import json
import os
from pathlib import Path
import re
import shutil
import signal
import socket
import subprocess
import tarfile
import tempfile
import time
import urllib.request


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--binary", type=Path, default=Path("target/debug/iunctus"))
    parser.add_argument("--funnel", type=Path)
    parser.add_argument("--minio", type=Path)
    parser.add_argument("--report", type=Path)
    arguments = parser.parse_args()
    repository = Path(__file__).resolve().parent.parent
    selected = arguments.binary.resolve(strict=True)
    report = {"versions": {}, "checks": []}
    report["versions"]["revision"] = command(["git", "-C", repository, "rev-parse", "HEAD"]).stdout.strip()
    report["versions"]["docker"] = command(["docker", "version", "--format", "{{.Server.Version}}"]).stdout.strip()
    with tempfile.TemporaryDirectory(prefix="iunctus-tes-") as temporary:
        root = Path(temporary)
        binary = root / "iunctus"
        shutil.copy2(selected, binary)
        with binary.open("rb") as stream:
            report["versions"]["binary_sha256"] = hashlib.file_digest(stream, "sha256").hexdigest()
        services = []
        try:
            funnel = arguments.funnel or download_funnel(root)
            minio = arguments.minio or download_minio(root)
            report["versions"]["funnel"] = command([funnel, "version"]).stdout.strip()
            report["versions"]["minio"] = command([minio, "--version"]).stdout.strip()
            environment = dict(os.environ, IUNCTUS_SITE_CONFIG="", XDG_CONFIG_HOME=str(root / "config"),
                               MINIO_ROOT_USER="qualification", MINIO_ROOT_PASSWORD="qualification-secret")
            endpoint = f"http://127.0.0.1:{free_port()}"
            server = f"http://127.0.0.1:{free_port()}"
            start_service([minio, "server", root / "data", "--address", endpoint.split("//")[1],
                           "--console-address", f"127.0.0.1:{free_port()}"], root / "minio.log", environment, services)
            wait_ready(endpoint + "/minio/health/live")
            command(["curl", "-fsS", "--aws-sigv4", "aws:amz:us-east-1:s3", "--user",
                     "qualification:qualification-secret", "-X", "PUT", endpoint + "/qualification"])
            config = root / "funnel.yaml"
            config.write_text(f"""Server:
  HostName: localhost
  HTTPPort: "{server.rsplit(':', 1)[1]}"
  RPCPort: "{free_port()}"
Compute: local
BoltDB:
  Path: {root}/funnel.db
Node:
  Resources:
    Cpus: 4
    RamGb: 4
AmazonS3:
  Disabled: true
GenericS3:
  - Endpoint: {endpoint}
    Key: qualification
    Secret: qualification-secret
    Region: us-east-1
    Bucket: qualification
GoogleStorage:
  Disabled: true
Swift:
  Disabled: true
HTTPStorage:
  Disabled: true
FTPStorage:
  Disabled: true
Worker:
  WorkDir: {root}/workers
  Container:
    DriverCommand: docker
    RunCommand: run -i --pull never --memory 1g --network none --label iunctus.qualification={root.name} {{{{if .RemoveContainer}}}}--rm{{{{end}}}} {{{{.GetEnvArgs}}}} {{{{if .Name}}}}--name {{{{.Name}}}}{{{{end}}}} {{{{if .Workdir}}}}--workdir {{{{.Workdir}}}}{{{{end}}}} {{{{range .Volumes}}}}--volume {{{{.HostPath}}}}:{{{{.ContainerPath}}}}:{{{{if .Readonly}}}}ro{{{{else}}}}rw{{{{end}}}} {{{{end}}}} {{{{.Image}}}} {{{{.Command}}}}
    PullCommand: image inspect {{{{.Image}}}}
""")
            start_service([funnel, "server", "run", "--config", config], root / "funnel.log", environment, services)
            wait_ready(server + "/ga4gh/tes/v1/service-info")
            run_checks(repository, binary, root, environment, endpoint, server, report)
        finally:
            for process, stream in reversed(services):
                process.terminate()
                try:
                    process.wait(timeout=15)
                except subprocess.TimeoutExpired:
                    process.kill()
                    process.wait()
                stream.close()
            containers = command(["docker", "ps", "-aq", "--filter", f"label=iunctus.qualification={root.name}"]).stdout.split()
            if containers:
                command(["docker", "rm", "-f", *containers])
            if arguments.report:
                arguments.report.write_text(json.dumps(report, indent=2) + "\n")
            print(json.dumps(report, indent=2), flush=True)


def command(arguments, **options):
    result = subprocess.run([str(value) for value in arguments], capture_output=True, text=True, **options)
    if result.returncode:
        raise RuntimeError(f"{arguments[0]} exited {result.returncode}:\n{result.stdout}\n{result.stderr}")
    return result


def download_funnel(root):
    metadata = json.loads(command(["curl", "-fsSL", "--max-time", "30",
                                   "https://api.github.com/repos/calypr/funnel/releases/tags/v0.11.14"]).stdout)
    asset = next(value for value in metadata["assets"] if value["name"].startswith("funnel-linux-amd64-")
                 and value["name"].endswith(".tar.gz"))
    archive = root / "funnel.tar.gz"
    command(["curl", "-fsSL", "--max-time", "240", asset["browser_download_url"], "-o", archive])
    with tarfile.open(archive) as package:
        member = next(value for value in package.getmembers() if value.name in ("funnel", "./funnel"))
        (root / "funnel").write_bytes(package.extractfile(member).read())
    (root / "funnel").chmod(0o755)
    archive.unlink()
    return root / "funnel"


def download_minio(root):
    version = "RELEASE.2025-09-07T16-13-09Z"
    archive = root / "minio.deb"
    address = f"https://github.com/minio/minio/releases/download/{version}/minio_20250907161309.0.0_amd64.deb"
    command(["curl", "-fsSL", "--max-time", "240", address, "-o", archive])
    with (root / "minio.tar.gz").open("wb") as stream:
        subprocess.run(["ar", "p", str(archive), "data.tar.gz"], stdout=stream, check=True)
    with tarfile.open(root / "minio.tar.gz") as package:
        (root / "minio").write_bytes(package.extractfile("./usr/local/bin/minio").read())
    (root / "minio").chmod(0o755)
    archive.unlink()
    (root / "minio.tar.gz").unlink()
    return root / "minio"


def free_port():
    with socket.socket() as listener:
        listener.bind(("127.0.0.1", 0))
        return listener.getsockname()[1]


def start_service(arguments, path, environment, services):
    stream = path.open("w")
    process = subprocess.Popen(["nice", "-n", "19", *map(str, arguments)], stdout=stream, stderr=stream, env=environment)
    services.append((process, stream))


def request(server, path, body=None):
    payload = None if body is None else json.dumps(body).encode()
    message = urllib.request.Request(server + "/ga4gh/tes/v1" + path, data=payload,
                                     headers={"Content-Type": "application/json"})
    with urllib.request.urlopen(message, timeout=10) as response:
        return json.load(response)


def wait_ready(address):
    deadline = time.monotonic() + 30
    while time.monotonic() < deadline:
        try:
            with urllib.request.urlopen(address, timeout=1):
                return
        except OSError:
            time.sleep(0.2)
    raise RuntimeError(f"service did not become ready: {address}")


def tasks(server):
    return request(server, "/tasks?view=FULL&page_size=1000").get("tasks", [])


def run_checks(repository, binary, root, environment, endpoint, server, report):
    cases = root / "cases"
    cases.mkdir()
    for executor in ("local", "tes"):
        folder = cases / executor
        folder.mkdir()
        (folder / "snake").mkdir()
        shutil.copy2(repository / "demo/snakemake/hello.smk", folder / "snake/hello.smk")
        (folder / "nextflow").mkdir()
        shutil.copy2(repository / "demo/nextflow/main.nf", folder / "nextflow/main.nf")
        (folder / "nextflow/nextflow.config").write_text("process.container = 'debian:stable-slim'\n")
        (folder / "directory/nested").mkdir(parents=True)
        (folder / "directory/nested/value.txt").write_text("directory input\n")
        (folder / "tool.cwl").write_text("""cwlVersion: v1.2
class: CommandLineTool
baseCommand: [bash, -c]
arguments: ['mkdir out; cat "$0"/nested/value.txt > out/result.txt']
requirements:
  DockerRequirement:
    dockerPull: debian:stable-slim
  NetworkAccess:
    networkAccess: true
inputs:
  source:
    type: Directory
    inputBinding:
      position: 1
outputs:
  result:
    type: File[]
    outputBinding:
      glob: out/*.txt
""")
        (folder / "job.json").write_text(json.dumps({"source": {"class": "Directory", "path": str(folder / "directory")}}))
        config = folder / "iunctus.toml"
        config.write_text(f"""[cache]
folder = "{folder}/cache"
[executor.tes]
server = "{server}"
staging = "object-store"
staging_root = "s3://qualification/staging"
idempotency_tag = "qualification/idempotency"
[storage.qualification]
type = "s3"
prefixes = ["s3://qualification/"]
endpoint = "{endpoint}"
region = "us-east-1"
access_key_id = "qualification"
secret_access_key = "qualification-secret"
""")
        for name, workflow, extra in (("snakemake", "snake/hello.smk", []), ("nextflow", "nextflow/main.nf", []),
                                      ("cwl", "tool.cwl", ["-inputs", str(folder / "job.json"), "-output-dir", str(folder / "outputs")])):
            invocation = ["nice", "-n", "19", binary, "run", folder / workflow, "-executor", executor,
                          "-iunctus-config", config, *extra]
            known = {task["id"] for task in tasks(server)}
            result = command(invocation, cwd=folder, env=environment, timeout=240)
            (folder / f"{name}.log").write_text(result.stdout + result.stderr)
            if executor == "tes":
                created = [task for task in tasks(server) if task["id"] not in known]
                assert len(created) == {"snakemake": 5, "nextflow": 4, "cwl": 1}[name], created
                assert all(task["state"] == "COMPLETE" for task in created)
                before = len(tasks(server))
                resumed = command([*invocation, "-resume"], cwd=folder, env=environment, timeout=240)
                assert len(tasks(server)) == before, "resume submitted new TES tasks"
                assert "from the cache" in resumed.stdout + resumed.stderr
                record(report, f"{name} resume", "passed", "No new TES tasks")
    local = cases / "local"
    remote = cases / "tes"
    assert (local / "snake/hello/all.txt").read_bytes() == (remote / "snake/hello/all.txt").read_bytes()
    record(report, "Snakemake parity", "passed", "demo/snakemake/hello.smk, five tasks, identical bytes")
    pattern = rb"(?:Bonjour|Ciao|Hello|Hola) world!\n"
    expected = [b"Bonjour world!\n", b"Ciao world!\n", b"Hello world!\n", b"Hola world!\n"]
    assert sorted(re.findall(pattern, (local / "nextflow.log").read_bytes())) == expected
    assert sorted(re.findall(pattern, (remote / "nextflow.log").read_bytes())) == expected
    record(report, "Nextflow parity", "passed", "demo/nextflow/main.nf, four stdout results, Debian image override")
    left = [value.read_bytes() for value in sorted((local / "outputs").rglob("*.txt"))]
    right = [value.read_bytes() for value in sorted((remote / "outputs").rglob("*.txt"))]
    assert left == right == [b"directory input\n"]
    record(report, "CWL parity", "passed", "Directory input and wildcard output, identical bytes")
    submitted = tasks(server)
    assert len(submitted) == 10, [(task["name"], task["state"]) for task in submitted]
    for task in submitted:
        for value in task.get("inputs", []) + task.get("outputs", []):
            assert value["type"] == "FILE" and value["url"].startswith("s3://qualification/staging/")
    record(report, "Object staging", "passed", "Ten workflow tasks used only S3 FILE inputs and outputs")
    lifecycle_checks(binary, remote, environment, server, report)


def record(report, check, result, notes):
    report["checks"].append({"check": check, "result": result, "notes": notes})
    print(f"{check}: {result}: {notes}", flush=True)


def lifecycle_checks(binary, folder, environment, server, report):
    workflow = folder / "lifecycle.nf"
    config = folder / "iunctus.toml"
    base = ["nice", "-n", "19", str(binary), "run", str(workflow), "-executor", "tes", "-iunctus-config", str(config)]
    workflow.write_text("process FAIL {\n container 'debian:stable-slim'\n script:\n 'exit 17'\n}\nworkflow { FAIL() }\n")
    known = {task["id"] for task in tasks(server)}
    failure = subprocess.run(base, cwd=folder, env=environment, capture_output=True, text=True, timeout=240)
    created = [task for task in tasks(server) if task["id"] not in known]
    assert failure.returncode != 0 and len(created) == 1, (failure.returncode, created, failure.stdout, failure.stderr)
    assert created[0]["state"] == "EXECUTOR_ERROR", created[0]
    record(report, "Failing command", "passed", f"Exit 17, one task, Funnel state {created[0]['state']}")
    workflow.write_text("process WAIT {\n container 'debian:stable-slim'\n script:\n 'sleep 120; echo done'\n}\nworkflow { WAIT() }\n")
    for name, stopping in (("Cancellation", signal.SIGINT), ("Coordinator restart", signal.SIGKILL)):
        known = {task["id"] for task in tasks(server)}
        with (folder / "lifecycle.log").open("w") as stream:
            process = subprocess.Popen(base, cwd=folder, env=environment, stdout=stream, stderr=stream)
            try:
                task = wait_running(server, known, process)
                process.send_signal(stopping)
                process.wait(timeout=90)
            finally:
                if process.poll() is None:
                    process.kill()
                    process.wait()
        if stopping == signal.SIGINT:
            assert request(server, f"/tasks/{task['id']}?view=FULL")["state"] == "CANCELED"
            record(report, name, "passed", "Ctrl-C reached Funnel and task became CANCELED")
        else:
            with (folder / "restart.log").open("w") as stream:
                process = subprocess.Popen(base, cwd=folder, env=environment, stdout=stream, stderr=stream)
                try:
                    replacement = wait_running(server, known | {task["id"]}, process)
                    assert request(server, f"/tasks/{task['id']}?view=FULL")["state"] == "CANCELED"
                    process.send_signal(signal.SIGINT)
                    process.wait(timeout=90)
                finally:
                    if process.poll() is None:
                        process.kill()
                        process.wait()
            record(report, name, "passed", "Killed coordinator's known task was canceled before a fresh attempt")
            document = {key: replacement[key] for key in ("name", "inputs", "outputs", "executors", "resources", "tags") if key in replacement}
            assert document["tags"]["qualification/idempotency"]
            first = request(server, "/tasks", document)["id"]
            second = request(server, "/tasks", document)["id"]
            for identity in {first, second}:
                request(server, f"/tasks/{identity}:cancel", {})
            record(report, "Idempotency tag", "passed" if first == second else "server limitation",
                   "Same tagged request returned " + ("one task ID" if first == second else "different task IDs; Funnel ignores deduplication tags"))


def wait_running(server, known, process):
    deadline = time.monotonic() + 90
    while time.monotonic() < deadline:
        for task in tasks(server):
            if task["id"] not in known and task["state"] == "RUNNING":
                return task
        if process.poll() is not None:
            raise RuntimeError("iunctus exited before a TES task was running")
        time.sleep(0.25)
    raise RuntimeError("TES task did not reach RUNNING")


if __name__ == "__main__":
    main()
