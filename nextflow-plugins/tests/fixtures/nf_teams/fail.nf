process SAY {
  tag "$x"
  publishDir "${projectDir}/results", mode: 'copy'
  input: val x
  output: path "${x}.txt"
  script: "echo oops >&2; exit 2"
}
workflow {
  SAY(channel.of('a','b'))
  workflow.onComplete {
  }
}
