"""Check deterministic qualification inputs and graph evidence rejection."""
# Copyright (c) 2026 The uwe Contributors
# SPDX-License-Identifier: MIT or Apache-2.0

from pathlib import Path
import os
import runpy
import sys
import tempfile
import unittest
from unittest.mock import patch

QUALIFY = runpy.run_path(str(Path(__file__).resolve().parents[1] / "qualify.py"))


class Qualification(unittest.TestCase):
    def test_reference_identity(self):
        with tempfile.TemporaryDirectory() as root:
            first, second = Path(root) / "one", Path(root) / "two"
            digest = QUALIFY["make_reference"](first, 3, 257)
            self.assertEqual(digest, QUALIFY["make_reference"](second, 3, 257))
            self.assertEqual(sum(path.stat().st_size for path in first.iterdir()), 771)
            third = Path(root) / "three"
            self.assertNotEqual(digest, QUALIFY["make_reference"](third, 3, 258))

    def test_graph_contract(self):
        graph = {"simulation": "no real tools", "nodes": [
            {"task": {"id": 1, "step": "FANOUT", "inputs": [{"directory": True}]}},
            {"task": {"id": 2, "step": "FANOUT", "inputs": [{"directory": True}]}},
            {"task": {"id": 3, "step": "GATHER", "inputs": []}},
        ], "edges": [{"from": 1, "to": 3}, {"from": 2, "to": 3}]}
        self.assertEqual(QUALIFY["counts"](graph, 2)["gather_sources"], 2)
        graph["edges"].pop()
        with self.assertRaisesRegex(ValueError, "missing gather"):
            QUALIFY["counts"](graph, 2)

    def test_requires_simulation(self):
        with self.assertRaisesRegex(ValueError, "simulated"):
            QUALIFY["counts"]({"nodes": [], "edges": []}, 1)


class RecorderPaths(unittest.TestCase):
    def test_record_locations(self):
        root = Path(__file__).resolve().parents[2]
        fixtures = root / "uwe/tests/fixtures"
        for name in ("nextflow", "argo", "cwl", "snakemake", "slurm/argo", "slurm/cwl", "slurm/snakemake"):
            with self.subTest(name=name), patch.dict(sys.modules, {"yaml": object()}), \
                    patch.object(sys, "path", list(sys.path)), patch.object(sys, "dont_write_bytecode", True), \
                    patch.dict(os.environ, {}, clear=True):
                module = runpy.run_path(str(fixtures / "real_runs" / f"{name}.py"))
                expected = "real_runs.txt" if name == "nextflow" else f"real_runs_{name.replace('/', '_')}.txt"
                self.assertEqual(module["OUTPUT"], fixtures / expected)
                if "REPOSITORY" in module:
                    self.assertEqual(module["REPOSITORY"], root)
                if "LOCAL" in module:
                    self.assertEqual(module["LOCAL"], fixtures / "real_runs_argo.txt")


if __name__ == "__main__":
    unittest.main()
