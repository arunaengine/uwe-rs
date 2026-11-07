process SAY {
  tag "$x"
  input: tuple val(x), path(seed)
  output: path "${x}.txt"
  script: "cat ${seed} > ${x}.txt; echo hello ${x} >> ${x}.txt"
}
process JOIN {
  input: path xs
  output: path 'joined.txt'
  script: "cat ${xs} > joined.txt"
}
process COUNT {
  input: path j
  output: path 'count.txt'
  script: "wc -l ${j} > count.txt"
}
workflow {
  seed = file("${projectDir}/data/seed.txt")
  SAY(channel.of('a','b').map { [it, seed] })
  COUNT(JOIN(SAY.out.collect()))
}
