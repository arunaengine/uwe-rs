process SLOW {
  output: val 'done'
  script: "sleep 1"
}
process BAD {
  input: val x
  script: "echo boom >&2; exit 3"
}
workflow { BAD(SLOW()) }
