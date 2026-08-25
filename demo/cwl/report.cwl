#!/usr/bin/env cwl-runner
# Joins the shouted greetings into one report, with Python.
cwlVersion: v1.2
class: CommandLineTool

requirements:
  DockerRequirement:
    dockerPull: python:3.13-slim

inputs:
  loud:
    type: File[]
    inputBinding:
      position: 2

baseCommand: [python3, -c]
arguments:
  - position: 0
    valueFrom: |
      import pathlib, sys
      language, files = sys.argv[1], sorted(sys.argv[2:], key=lambda f: pathlib.Path(f).name)
      lines = [f"The iunctus demo in {language}: {len(files)} greetings", ""]
      for name in files:
          text = pathlib.Path(name).read_text().splitlines()
          lines.append(f"{text[0]:<14} {', '.join(text[1:])}")
      pathlib.Path("report.txt").write_text("\n".join(lines) + "\n")
  - position: 1
    valueFrom: CWL

outputs:
  report:
    type: File
    outputBinding:
      glob: report.txt
