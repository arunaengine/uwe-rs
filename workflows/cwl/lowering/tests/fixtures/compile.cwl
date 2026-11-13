#!/usr/bin/env cwl-runner
# Compiles a source file into the output folder, which an argument names through runtime.
# Copyright (c) 2026 The uwe Contributors
# SPDX-License-Identifier: MIT or Apache-2.0
cwlVersion: v1.2
class: CommandLineTool
requirements:
  ResourceRequirement: {coresMin: 2}
baseCommand: javac
arguments: ["-d", $(runtime.outdir), {prefix: "-J-Xss", separate: false, valueFrom: $(runtime.cores)}]
inputs:
  src:
    type: File
    inputBinding: {position: 1}
outputs:
  classfile:
    type: File
    outputBinding: {glob: "*.class"}
