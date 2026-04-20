#!/bin/sh
# Records the expected output with Nextflow 26.04.6 and nf-validation 1.1.3, which needs the
# legacy parser, in a copy of this directory; tests/nf_validation.rs has the results.
export NXF_SYNTAX_PARSER=v1
nextflow run main.nf -ansi-log false
nextflow run invalid.nf -ansi-log false --n x --mode c --outdir ''
