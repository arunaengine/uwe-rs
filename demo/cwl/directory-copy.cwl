# Copies text files from a Directory input and collects wildcard outputs.
# Copyright (c) 2026 The uwe Contributors
# SPDX-License-Identifier: MIT or Apache-2.0

cwlVersion: v1.2
class: CommandLineTool
requirements:
  DockerRequirement:
    dockerPull: debian:stable-slim
baseCommand: [sh, -c]
arguments:
  - 'cp "$1"/*.txt .'
  - copy
inputs:
  folder:
    type: Directory
    inputBinding:
      position: 1
outputs:
  copies:
    type:
      type: array
      items: File
    outputBinding:
      glob: '*.txt'
