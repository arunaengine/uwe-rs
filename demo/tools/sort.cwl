#!/usr/bin/env cwl-runner
# Sorts the lines of a text, as a CWL CommandLineTool.
# Copyright (c) 2026 The uwe Contributors
# SPDX-License-Identifier: MIT or Apache-2.0
cwlVersion: v1.2
class: CommandLineTool
label: Sort lines
$namespaces:
  edam: http://edamontology.org/
hints:
  DockerRequirement:
    dockerPull: debian:stable-slim
baseCommand: [sort]
inputs:
  text:
    type: File
    doc: The text to sort.
    inputBinding:
      position: 1
stdout: sorted.txt
outputs:
  sorted:
    type: stdout
    format: edam:format_2330
