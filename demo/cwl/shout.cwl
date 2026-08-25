#!/usr/bin/env cwl-runner
# Shouts the first line of a greeting, and adds the node that shouted it.
cwlVersion: v1.2
class: CommandLineTool

requirements:
  DockerRequirement:
    dockerPull: debian:stable-slim

inputs:
  greeting: File

baseCommand: [sh, -c]
arguments:
  - '{ head -n 1 "$0" | tr a-z A-Z; tail -n +2 "$0"; echo "shouted on `cat /proc/sys/kernel/hostname`"; } > "$1.loud.txt"'
  - $(inputs.greeting.path)
  - $(inputs.greeting.nameroot)

outputs:
  loud:
    type: File
    outputBinding:
      glob: $(inputs.greeting.nameroot).loud.txt
