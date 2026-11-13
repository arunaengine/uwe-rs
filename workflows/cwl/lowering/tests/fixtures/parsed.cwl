#!/usr/bin/env cwl-runner
# Counts words with static arguments, and an output that a library function reads.
# Copyright (c) 2026 The uwe Contributors
# SPDX-License-Identifier: MIT or Apache-2.0
cwlVersion: v1.2
class: CommandLineTool
requirements:
  InlineJavascriptRequirement:
    expressionLib: ["function parse(files) { return parseInt(files[0].contents); }"]
baseCommand: [wc, -w]
stdout: count.txt
inputs:
  text:
    type: File
    inputBinding: {position: 1}
outputs:
  count:
    type: int
    outputBinding:
      glob: count.txt
      loadContents: true
      outputEval: $(parse(self))
