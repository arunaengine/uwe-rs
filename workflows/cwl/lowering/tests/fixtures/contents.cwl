#!/usr/bin/env cwl-runner
# Echoes the contents that CWL loads into a workflow input.
# Copyright (c) 2026 The uwe Contributors
# SPDX-License-Identifier: MIT or Apache-2.0
cwlVersion: v1.2
class: Workflow
requirements:
  StepInputExpressionRequirement: {}
inputs:
  text:
    type: File
    loadContents: true
outputs:
  said:
    type: File
    outputSource: say/out
steps:
  say:
    run: echo.cwl
    in:
      message: {source: text, valueFrom: "$(self.contents)"}
    out: [out]
