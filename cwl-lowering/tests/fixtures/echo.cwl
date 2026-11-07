#!/usr/bin/env cwl-runner
# Echoes a message and more words to a named stdout file.
# Copyright (c) 2026 The uwe Contributors
# SPDX-License-Identifier: MIT or Apache-2.0
cwlVersion: v1.2
class: CommandLineTool
baseCommand: echo
arguments: ["-n"]
inputs:
  message:
    type: string
    inputBinding: {position: 1}
  words:
    type: string[]?
    inputBinding: {position: 2}
  unused: int?
stdout: out.txt
outputs:
  out:
    type: stdout
