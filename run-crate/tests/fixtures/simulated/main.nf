process UPPER {
    input:
    path text
    output:
    path 'upper.txt'
    script:
    "tr a-z A-Z < $text > upper.txt"
}
