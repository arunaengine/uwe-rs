include { sayHello } from 'plugin/nf-logsaver'
process SAY_IT {
    tag "${x}"
    input:
    val x
    output:
    stdout
    script:
    """
    echo "out ${x}"
    echo "err ${x}" >&2
    """
}
workflow sub {
    take: ch
    main: SAY_IT(ch)
}
workflow {
    sayHello('probe')
    sub(channel.of('A', 'b'))
}
