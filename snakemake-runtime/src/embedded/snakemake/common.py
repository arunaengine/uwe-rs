"""Provides constants and helpers for snakemake.common."""
# Copyright (c) 2026 The iunctus Contributors
# SPDX-License-Identifier: MIT or Apache-2.0

import hashlib

from snakemake import __version__  # noqa: F401

ON_WINDOWS = False
MIN_PY_VERSION = (3, 7)


def get_report_id(path):
    return hashlib.sha256(str(path).encode()).hexdigest()
