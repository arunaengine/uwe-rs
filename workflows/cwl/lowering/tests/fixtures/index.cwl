#!/usr/bin/env cwl-runner
# Sorts an indexed BAM file and indexes the result, with secondary files on both sides.
# Copyright (c) 2026 The uwe Contributors
# SPDX-License-Identifier: MIT or Apache-2.0
cwlVersion: v1.2
class: CommandLineTool
baseCommand: [sort-and-index]
inputs:
  bam:
    type: File
    secondaryFiles: [.bai, ^.txt?]
    inputBinding: {position: 1}
outputs:
  sorted:
    type: File
    secondaryFiles: [.bai]
    outputBinding: {glob: sorted.bam}
