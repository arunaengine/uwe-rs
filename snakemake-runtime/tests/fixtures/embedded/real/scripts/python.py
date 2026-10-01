"""Writes the Python job outputs for the embedded-code fixture."""
# Copyright (c) 2026 The iunctus Contributors
# SPDX-License-Identifier: MIT or Apache-2.0
import os

from helper import shout
from snakemake.shell import shell

with open(snakemake.output[0], "w") as out:
    print(snakemake.input.data, snakemake.input[1], len(snakemake.input), file=out)
    print(snakemake.params.words, snakemake.params["number"], snakemake.params.table, file=out)
    print(snakemake.params.get("missing", "default"), sorted(snakemake.params.keys()), file=out)
    print(snakemake.wildcards.sample, snakemake.wildcards[0], snakemake.threads, snakemake.rule, file=out)
    print(snakemake.config["greeting"], snakemake.config["samples"], shout("x"), file=out)
    print(os.path.basename(__file__), os.path.basename(snakemake.scriptdir), file=out)
    print(open(snakemake.input.data).read().split(), file=out)
    print(snakemake.log[0], snakemake.log_fmt_shell(stdout=False, append=True), file=out)
log = snakemake.log_fmt_shell(stdout=False, stderr=True)
shell("echo shell {snakemake.wildcards.sample} threads $OMP_NUM_THREADS >> {snakemake.output[0]:q} {log}")
shell("(echo to the log >&2) {log}")
