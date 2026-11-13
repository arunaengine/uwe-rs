process STEP_ONE {
    tag "${meta.id}"
    input:
    tuple val(meta), val(x)
    output:
    tuple val(meta), path('one.txt')
    script:
    """
    echo "${x}" > one.txt
    """
}
process STEP_TWO {
    tag "${meta.id}"
    input:
    tuple val(meta), path(f)
    output:
    stdout
    script:
    """
    cat ${f}
    """
}
process NO_META {
    input:
    val x
    script:
    """
    echo ${x}
    """
}
workflow {
    ch = channel.of([[id: 's1', group: 'g'], 1], [[id: 's2', group: 'g'], 2])
    STEP_TWO(STEP_ONE(ch))
    NO_META(channel.of(9))
}
