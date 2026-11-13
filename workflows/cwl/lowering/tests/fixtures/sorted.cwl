#!/usr/bin/env cwl-runner
# Sorts a text file, whose output format an expression picks from the input.
# Copyright (c) 2026 The uwe Contributors
# SPDX-License-Identifier: MIT or Apache-2.0
cwlVersion: v1.2
class: CommandLineTool
requirements:
  InlineJavascriptRequirement: {}
baseCommand: sort
inputs:
  text:
    type: File
    inputBinding: {position: 1}
stdout: sorted.txt
outputs:
  sorted:
    type: stdout
    format: ${ return inputs.text.format || "http://edamontology.org/format_2330"; }
