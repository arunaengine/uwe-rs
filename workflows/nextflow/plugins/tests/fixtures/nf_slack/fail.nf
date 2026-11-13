include { slackMessage; slackFileUpload } from 'plugin/nf-slack'
process SAY {
  tag "$x"
  publishDir "${projectDir}/results", mode: 'copy'
  input: val x
  output: path "${x}.txt"
  script: "echo oops >&2; exit 2"
}
workflow {
  slackMessage("Processing *two* samples")
  slackMessage([message: "Rich text", color: "#2EB887", fields: [[title: "Samples", value: "2", short: true], [title: "Mode", value: "test"]]])
  SAY(channel.of('a','b'))
  workflow.onComplete {
    slackFileUpload(file: "${projectDir}/results/a.txt", title: 'A result', comment: 'first file', filename: 'a-result.txt')
  }
}
