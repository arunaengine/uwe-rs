#!/usr/bin/env cwl-runner
# Echoes every message with every list of words, nested by message.
# Copyright (c) 2026 The uwe Contributors
# SPDX-License-Identifier: MIT or Apache-2.0
cwlVersion: v1.2
class: Workflow
requirements:
  ScatterFeatureRequirement: {}
inputs:
  messages: string[]
  lists:
    type:
      type: array
      items: {type: array, items: string}
outputs:
  said:
    type:
      type: array
      items: {type: array, items: File}
    outputSource: say/out
steps:
  say:
    run: echo.cwl
    scatter: [message, words]
    scatterMethod: nested_crossproduct
    in: {message: messages, words: lists}
    out: [out]
