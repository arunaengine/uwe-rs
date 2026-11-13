include { annotateArtifact; getRunUid; getTransformUid; getInstanceSlug } from 'plugin/nf-lamin'
process SAY {
  tag "$x"
  input: val x
  output: path "${x}.txt"
  script: "echo hello ${x} > ${x}.txt"
}
workflow {
  log.info "lamin instance=${getInstanceSlug()} run=${getRunUid()} transform=${getTransformUid()}"
  annotateArtifact(file('s3://demo-bucket/results/out.txt'), kind: 'dataset', description: 'Annotated output', ulabel_uids: ['+annotated'])
  SAY(channel.of('a'))
}
