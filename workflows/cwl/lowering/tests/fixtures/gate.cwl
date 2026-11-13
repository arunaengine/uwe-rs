#!/usr/bin/env cwl-runner
# Echoes a message only when the gate is open.
# Copyright (c) 2026 The uwe Contributors
# SPDX-License-Identifier: MIT or Apache-2.0
cwlVersion: v1.2
class: Workflow
inputs:
  message: string
  go: boolean
outputs:
  said:
    type: File?
    outputSource: say/out
steps:
  say:
    run: echo.cwl
    in: {message: message, go: go}
    when: $(inputs.go)
    out: [out]
