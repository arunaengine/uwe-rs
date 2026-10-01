#!/usr/bin/env bash
# Writes the Bash job output for the embedded-code fixture.
# Copyright (c) 2026 The iunctus Contributors
# SPDX-License-Identifier: MIT or Apache-2.0
set -euo pipefail
exec > "${snakemake_output[0]}" 2> "${snakemake_log[0]}"
echo "input ${snakemake_input[0]} named ${snakemake_input[named]}"
echo "params ${snakemake_params[text]} and ${snakemake_params[pair]}"
echo "wildcard ${snakemake_wildcards[sample]} threads ${snakemake[threads]} rule ${snakemake[rule]}"
echo "config ${snakemake_config[greeting]}"
wc -l < "${snakemake_input[0]}"
