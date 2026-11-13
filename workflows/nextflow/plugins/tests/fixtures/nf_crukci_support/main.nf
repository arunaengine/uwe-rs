include { javaMemMB; javaMemoryOptions; sizeOf; makeCollection; safeName; logException } from 'plugin/nf-crukci-support'
process JAVA {
  memory '2 GB'
  output: stdout
  script:
  def m = javaMemoryOptions(task)
  "echo heap=${m.heap} meta=${m.metaSpace} misc=${m.misc} all=${m.all} opts='${m.jvmOpts}' mb=${javaMemMB(task)}"
}
process SMALL {
  memory '200 MB'
  output: stdout
  script:
  "echo heap=${javaMemMB(task)}"
}
process OOM {
  output: stdout
  script: "echo 'slurmstepd: error: Exceeded job memory limit'; sleep 12; echo done"
}
workflow {
  println "sizeOf: ${sizeOf([1,2,3])} ${sizeOf('x')} ${sizeOf(null)} ${sizeOf([a:1,b:2])}"
  println "makeCollection: ${makeCollection('f.txt')} ${makeCollection(['a','b'])} ${makeCollection(null)}"
  println "safeName: [${safeName('My Sample: Run 1 (2026)')}] [${safeName('Data/File.txt')}] [${safeName('Tab\tRemoved')}] [${safeName('Ünïcode-ß.txt')}] [${safeName(null)}]"
  JAVA().view()
  OOM().view()
  if (params.small) SMALL().view()
  if (params.boom) { try { throw new IllegalStateException('demo failure') } catch (Exception e) { logException(e) } }
}
