#!/usr/bin/env cwl-runner
# Echoes a message every time, as WorkReuse turns off the reuse of earlier results.
# Copyright (c) 2026 The uwe Contributors
# SPDX-License-Identifier: MIT or Apache-2.0
cwlVersion: v1.2
class: CommandLineTool
requirements:
  WorkReuse: {enableReuse: false}
baseCommand: echo
inputs:
  message:
    type: string
    inputBinding: {position: 1}
stdout: out.txt
outputs:
  out:
    type: stdout
