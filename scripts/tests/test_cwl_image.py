"""Check loader aliases in the native CWL worker image build context."""
# Copyright (c) 2026 The uwe Contributors
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
    def test_packages_static(self):
        script = Path(__file__).resolve().parents[1] / "cwl-js-image.py"
        for code, stdout, stderr in [(1, "", "\tnot a dynamic executable\n"),
                                     (1, "not a dynamic executable\n", ""),
                                     (0, "\tstatically linked\n", "")]:
            with self.subTest(code=code, stdout=stdout, stderr=stderr), tempfile.TemporaryDirectory() as directory:
                binary = Path(directory) / "uwe"
                binary.write_bytes(b"trusted static binary fixture")

                def command(arguments, **options):
                    if arguments[0] == "ldd":
                        self.assertEqual(arguments, ["ldd", "--", str(binary)])
                        self.assertEqual(options["env"]["LC_ALL"], "C")
                        return subprocess.CompletedProcess(arguments, code, stdout, stderr)
                    self.assertEqual(arguments, ["mock-engine", "build", "--network=none", "--pull=false", "--tag", "fixture:1", "-"])
                    with tarfile.open(fileobj=options["stdin"], mode="r:") as context:
                        self.assertEqual(context.getnames(), ["Dockerfile", "rootfs/tmp",
                                         "rootfs/usr/local/bin/uwe", "rootfs/usr/share/uwe/cwl-js.sha256"])
                        self.assertEqual(context.extractfile("rootfs/usr/local/bin/uwe").read(), binary.read_bytes())
                        self.assertTrue(context.getmember("rootfs/tmp").isdir())
                        self.assertEqual(context.getmember("rootfs/tmp").mode, 0o1777)
                        identities = context.extractfile("rootfs/usr/share/uwe/cwl-js.sha256").read().decode()
                        self.assertEqual(identities, f"{hashlib.sha256(binary.read_bytes()).hexdigest()}  /usr/local/bin/uwe\n")
                    return subprocess.CompletedProcess(arguments, 0)

                with patch("subprocess.run", side_effect=command), patch("sys.argv", [str(script), str(binary), "--engine", "mock-engine", "--image", "fixture:1"]):
                    runpy.run_path(str(script), run_name="__main__")

    def test_rejects_ldd(self):
        script = Path(__file__).resolve().parents[1] / "cwl-js-image.py"
        with tempfile.TemporaryDirectory() as directory:
            binary = Path(directory) / "uwe"
            binary.write_bytes(b"trusted binary fixture")
            for code, output, error in [(1, "ldd failed", subprocess.CalledProcessError),
                                        (0, "libc.so => not found", RuntimeError),
                                        (0, "unexpected output", RuntimeError)]:
                with self.subTest(output=output), patch("subprocess.run", return_value=subprocess.CompletedProcess(
                        ["ldd", "--", str(binary)], code, output, "")) as command, patch("sys.argv", [str(script), str(binary)]):
                    with self.assertRaises(error):
                        runpy.run_path(str(script), run_name="__main__")
                    command.assert_called_once()

    def test_preserves_loader(self):
        script = Path(__file__).resolve().parents[1] / "cwl-js-image.py"
        with tempfile.TemporaryDirectory() as directory:
            binary = Path(directory) / "uwe"
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
                    identities = context.extractfile("rootfs/usr/share/uwe/cwl-js.sha256").read().decode()
                    self.assertIn(f"{hashlib.sha256(loader.read_bytes()).hexdigest()}  {alias}\n", identities)
                return subprocess.CompletedProcess(arguments, 0)

            with patch("subprocess.run", side_effect=command), patch("sys.argv", [str(script), str(binary), "--engine", "mock-engine", "--image", "fixture:1"]):
                runpy.run_path(str(script), run_name="__main__")


if __name__ == "__main__":
    unittest.main()
