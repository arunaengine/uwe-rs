
process SPLIT {
    publishDir "${projectDir}/results/split", mode: 'copy'
    input:
    path infile
    output:
    path 'part_*.txt'
    script:
    """
    split -l 1 -d --additional-suffix=.txt ${infile} part_
    """
}

process SHOUT {
    publishDir "${projectDir}/results/shout", mode: 'copy'
    input:
    path part
    output:
    path "${part.baseName}.up.txt"
    script:
    """
    echo ${params.greeting} > ${part.baseName}.up.txt
    tr a-z A-Z < ${part} >> ${part.baseName}.up.txt
    """
}

workflow {
    SPLIT(file(params.input))
    SHOUT(SPLIT.out.flatten())
}
