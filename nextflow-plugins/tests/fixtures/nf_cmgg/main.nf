process MAKE {
    publishDir params.outdir, mode: 'copy'

    input:
    val id

    output:
    path "${id}*"

    script:
    """
    echo bam > ${id}.bam
    echo r1 > ${id}_R1.fastq.gz
    echo r2 > ${id}_R2.fastq.gz
    """
}

process METRICS {
    publishDir params.outdir, mode: 'copy'

    output:
    path 'metrics.tsv'

    script:
    """
    printf 'Sample\\tyield\\nS1\\t150\\nS2\\t50\\nS10\\t300\\nS99\\t1\\n' > metrics.tsv
    """
}

workflow {
    MAKE(channel.of('S1', 'S2', 'S10', 'X9'))
    METRICS()
}
