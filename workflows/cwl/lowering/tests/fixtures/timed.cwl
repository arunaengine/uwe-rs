#!/usr/bin/env cwl-runner
# Sleeps for a minute per core, a time limit that an expression sets from the runtime object.
# Copyright (c) 2026 The uwe Contributors
# SPDX-License-Identifier: MIT or Apache-2.0
cwlVersion: v1.2
class: CommandLineTool
requirements:
  InlineJavascriptRequirement: {}
  ResourceRequirement: {coresMin: 2}
  ToolTimeLimit:
    timelimit: $(runtime.cores * 60)
baseCommand: [sleep, "1"]
inputs: []
outputs: []
