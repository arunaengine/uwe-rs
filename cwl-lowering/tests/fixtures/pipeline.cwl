#!/usr/bin/env cwl-runner
# Echoes a message, then counts its lines in a subworkflow.
# Copyright (c) 2026 The uwe Contributors
# SPDX-License-Identifier: MIT or Apache-2.0
cwlVersion: v1.2
class: Workflow
requirements:
  SubworkflowFeatureRequirement: {}
inputs:
  message: string
outputs:
  count:
    type: File
    outputSource: inner/count
  said:
    type: File
    outputSource: say/out
steps:
  say:
    run: echo.cwl
    in: {message: message}
    out: [out]
  inner:
    run: counting.cwl
    in: {text: say/out}
    out: [count]
