params.outdir = 'results'
params.tag = 'a'
process MAKE {
    publishDir "${params.outdir}/make", mode: 'copy'
    output:
    path '*'
    script:
    """
    echo "hello ${params.tag}" > hello.txt
    echo "fixed" > fixed.txt
    echo "log ${params.tag}" > run.log
    printf 'x%.0s' {1..100} > big.dat
    echo "zipped" | gzip > data.txt.gz
    """
}
workflow { MAKE() }
