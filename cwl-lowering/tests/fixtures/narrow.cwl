#!/usr/bin/env cwl-runner
# Counts the lines of two sequence files, whose formats the ontology narrow.ttl matches.
# Copyright (c) 2026 The uwe Contributors
# SPDX-License-Identifier: MIT or Apache-2.0
cwlVersion: v1.2
class: CommandLineTool
$namespaces:
  ex: http://example.org/formats/
$schemas:
  - narrow.ttl
baseCommand: [wc, -l]
inputs:
  narrow:
    type: File
    format: ex:fasta
    inputBinding: {position: 1}
  same:
    type: File
    format: ex:fasta
    inputBinding: {position: 2}
stdout: out.txt
outputs:
  out:
    type: stdout
