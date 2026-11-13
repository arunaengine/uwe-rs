"""Runs the configured input transformation for the wrapper fixture."""
# Copyright (c) 2026 The uwe Contributors
# SPDX-License-Identifier: MIT or Apache-2.0
__author__ = "uwe"

from snakemake.shell import shell

extra = snakemake.params.get("extra", "")
log = snakemake.log_fmt_shell(stdout=False, stderr=True)
upper = "tr a-z A-Z" if extra == "--upper" else "cat"
shell("{upper} < {snakemake.input[0]} > {snakemake.output[0]} {log}")
