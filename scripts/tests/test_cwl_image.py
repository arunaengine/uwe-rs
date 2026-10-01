"""Check loader aliases in the native CWL worker image build context."""
# Copyright (c) 2026 The iunctus Contributors
# SPDX-License-Identifier: MIT or Apache-2.0

import hashlib
from pathlib import Path
import runpy
import subprocess
import tarfile
import tempfile
import unittest
from unittest.mock import patch


class ImageContext(unittest.TestCase):
    def test_preserves_loader(self):
        script = Path(__file__).resolve().parents[1] / "cwl-js-image.py"
        with tempfile.TemporaryDirectory() as directory:
            binary = Path(directory) / "iunctus"
            loader = Path(directory) / "ld-linux-x86-64.so.2"
            binary.write_bytes(b"trusted binary fixture")
            loader.write_bytes(b"trusted loader fixture")
            alias = "/lib64/ld-linux-x86-64.so.2"

            def command(arguments, **options):
                if arguments[0] == "ldd":
                    self.assertEqual(arguments, ["ldd", "--", str(binary)])
                    return subprocess.CompletedProcess(arguments, 0, f"{alias} => {loader} (0x001)\n", "")
                self.assertEqual(arguments, ["mock-engine", "build", "--network=none", "--pull=false", "--tag", "fixture:1", "-"])
                with tarfile.open(fileobj=options["stdin"], mode="r:") as context:
                    for location in [alias, str(loader)]:
                        self.assertEqual(context.extractfile("rootfs" + location).read(), loader.read_bytes())
                    identities = context.extractfile("rootfs/usr/share/iunctus/cwl-js.sha256").read().decode()
                    self.assertIn(f"{hashlib.sha256(loader.read_bytes()).hexdigest()}  {alias}\n", identities)
                return subprocess.CompletedProcess(arguments, 0)

            with patch("subprocess.run", side_effect=command), patch("sys.argv", [str(script), str(binary), "--engine", "mock-engine", "--image", "fixture:1"]):
                runpy.run_path(str(script), run_name="__main__")


if __name__ == "__main__":
    unittest.main()
