#!/usr/bin/env cwl-runner
# Echoes a greeting, hello by default, and a name to a named stdout file.
# Copyright (c) 2026 The uwe Contributors
# SPDX-License-Identifier: MIT or Apache-2.0
cwlVersion: v1.2
class: CommandLineTool
baseCommand: echo
inputs:
  greeting:
    type: string
    default: hello
    inputBinding: {position: 1}
  name:
    type: string
    inputBinding: {position: 2}
stdout: out.txt
outputs:
  out:
    type: stdout
