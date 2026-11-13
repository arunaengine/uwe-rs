#!/usr/bin/env cwl-runner
# Doubles a value of any type that has to be given.
# Copyright (c) 2026 The uwe Contributors
# SPDX-License-Identifier: MIT or Apache-2.0
cwlVersion: v1.2
class: ExpressionTool
requirements:
  InlineJavascriptRequirement: {}
inputs:
  value: Any
outputs:
  out: int
expression: "$({'out': 2 * inputs.value})"
