#!/usr/bin/env cwl-runner
# Writes a greeting and the node that made it.
cwlVersion: v1.2
class: CommandLineTool

requirements:
  DockerRequirement:
    dockerPull: debian:stable-slim

inputs:
  name: string

baseCommand: [sh, -c]
arguments:
  - printf 'Hello, %s!\ngreeted on %s\n' "$0" "`cat /proc/sys/kernel/hostname`" > "$0.txt"
  - $(inputs.name)

outputs:
  greeting:
    type: File
    outputBinding:
      glob: $(inputs.name).txt
