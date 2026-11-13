#!/usr/bin/env cwl-runner
# Joins the outputs of echo steps by several sources, linkMerge and pickValue.
# Copyright (c) 2026 The uwe Contributors
# SPDX-License-Identifier: MIT or Apache-2.0
cwlVersion: v1.2
class: Workflow
requirements:
  MultipleInputFeatureRequirement: {}
  ScatterFeatureRequirement: {}
inputs:
  name: string
  names: string[]
outputs:
  both:
    type: File[]
    outputSource: [say/out, again/out]
  flat:
    type: File[]
    outputSource: [each/out, say/out]
    linkMerge: merge_flattened
  first:
    type: File
    outputSource: [say/out, again/out]
    pickValue: first_non_null
  count:
    type: File
    outputSource: count/count
steps:
  say:
    run: echo.cwl
    in: {message: name}
    out: [out]
  again:
    run: echo.cwl
    in:
      message: name
      words: {source: [name, name]}
    out: [out]
  each:
    run: echo.cwl
    scatter: message
    in: {message: names}
    out: [out]
  count:
    run: count.cwl
    in:
      text: {source: [say/out, again/out], pickValue: first_non_null}
    out: [count]
