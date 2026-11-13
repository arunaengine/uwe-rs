#!/usr/bin/env cwl-runner
# Prints an environment variable that EnvVarRequirement sets, beside one from an input.
# Copyright (c) 2026 The uwe Contributors
# SPDX-License-Identifier: MIT or Apache-2.0
cwlVersion: v1.2
class: CommandLineTool
requirements:
  EnvVarRequirement:
    envDef: {GREETING: hello, NAME: $(inputs.name)}
baseCommand: [printenv, GREETING]
inputs:
  name: string
stdout: greeting.txt
outputs:
  greeting: stdout
