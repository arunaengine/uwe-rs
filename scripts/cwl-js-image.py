#!/usr/bin/env python3
"""Build the native CWL JavaScript helper from a trusted Linux binary and its libraries."""
# Copyright (c) 2026 The uwe Contributors
# SPDX-License-Identifier: MIT or Apache-2.0

import argparse
import hashlib
import io
import os
from pathlib import Path
import re
import subprocess
import tarfile
import tempfile


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("binary", type=Path, help="trusted compiled iunctus binary")
    parser.add_argument("--engine", default=os.environ.get("UWE_ENGINE", "docker"))
    parser.add_argument("--image", default=os.environ.get("UWE_CWL_JS_IMAGE", "iunctus-cwl-js:0.1.0"))
    arguments = parser.parse_args()
    binary = arguments.binary.resolve(strict=True)
    resolved = subprocess.run(["ldd", "--", str(binary)], capture_output=True, text=True,
                              env=dict(os.environ, LC_ALL="C"))
    static = (resolved.stdout + resolved.stderr).strip() in (
        "statically linked", "not a dynamic executable")
    if not static:
        resolved.check_returncode()
    files = {"/usr/local/bin/iunctus": binary}
    for line in ([] if static else resolved.stdout.splitlines()):
        if "=> not found" in line:
            raise RuntimeError("a required runtime library was not found")
        match = re.fullmatch(r"\s*(?:(\S+)\s+=>\s+)?(/.+?)\s+\(0x[0-9a-fA-F]+\)\s*", line)
        if match:
            location = Path(os.path.normpath(match[2]))
            source = location.resolve(strict=True)
            files[str(location)] = source
            if match[1] and Path(match[1]).is_absolute():
                files[os.path.normpath(match[1])] = source
        elif not line.strip().startswith("linux-vdso"):
            raise RuntimeError("unrecognized ldd output for the trusted binary")

    dockerfile = Path(__file__).resolve().parent.parent / "containers/cwl-js/Dockerfile"
    with tempfile.TemporaryFile() as context:
        with tarfile.open(fileobj=context, mode="w") as archive:
            def add_bytes(name, data, mode=0o644):
                entry = tarfile.TarInfo(name)
                entry.mode = mode
                entry.size = len(data)
                archive.addfile(entry, io.BytesIO(data))

            add_bytes("Dockerfile", dockerfile.read_bytes())
            entry = tarfile.TarInfo("rootfs/tmp")
            entry.type = tarfile.DIRTYPE
            entry.mode = 0o1777
            archive.addfile(entry)
            identities = []
            for location, source in sorted(files.items()):
                data = source.read_bytes()
                identities.append(f"{hashlib.sha256(data).hexdigest()}  {location}\n")
                add_bytes("rootfs" + location, data, 0o755)
            add_bytes("rootfs/usr/share/iunctus/cwl-js.sha256", "".join(identities).encode())
        context.seek(0)
        pull = "--pull=never" if Path(arguments.engine).name == "podman" else "--pull=false"
        subprocess.run([arguments.engine, "build", "--network=none", pull, "--tag", arguments.image, "-"], stdin=context, check=True)


if __name__ == "__main__":
    main()
