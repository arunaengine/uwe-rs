#!/usr/bin/env python3
"""Check demo outputs, failures, resume and cancellation with one local Docker image."""
# Copyright (c) 2026 The uwe Contributors
# SPDX-License-Identifier: MIT or Apache-2.0

import json
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer
import os
from pathlib import Path
import re
import runpy
import shutil
import signal
import sqlite3
import subprocess
import sys
import tempfile
import threading
import time
from urllib.parse import unquote, urlsplit

ROOT = Path(__file__).resolve().parent.parent
FIXTURES = ROOT / "uwe/tests/fixtures"
IMAGE = "debian:stable-slim@sha256:eb593cf2c358cacef45ca0a424bbc7d30cfa3466265fc2662b9466a0ca6ba1c5"
TIMEOUT = 60


def main():
    started = time.monotonic()
    binary = Path(sys.argv[1] if len(sys.argv) > 1 else "target/debug/iunctus").resolve(strict=True)
    docker("image", "inspect", IMAGE)
    print(f"Image: {IMAGE} (local only)", flush=True)
    failures = []
    proxy = runpy.run_path(str(FIXTURES / "docker_no_pull.py"))["NoPull"]
    with tempfile.TemporaryDirectory(prefix="iunctus-container-runs-") as temporary:
        root = Path(temporary)
        try:
            with proxy(root) as socket:
                environment = dict(os.environ, XDG_CONFIG_HOME=str(root / "config"),
                                   IUNCTUS_SITE_CONFIG="", DOCKER_HOST=f"unix://{socket}")
                for name in ("snakemake", "nextflow", "cwl", "argo", "failure", "tes", "cancel", "simulator"):
                    begin = time.monotonic()
                    try:
                        exercise(binary, root, environment, name)
                    except (AssertionError, OSError, sqlite3.Error, subprocess.SubprocessError) as error:
                        failures.append(name)
                        print(f"FAIL {name}: {error}", flush=True)
                    else:
                        print(f"PASS {name} ({time.monotonic() - begin:.2f}s)", flush=True)
        finally:
            for container in owned(root):
                docker("rm", "--force", container)
    print(f"Container runs: {8 - len(failures)} passed, {len(failures)} failed "
          f"({time.monotonic() - started:.2f}s)", flush=True)
    return bool(failures)


def exercise(binary, root, environment, name):
    folder = root / name
    folder.mkdir()
    environment = dict(environment, XDG_CACHE_HOME=str(folder / "cache"))
    source, count = prepare(folder, name)
    command = [str(binary), "run", str(source), "-no-network", "-max-pending", "2",
               "-default-image", IMAGE, "-output-dir", str(folder / "results")]
    if name == "snakemake":
        command.append("hello/all.txt")
    if name == "cwl":
        command += ["-inputs", str(folder / "job.json")]
    if name == "tes":
        tes_failure(command, folder, environment)
        return
    if name == "simulator":
        command += ["-executor", "simulator"]
        environment["DOCKER_HOST"] = f"unix://{folder}/absent.sock"
    since = time.time()
    code, output = execute(command, folder, environment, name == "cancel")
    assert code == (130 if name == "cancel" else 0) or name == "failure", output
    run, tasks, attempts, logs = recorded(folder)
    if name == "failure":
        assert code != 0, output
        assert run[1] == "failed" and tasks == [("FAIL", "failed")], (run, tasks, output)
        assert attempts == [(17, None, IMAGE)], (attempts, output)
        summary(output, "Failed after", 1, 0, 0, 1)
    elif name == "cancel":
        assert code == 130, output
        assert run[1] == "failed" and tasks == [("WAIT", "failed")], (run, tasks, output)
        assert not owned(folder), "cancelled containers remain"
        summary(output, "Failed after", 1, 0, 0, 1)
    else:
        assert code == 0, output
        assert len(tasks) == count and all(state == "succeeded" for _, state in tasks), tasks
        assert run[1] == "succeeded", run
        if name == "simulator":
            assert run[2:4] == ("simulator", 1), run
            assert not starts(since, run[0]), "simulator started a container"
        else:
            assert run[2:4] == ("local", 0), run
            assert attempts == [(0, None, IMAGE)] * count, attempts
            assert len(starts(since, run[0])) == count, "tasks did not start real containers"
            summary(output, "Done in", count, count, 0, 0)
            outputs(folder, name, logs)
    if name == "nextflow":
        since = time.time()
        code, output = execute(command + ["-resume", run[0]], folder, environment, False)
        resumed, tasks, attempts, logs = recorded(folder)
        assert code == 0 and resumed[1] == "succeeded", output
        assert resumed[4] == run[0], "resume did not record its parent run"
        assert len(tasks) == count and all(state == "succeeded" for _, state in tasks), tasks
        assert len(attempts) == count and all(
            code == 0 and reused in (run[0], "cache") and image == IMAGE
            for code, reused, image in attempts), attempts
        assert not starts(since, resumed[0]), "resume started containers"
        summary(output, "Done in", count, 0, count, 0)
        outputs(folder, name, logs)
        print("  resume: all 4 tasks reused, no containers started", flush=True)


