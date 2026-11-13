params.greeting = 'hello'
process SAY {
  tag "$x"
  publishDir "${projectDir}/results", mode: 'copy'
  input: val x
  output: path "${x}.txt"
  script: "echo ${params.greeting} ${x} > ${x}.txt"
}
process JOIN {
  publishDir "${projectDir}/results", mode: 'copy'
  input: path xs
  output: path 'report.html'
  script: "cat ${xs} > report.html"
}
workflow { JOIN(SAY(channel.of('a','b')).collect()) }
