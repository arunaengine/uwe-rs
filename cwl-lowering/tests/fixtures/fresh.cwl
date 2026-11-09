#!/usr/bin/env cwl-runner
# Echoes a message, again every time when the input fresh asks for it.
# Copyright (c) 2026 The uwe Contributors
# SPDX-License-Identifier: MIT or Apache-2.0
cwlVersion: v1.2
class: CommandLineTool
requirements:
  InlineJavascriptRequirement: {}
  WorkReuse:
    enableReuse: $(!inputs.fresh)
baseCommand: echo
inputs:
  message:
    type: string
    inputBinding: {position: 1}
  fresh: boolean
stdout: out.txt
outputs:
  out:
    type: stdout
