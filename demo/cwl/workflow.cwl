#!/usr/bin/env cwl-runner
# The iunctus demo in CWL: three greetings, each made and shouted by its own task (on any node
# of the cluster), then one report that joins them, made with Python.
# Run it with `iunctus run workflow.cwl`; the names have a default, so no job file is needed.
cwlVersion: v1.2
class: Workflow

requirements:
  ScatterFeatureRequirement: {}

inputs:
  names:
    type: string[]
    default: [Ada, Grace, Linus]

outputs:
  greetings:
    type: File[]
    outputSource: greet/greeting
  report:
    type: File
    outputSource: report/report

steps:
  greet:
    run: greet.cwl
    scatter: name
    in:
      name: names
    out: [greeting]
  shout:
    run: shout.cwl
    scatter: greeting
    in:
      greeting: greet/greeting
    out: [loud]
  report:
    run: report.cwl
    in:
      loud: shout/loud
    out: [report]
