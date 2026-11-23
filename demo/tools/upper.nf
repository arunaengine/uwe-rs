// Turns the text of a sample into capital letters, as a Nextflow process with a meta map.
// Copyright (c) 2026 The uwe Contributors
// SPDX-License-Identifier: MIT or Apache-2.0

process UPPER {
    container 'debian:stable-slim'

    input:
    tuple val(meta), path(text)

    output:
    tuple val(meta), path("${meta.id}.upper.txt"), emit: upper

    script:
    """
    tr a-z A-Z < ${text} > ${meta.id}.upper.txt
    """
}
