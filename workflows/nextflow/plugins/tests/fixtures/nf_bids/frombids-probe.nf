include { fromBIDS } from 'plugin/nf-bids'

def norm(v, String root) {
    if (v instanceof java.nio.file.Path) {
        return 'Path:' + v.toString().replace(root, '<ds>')
    }
    if (v instanceof Map) {
        return v.collectEntries { k, x -> [k.toString(), norm(x, root)] }
    }
    if (v instanceof List) {
        return v.collect { x -> norm(x, root) }
    }
    if (v instanceof String) {
        return v.replace(root, '<ds>')
    }
    return v == null ? null : (v instanceof Number || v instanceof Boolean ? v : v.getClass().simpleName + ':' + v.toString())
}

workflow {
    def root = file(params.dataset).toAbsolutePath().toString()
    def opts = params.opts ? new groovy.json.JsonSlurper().parseText(params.opts.toString()) : [:]
    channel.fromBIDS(params.dataset, params.config, opts)
        .map { item -> groovy.json.JsonOutput.toJson(norm(item, root)) }
        .toSortedList()
        .subscribe { items ->
            println "items: ${items.size()}"
            items.each { println it }
        }
}
