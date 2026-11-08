#!/usr/bin/env cwl-runner
# Lists the names of the files a tool made, which outputEval reads from the glob.
# Copyright (c) 2026 The uwe Contributors
# SPDX-License-Identifier: MIT or Apache-2.0
cwlVersion: v1.2
class: CommandLineTool
baseCommand: touch
inputs:
  names:
    type: string[]
    inputBinding: {position: 1}
outputs:
  count:
    type: int
    outputBinding:
      glob: "*"
      outputEval: $(self.length)
  made:
    type: File[]
    outputBinding: {glob: "*"}
