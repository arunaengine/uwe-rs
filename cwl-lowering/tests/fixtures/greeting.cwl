#!/usr/bin/env cwl-runner
# Greets a person of a named record type that SchemaDefRequirement defines.
# Copyright (c) 2026 The uwe Contributors
# SPDX-License-Identifier: MIT or Apache-2.0
cwlVersion: v1.2
class: CommandLineTool
requirements:
  SchemaDefRequirement:
    types:
      - {name: Mood, type: enum, symbols: [happy, sad]}
      - name: Person
        type: record
        fields:
          name: string
          mood: Mood
baseCommand: echo
inputs:
  person: Person
  mood: {type: Mood, inputBinding: {position: 1}}
outputs:
  greeting: stdout
