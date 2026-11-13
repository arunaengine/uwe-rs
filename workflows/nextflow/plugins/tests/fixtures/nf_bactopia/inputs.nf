include { bactopiaInputs; validateParameters } from 'plugin/nf-bactopia'
workflow {
    def v = validateParameters(false)
    println "validate => data=${v.data} hasErrors=${v.hasErrors}"
    println "validate logs =>\n${v.logs}"
    if (params.mode) {
        def r = bactopiaInputs(params.mode)
        println "inputs hasErrors=${r.hasErrors}"
        println "inputs logs =>\n${r.logs}"
        r.samples.each { println "sample => ${it}" }
    }
}