def prepare(folder, name):
    if name == "snakemake":
        source = folder / "hello.smk"
        source.write_text((ROOT / "demo/snakemake/hello.smk").read_text()
                          .replace("docker://debian:stable-slim", f"docker://{IMAGE}"))
        return source, 5
    if name in ("nextflow", "simulator"):
        source = folder / "main.nf"
        shutil.copyfile(ROOT / "demo/nextflow/main.nf", source)
        (folder / "nextflow.config").write_text(
            (ROOT / "demo/nextflow/nextflow.config").read_text()
            .replace("quay.io/nextflow/bash", IMAGE))
        return source, 4
    if name == "argo":
        source = folder / "dag.yaml"
        source.write_text((ROOT / "demo/argo/dag-diamond.yaml").read_text()
                          .replace("image: busybox", f"image: {IMAGE}"))
        return source, 4
    if name == "cwl":
        source = folder / "directory-copy.cwl"
        source.write_text((ROOT / "demo/cwl/directory-copy.cwl").read_text()
                          .replace("debian:stable-slim", IMAGE))
        inputs = folder / "inputs"
        inputs.mkdir()
        for leaf in ("first", "second"):
            shutil.copyfile(FIXTURES / f"directory_{leaf}.txt", inputs / f"{leaf}.txt")
        (folder / "job.json").write_text(json.dumps({"folder": {
            "class": "Directory", "path": str(inputs)}}))
        return source, 1
    source = folder / "main.nf"
    step, script = ("FAIL", "exit 17") if name in ("failure", "tes") else ("WAIT", "sleep 120")
    source.write_text(f"process {step} {{\n container '{IMAGE}'\n script:\n '{script}'\n}}\n"
                      f"workflow {{\n {step}()\n}}\n")
    return source, 1


def execute(command, folder, environment, cancel):
    with subprocess.Popen(command, cwd=folder, env=environment, stdout=subprocess.PIPE,
                          stderr=subprocess.PIPE, text=True) as process:
        try:
            if cancel:
                deadline = time.monotonic() + TIMEOUT
                while not owned(folder):
                    assert process.poll() is None, "run ended before a container started"
                    assert time.monotonic() < deadline, "container did not start"
                    time.sleep(0.1)
                process.send_signal(signal.SIGINT)
            stdout, stderr = process.communicate(timeout=TIMEOUT if not cancel else 15)
        except BaseException:
            process.terminate()
            try:
                process.communicate(timeout=15)
            except subprocess.TimeoutExpired:
                process.kill()
                process.communicate()
            raise
    output = stdout + stderr
    for line in output.splitlines():
        if re.match(r"^(Done in|Failed after) ", line):
            print(f"  exit {process.returncode}: {line}", flush=True)
    return process.returncode, output


def recorded(folder):
    with sqlite3.connect(folder / "cache/iunctus/runs.sqlite") as database:
        run = database.execute("SELECT id, state, executor, simulated, resumes FROM runs "
                               "ORDER BY started DESC, rowid DESC LIMIT 1").fetchone()
        assert run, "no run was recorded"
        tasks = database.execute("SELECT step, state FROM tasks WHERE run = ? ORDER BY id",
                                 (run[0],)).fetchall()
        attempts = database.execute("SELECT exit_code, reused, image FROM attempts "
                                    "WHERE run = ? ORDER BY task", (run[0],)).fetchall()
        logs = database.execute("SELECT text FROM logs WHERE run = ? AND stream = 'stdout' "
                                "ORDER BY task", (run[0],)).fetchall()
    return run, tasks, attempts, [text.encode() for (text,) in logs]


def summary(output, state, count, ran, reused, failed):
    expected = f"{count} {'task' if count == 1 else 'tasks'}, {ran} ran, {reused} from the cache, {failed} failed"
    lines = [line for line in output.splitlines() if line.startswith(f"{state} ")]
    assert len(lines) == 1 and lines[0].endswith(f": {expected}"), output


def outputs(folder, name, logs):
    if name == "snakemake":
        assert (folder / "hello/all.txt").read_bytes() == (FIXTURES / "snakemake_hello.txt").read_bytes()
    elif name in ("nextflow", "argo"):
        expected = (FIXTURES / f"{name}_stdout.txt").read_bytes()
        assert b"".join(sorted(logs)) == expected, (logs, expected)
    elif name == "cwl":
        found = sorted((folder / "results").glob("*.txt"))
        assert [path.name for path in found] == ["first.txt", "second.txt"], found
        for path in found:
            assert path.read_bytes() == (FIXTURES / f"directory_{path.stem}.txt").read_bytes(), path


def docker(*arguments):
    return subprocess.run(["docker", *arguments], check=True, capture_output=True,
                          text=True, timeout=15).stdout


