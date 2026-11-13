#!/bin/sh
# Records nf-validation 1.1.3 oracle output with Nextflow 26.04.6 and its legacy parser.
# Copyright (c) 2026 The uwe Contributors
# SPDX-License-Identifier: MIT or Apache-2.0
export NXF_SYNTAX_PARSER=v1
nextflow run main.nf -ansi-log false
nextflow run invalid.nf -ansi-log false --n x --mode c --outdir ''
