process WORK {
    cpus 1
    memory '1 GB'
    input:
    val x
    output:
    stdout
    script:
    """
    echo "task ${x}"
    """
}

workflow {
    WORK(channel.of(1, 2, 3)).view()
}
