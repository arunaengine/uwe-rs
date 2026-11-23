// Counts the words of a text in one process, which `uwe tool run` can also run on its own.
// Copyright (c) 2026 The uwe Contributors
// SPDX-License-Identifier: MIT or Apache-2.0

params.text = "${projectDir}/input.txt"

process COUNT {
    container 'debian:stable-slim'

    input:
    path text

    output:
    path 'count.txt', emit: count

    script:
    """
    wc -w < ${text} > count.txt
    """
}

workflow {
    COUNT(channel.fromPath(params.text)).view { it.text.trim() }
}
