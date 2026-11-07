process MAPPED {
  container 'ubuntu:24.04'
  output: stdout
  script: "echo mapped"
}
process MISSING {
  container 'quay.io/biocontainers/fastqc:0.12.1--hdfd78af_0'
  output: stdout
  script: "echo missing"
}
workflow { MAPPED().view(); MISSING().view() }
