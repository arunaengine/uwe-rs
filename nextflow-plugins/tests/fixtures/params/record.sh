#!/bin/sh
# Records the expected output with Nextflow 26.04.6 and nf-schema 2.5.1 in a copy of this
# directory, with the nextflow.config and main.nf that the comments of tests/nf_schema.rs show.
nextflow -q run main.nf --outdir res --n 7
nextflow -q run main.nf --outdir res --mode c --input 'a b.txt'
nextflow -q run main.nf
