process BOOM {
    input:
    val x
    output:
    path 'out.txt'
    script:
    """
    echo ${x} > out.txt
    exit 3
    """
}
workflow { BOOM(channel.of(1)) }
