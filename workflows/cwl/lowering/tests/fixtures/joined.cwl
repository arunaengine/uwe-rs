#!/usr/bin/env cwl-runner
# Echoes the words of an input that valueFrom gives, joined by an item separator.
# Copyright (c) 2026 The uwe Contributors
# SPDX-License-Identifier: MIT or Apache-2.0
cwlVersion: v1.2
class: CommandLineTool
baseCommand: echo
inputs:
  words:
    type: string[]
    inputBinding:
      valueFrom: $(self)
      itemSeparator: ","
outputs: []
