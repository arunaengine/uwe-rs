#!/usr/bin/env cwl-runner
# Echoes a message made by valueFrom, and the message with a count after it.
# Copyright (c) 2026 The uwe Contributors
# SPDX-License-Identifier: MIT or Apache-2.0
cwlVersion: v1.2
class: CommandLineTool
baseCommand: echo
inputs:
  message:
    type: string
    inputBinding: {position: 1, valueFrom: "$(self)!"}
  count:
    type: int
    default: 2
    inputBinding: {position: 2, valueFrom: "$(inputs.message) $(self)"}
stdout: out.txt
outputs:
  out:
    type: stdout
