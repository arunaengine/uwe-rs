include { loadIridaSampleIds } from 'plugin/nf-iridanext'
process ASSEMBLY {
  publishDir "${params.outdir}/assembly", mode: 'copy'
  input: val meta
  output: tuple val(meta), path("${meta.id}.assembly.fa.gz")
  script: "echo '>c' | gzip > ${meta.id}.assembly.fa.gz"
}
process SUMMARY {
  publishDir "${params.outdir}/summary", mode: 'copy'
  input: val ids
  output: path 'summary.txt.gz'
  script: "echo ${ids.join(' ')} | gzip > summary.txt.gz"
}
process META {
  publishDir "${params.outdir}/meta", mode: 'copy'
  input: val ids
  output:
  path 'output.csv'
  path 'output.json'
  script:
  """
  printf 'column1,b,c\\nSAMPLE1,,3\\nSAMPLE2,4,5\\nOTHER,6,7\\n' > output.csv
  printf '{"SAMPLE1": {"k": {"x": 1, "y": [true, "a"]}}, "SAMPLE3": {"k": null}}' > output.json
  """
}
workflow {
  ch = channel.of([id: 'SAMPLE1'], [id: 'SAMPLE2'])
  extra = channel.of([[id: 'SAMPLE3'], 'x']).loadIridaSampleIds()
  extra.view { "loaded ${it}" }
  ASSEMBLY(ch)
  ids = ch.map { it.id }.collect()
  SUMMARY(ids)
  META(ids)
}
