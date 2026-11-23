# Numbers the lines of a text below a line that names its sample, as a Snakemake rule.
# Copyright (c) 2026 The uwe Contributors
# SPDX-License-Identifier: MIT or Apache-2.0


rule number:
    input:
        text="texts/{sample}.txt",
    output:
        numbered="numbered/{sample}.txt",
    container:
        "docker://debian:stable-slim"
    shell:
        "(echo '# {wildcards.sample}'; cat -n {input.text}) > {output.numbered}"
