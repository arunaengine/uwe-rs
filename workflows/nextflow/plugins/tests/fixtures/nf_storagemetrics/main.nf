include { sayHello } from 'plugin/nf-storagemetrics'
process MAKE {
    tag "t${x}"
    input:
    val x
    output:
    path 'data.bin'
    script:
    """
    head -c \$(( ${x} * 1500 )) /dev/zero > data.bin
    """
}
workflow {
    sayHello('probe')
    MAKE(channel.of(1, 2, 1000))
}
