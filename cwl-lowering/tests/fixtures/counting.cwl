#!/usr/bin/env cwl-runner
# Counts the lines of a file with one step.
# Copyright (c) 2026 The uwe Contributors
# SPDX-License-Identifier: MIT or Apache-2.0
cwlVersion: v1.2
class: Workflow
inputs:
  text: File
outputs:
  count:
    type: File
    outputSource: wc/count
steps:
  wc:
    run: count.cwl
    in: {text: text}
    out: [count]
