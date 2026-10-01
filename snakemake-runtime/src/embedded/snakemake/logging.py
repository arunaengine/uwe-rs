"""Provides the Snakemake job logger on standard error."""
# Copyright (c) 2026 The iunctus Contributors
# SPDX-License-Identifier: MIT or Apache-2.0

import logging as _logging
import sys as _sys

logger = _logging.getLogger("snakemake")
if not logger.handlers:
    _handler = _logging.StreamHandler(_sys.stderr)
    _handler.setFormatter(_logging.Formatter("%(message)s"))
    logger.addHandler(_handler)
    logger.setLevel(_logging.INFO)
    logger.propagate = False
