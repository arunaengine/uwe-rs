#!/usr/bin/env cwl-runner
# Adds two numbers, the second 2 by default, and lists both.
# Copyright (c) 2026 The uwe Contributors
# SPDX-License-Identifier: MIT or Apache-2.0
cwlVersion: v1.2
class: ExpressionTool
requirements:
  InlineJavascriptRequirement: {}
inputs:
  a: int
  b:
    type: int
    default: 2
outputs:
  sum: int
  both: int[]
expression: |
  ${ return {"sum": inputs.a + inputs.b, "both": [inputs.a, inputs.b]}; }
