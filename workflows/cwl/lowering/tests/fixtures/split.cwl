#!/usr/bin/env cwl-runner
# Splits a file into one file per line, which an array glob collects.
# Copyright (c) 2026 The uwe Contributors
# SPDX-License-Identifier: MIT or Apache-2.0
cwlVersion: v1.2
class: CommandLineTool
baseCommand: split
arguments:
  - {prefix: "-l", valueFrom: "1", position: 0}
inputs:
  text:
    type: File
    inputBinding: {position: 1}
  prefix:
    type: string
    inputBinding: {position: 2}
outputs:
  parts:
    type: File[]
    outputBinding: {glob: "$(inputs.prefix)*"}
