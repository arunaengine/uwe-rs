#!/usr/bin/env cwl-runner
# Prints an environment variable, which a wir task cannot set yet.
# Copyright (c) 2026 The uwe Contributors
# SPDX-License-Identifier: MIT or Apache-2.0
cwlVersion: v1.2
class: CommandLineTool
requirements:
  EnvVarRequirement:
    envDef: {GREETING: hello}
baseCommand: [printenv, GREETING]
inputs: []
stdout: greeting.txt
outputs:
  greeting: stdout
