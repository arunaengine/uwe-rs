# Hello world in Snakemake: one job per greeting, then one that collects them.
# Copyright (c) 2026 The uwe Contributors
# SPDX-License-Identifier: MIT or Apache-2.0

workdir: workflow.basedir

GREETINGS = ["Hello", "Hola", "Bonjour", "Ciao"]


rule all:
    input:
        "hello/all.txt"


rule greet:
    output:
        "hello/{greeting}.txt"
    container:
        "docker://debian:stable-slim"
    shell:
        "echo '{wildcards.greeting} world!' > {output}"


rule collect:
    input:
        expand("hello/{greeting}.txt", greeting=GREETINGS)
    output:
        "hello/all.txt"
    container:
        "docker://debian:stable-slim"
    shell:
        "cat {input} > {output}"
