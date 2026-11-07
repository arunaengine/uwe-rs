/*
 * Runs a task for each full group of groupTupleBy with a size while the source still runs.
 * Copyright (c) 2026 The uwe Contributors
 * SPDX-License-Identifier: MIT or Apache-2.0
 */

include { groupTupleBy } from 'plugin/nf-bids'

process MAKE {
    input: val item
    output: val item
    script: "make ${item[1]}"
}

process USE {
    input: val group
    output: val group
    script: "use ${group[0]}"
}

workflow {
    MAKE(channel.of(['a', 1], ['a', 2], ['b', 3]))
    USE(MAKE.out.groupTupleBy({ it[0] }, [size: 2]))
}
