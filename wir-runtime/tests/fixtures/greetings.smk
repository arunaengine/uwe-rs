# Greets each name of the config in a file of its own below `greetings`, and counts the lines
# (tests/snakemake.rs).
# Copyright (c) 2026 The uwe Contributors
# SPDX-License-Identifier: MIT or Apache-2.0

container: "docker://debian:stable-slim"

rule all:
    input:
        greetings=expand("greetings/{name}.txt", name=config["names"]),
        total="total.txt",

rule greet:
    output: "greetings/{name}.txt"
    params: word=config.get("greeting", "hello")
    shell: "echo '{params.word} {wildcards.name}' > {output}"

rule total:
    input: expand("greetings/{name}.txt", name=config["names"])
    output: "total.txt"
    shell: "cat {input} | wc -l > {output}"
