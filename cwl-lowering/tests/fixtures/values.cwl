#!/usr/bin/env cwl-runner
# Echoes messages made by valueFrom, once for a name and once per name of a scatter.
# Copyright (c) 2026 The uwe Contributors
# SPDX-License-Identifier: MIT or Apache-2.0
cwlVersion: v1.2
class: Workflow
requirements:
  ScatterFeatureRequirement: {}
  StepInputExpressionRequirement: {}
inputs:
  name: string
  names: string[]
outputs:
  said:
    type: File
    outputSource: say/out
  each:
    type: File[]
    outputSource: each/out
steps:
  say:
    run: echo.cwl
    in:
      message: {source: name, valueFrom: "hello $(self)"}
    out: [out]
  each:
    run: echo.cwl
    scatter: message
    in:
      message: {source: names, valueFrom: "$(inputs.greeting) $(self)"}
      greeting: {default: hi}
    out: [out]
