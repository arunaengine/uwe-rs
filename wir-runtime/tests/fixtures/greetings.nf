// Greets each name in a file of its own, publishes the files below `params.outdir`, and
// emits them from the workflow `greetings` (tests/nextflow.rs).
// Copyright (c) 2026 The uwe Contributors
// SPDX-License-Identifier: MIT or Apache-2.0

params.names = ['ada']
params.greeting = 'hello'
params.outdir = 'results'

process GREET {
    container 'debian:stable-slim'
    publishDir "${params.outdir}/greetings", mode: 'copy'

    input:
    val name

    output:
    path "${name}.txt"

    script:
    """
    echo '${params.greeting} ${name}' > ${name}.txt
    """
}

workflow greetings {
    main:
    GREET(channel.fromList(params.names))

    emit:
    files = GREET.out
    count = GREET.out.count()
}

workflow {
    greetings()
}
