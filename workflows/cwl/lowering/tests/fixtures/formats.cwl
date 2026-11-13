#!/usr/bin/env cwl-runner
# Counts the lines of a FASTA or FASTQ file, which the formats of its input ask for.
# Copyright (c) 2026 The uwe Contributors
# SPDX-License-Identifier: MIT or Apache-2.0
cwlVersion: v1.2
class: CommandLineTool
baseCommand: [wc, -l]
inputs:
  reads:
    type: File
    format: [http://edamontology.org/format_1929, http://edamontology.org/format_1930]
    inputBinding: {position: 1}
outputs:
  count: stdout
