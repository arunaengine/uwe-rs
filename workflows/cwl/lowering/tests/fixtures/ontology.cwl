#!/usr/bin/env cwl-runner
# Counts the lines of a FASTA file, a format that the EDAM ontology may match by a narrower one.
# Copyright (c) 2026 The uwe Contributors
# SPDX-License-Identifier: MIT or Apache-2.0
cwlVersion: v1.2
class: CommandLineTool
$namespaces:
  edam: http://edamontology.org/
baseCommand: [wc, -l]
inputs:
  sequences:
    type: File
    format: edam:format_1929
    inputBinding: {position: 1}
outputs: []
