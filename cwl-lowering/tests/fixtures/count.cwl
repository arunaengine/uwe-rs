#!/usr/bin/env cwl-runner
# Counts the lines of a file into count.txt, which a glob reads back.
# Copyright (c) 2026 The uwe Contributors
# SPDX-License-Identifier: MIT or Apache-2.0
cwlVersion: v1.2
class: CommandLineTool
requirements:
  DockerRequirement: {dockerPull: "docker.io/library/debian:12-slim"}
  ResourceRequirement: {coresMin: 2, ramMin: 64}
baseCommand: [wc, -l]
inputs:
  text:
    type: File
    inputBinding: {position: 1}
stdout: count.txt
outputs:
  count:
    type: File
    outputBinding: {glob: count.txt}
