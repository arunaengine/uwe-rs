#!/usr/bin/env cwl-runner
# Greets with the defaults of the workflow, of the tool and of the steps, and counts a file.
# Copyright (c) 2026 The uwe Contributors
# SPDX-License-Identifier: MIT or Apache-2.0
cwlVersion: v1.2
class: Workflow
inputs:
  name:
    type: string
    default: world
  other: string?
  text:
    type: File
    default: {class: File, location: echo.cwl}
outputs:
  said:
    type: File
    outputSource: say/out
  again:
    type: File
    outputSource: again/out
  count:
    type: File
    outputSource: count/count
steps:
  say:
    run: greet.cwl
    in: {name: name}
    out: [out]
  again:
    run: greet.cwl
    in:
      name: {source: other, default: there}
      greeting: {default: hi}
    out: [out]
  count:
    run: count.cwl
    in: {text: text}
    out: [count]
