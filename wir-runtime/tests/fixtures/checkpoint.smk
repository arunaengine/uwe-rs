# Picks the file to make in a checkpoint, then makes it and copies it to `final.txt`
# (tests/snakemake.rs).
# Copyright (c) 2026 The uwe Contributors
# SPDX-License-Identifier: MIT or Apache-2.0

def chosen(wildcards):
    with open(checkpoints.pick.get().output[0]) as manifest:
        return manifest.read().strip()

rule all:
    input: final="final.txt"

rule finish:
    input: chosen
    output: "final.txt"
    shell: "cat {input} > {output}"

checkpoint pick:
    output: "choice.txt"
    shell: "echo made/chosen.txt > {output}"

rule generate:
    output: "made/{sample}.txt"
    shell: "echo {wildcards.sample} > {output}"