def owned(folder):
    names = docker("ps", "--filter", "name=iunctus-", "--format", "{{.Names}}").split()
    found = []
    for name in names:
        inspected = subprocess.run(["docker", "inspect", name], capture_output=True,
                                   text=True, timeout=15)
        if inspected.returncode == 0:
            mounts = json.loads(inspected.stdout)[0]["Mounts"]
            if any(Path(mount["Source"]).is_relative_to(folder) for mount in mounts):
                found.append(name)
    return found


def starts(since, run):
    events = docker("events", "--since", str(since), "--until", str(time.time()),
                    "--filter", "type=container", "--filter", "event=start", "--format", "{{json .}}")
    return [event for line in events.splitlines() if (event := json.loads(line))
            ["Actor"]["Attributes"].get("name", "").startswith(f"iunctus-{run}-")]


def tes_failure(command, folder, environment):
    with ThreadingHTTPServer(("127.0.0.1", 0), TesHandler) as server:
        server.objects = {}
        server.task = None
        image = json.loads(docker("image", "inspect", IMAGE))[0]
        server.config = {"config": image["Config"], "os": image["Os"],
                         "architecture": image["Architecture"], "variant": image.get("Variant", "")}
        server.digest = image["Id"]
        endpoint = f"http://127.0.0.1:{server.server_port}"
        server.image = f"127.0.0.1:{server.server_port}/debian@{IMAGE.split('@')[1]}"
        source = folder / "main.nf"
        source.write_text(source.read_text().replace(IMAGE, server.image))
        config = folder / "tes.toml"
        config.write_text(f'''[executor.tes]
server = "{endpoint}"
staging = "object-store"
staging_root = "s3://bucket/staging"
[storage.tes]
type = "s3"
prefixes = ["s3://bucket/"]
endpoint = "{endpoint}"
region = "us-east-1"
access_key_id = "container-runs"
secret_access_key = "container-runs-secret"
''')
        thread = threading.Thread(target=server.serve_forever, daemon=True)
        thread.start()
        try:
            invocation = [value for value in command if value != "-no-network"]
            code, output = execute(invocation + ["-executor", "tes", "-iunctus-config", str(config)],
                                   folder, environment, False)
            run, tasks, attempts, _ = recorded(folder)
            assert code != 0 and run[1:3] == ("failed", "tes"), (run, output)
            assert tasks == [("FAIL", "failed")], (tasks, output)
            assert attempts == [(17, None, server.image)], (attempts, output)
            assert server.task and server.objects, "no TES object staging occurred"
            summary(output, "Failed after", 1, 0, 0, 1)
        finally:
            server.shutdown()
            thread.join()


class TesHandler(BaseHTTPRequestHandler):
    """Return a completed wrapper with exit 17 despite a successful TES executor."""

    def do_PUT(self):
        self.server.objects[unquote(urlsplit(self.path).path)] = self.body()
        self.reply(200, b"")

    def do_POST(self):
        task = json.loads(self.body())
        inputs = task["inputs"]
        assert inputs and all(self.key(item["url"]) in self.server.objects for item in inputs)
        assert any(b"exit 17" in value for value in self.server.objects.values())
        assert all(item["image"] == self.server.image for item in task["executors"])
        self.server.task = task
        self.reply(200, json.dumps({"id": "failed-command"}).encode())

    def do_GET(self):
        if self.path.startswith("/v2/"):
            # TES reads image metadata from a registry; keep that lookup on loopback too.
            document = ({"schemaVersion": 2, "config": {"digest": self.server.digest}}
                        if "/manifests/" in self.path else self.server.config)
            self.reply(200, json.dumps(document).encode())
        elif self.path.startswith("/ga4gh/tes/v1/tasks/"):
            outputs = []
            for item in self.server.task["outputs"]:
                if item["path"].startswith("/iunctus/control/"):
                    content = b"17\n" if item["path"].endswith("/exit") else b""
                    self.server.objects[self.key(item["url"])] = content
                    outputs.append({"url": item["url"], "path": item["path"],
                                    "size_bytes": str(len(content))})
            document = {"id": "failed-command", "state": "COMPLETE", "logs": [
                {"logs": [{"exit_code": 0}], "outputs": outputs}]}
            self.reply(200, json.dumps(document).encode())
        else:
            content = self.server.objects.get(unquote(urlsplit(self.path).path))
            self.reply(404 if content is None else 200, content or b"")

    def do_HEAD(self):
        self.do_GET()

    def body(self):
        return self.rfile.read(int(self.headers["Content-Length"]))

    def key(self, url):
        parsed = urlsplit(url)
        return unquote(f"/{parsed.netloc}{parsed.path}")

    def reply(self, status, content):
        self.send_response(status)
        self.send_header("Content-Length", str(len(content)))
        self.end_headers()
        if self.command != "HEAD":
            self.wfile.write(content)

    def log_message(self, format, *arguments):
        pass


if __name__ == "__main__":
    sys.exit(main())
