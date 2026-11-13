process WORK {
  tag "${meta.id}"
  errorStrategy { meta.id == 'S3' ? 'ignore' : (task.attempt < 2 && meta.id == 'S2' ? 'retry' : 'terminate') }
  maxRetries 2
  cpus 1
  memory '100 MB'
  input:
  tuple val(meta), val(x)
  output:
  tuple val(meta), path("${meta.id}.txt")
  script:
  """
  if [ "${meta.id}" = "S3" ]; then exit 3; fi
  if [ "${meta.id}" = "S2" ] && [ "${task.attempt}" = "1" ]; then exit 1; fi
  echo ${x} > ${meta.id}.txt
  """
}
process MERGE {
  input:
  tuple val(meta), path(f)
  output:
  path "merged_${meta.id}.txt"
  script:
  "cat ${f} > merged_${meta.id}.txt"
}
workflow {
  ch = channel.of([[id: 'S1', sample_id: ['A', 'B']], 1], [[id: 'S2', sample_id: 'C'], 2], [[id: 'S3', sample_id: 'D'], 3])
  WORK(ch) | MERGE
}
