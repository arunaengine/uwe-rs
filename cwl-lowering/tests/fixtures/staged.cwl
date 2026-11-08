#!/usr/bin/env cwl-runner
# Indexes a genome in the working folder, beside a config file that the listing writes.
# Copyright (c) 2026 The uwe Contributors
# SPDX-License-Identifier: MIT or Apache-2.0
cwlVersion: v1.2
class: CommandLineTool
requirements:
  InitialWorkDirRequirement:
    listing:
      - $(inputs.genome)
      - entryname: index.conf
        entry: |
          name=$(inputs.name)
baseCommand: [index]
arguments: [$(inputs.genome)]
inputs:
  genome: File
  name: string
outputs:
  indexed:
    type: File
    outputBinding: {glob: $(inputs.genome.basename)}
