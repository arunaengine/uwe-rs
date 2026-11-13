#!/usr/bin/env cwl-runner
# Echoes each message of a list.
# Copyright (c) 2026 The uwe Contributors
# SPDX-License-Identifier: MIT or Apache-2.0
cwlVersion: v1.2
class: Workflow
requirements:
  ScatterFeatureRequirement: {}
inputs:
  messages: string[]
outputs:
  said:
    type: File[]
    outputSource: say/out
steps:
  say:
    run: echo.cwl
    scatter: message
    in: {message: messages}
    out: [out]
