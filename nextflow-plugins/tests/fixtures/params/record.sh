#!/bin/sh
# Records nf-schema 2.5.1 parameter oracle output with Nextflow 26.04.6.
# Copyright (c) 2026 The uwe Contributors
# SPDX-License-Identifier: MIT or Apache-2.0
nextflow -q run main.nf --outdir res --n 7
nextflow -q run main.nf --outdir res --mode c --input 'a b.txt'
nextflow -q run main.nf
