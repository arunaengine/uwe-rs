#!/usr/bin/env cwl-runner
# Appends to a file in place, which a wir task cannot do yet.
# Copyright (c) 2026 The uwe Contributors
# SPDX-License-Identifier: MIT or Apache-2.0
cwlVersion: v1.2
class: CommandLineTool
requirements:
  InplaceUpdateRequirement: {inplaceUpdate: true}
  InitialWorkDirRequirement:
    listing:
      - {entry: $(inputs.log), writable: true}
baseCommand: [sh, -c, "echo done >> log.txt"]
inputs:
  log: File
outputs: []
