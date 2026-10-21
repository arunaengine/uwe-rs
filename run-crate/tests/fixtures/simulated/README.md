# Simulated run fixture-run of main.nf

Simulated run: uwe did not run the tasks. Its simulator made up their output files, so the files, digests and times in this crate are not results of the tools. A run with real data gives the real results.

This folder is a Workflow Run RO-Crate of the Provenance Run Crate profile 0.5, written by uwe 0.1.0.

- `ro-crate-metadata.json` describes the run: the workflow, its steps, and one action per task with its command, container image, inputs, outputs, start and end.
- `main.nf` is the workflow that ran.
- `inputs.schema.json` is the JSON Schema of the inputs of the workflow.
- `outputs.schema.json` is the JSON Schema of the outputs of the workflow.
- `workflow/` holds the files of the workflow as the run had them.
- `data/` holds each file of the run once, named by its SHA-256 digest; the metadata gives each file its name in the run.

The run succeeded: started 2026-09-30T17:26:29.447+00:00, ended 2026-09-30T17:26:29.449+00:00, 2 tasks.
