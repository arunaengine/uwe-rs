/*
 * The data functions of the nf-boost plugin 0.8.0, in Nextflow script on uwe's Groovy library.
 */

// The text of a file, or the text itself.
def boostText(source) {
    source instanceof java.nio.file.Path ? source.text : source.toString()
}

// JSON text or a JSON file as maps, lists and values.
def fromJson(source) {
    new groovy.json.JsonSlurper().parseText(boostText(source))
}

def toJson(value, pretty = false) {
    def text = groovy.json.JsonOutput.toJson(value)
    pretty ? groovy.json.JsonOutput.prettyPrint(text) : text
}

// YAML text or a YAML file; several documents give a list of them.
def fromYaml(source) {
    def parts = boostText(source).split(/(?m)^---[ \t]*$/).findAll { part -> part.trim() }
    def documents = parts.collect { part -> new org.yaml.snakeyaml.Yaml().load(part) }
    documents.size() == 1 ? documents[0] : documents
}

def mergeCsv(records, path) {
    mergeCsv([:], records, path)
}

// The records, maps or lists, as lines of quoted values in the file, after an optional header.
def mergeCsv(Map options, records, path) {
    if (!records) {
        throw new IllegalArgumentException('In `mergeCsv` function -- at least one record must be provided')
    }
    def separator = options.sep ?: ','
    def quoted = { value -> '"' + (value == null ? '' : value.toString()) + '"' }
    def keys = options.header == true && records[0] instanceof Map ? records[0].keySet().toList() : null
    def lines = []
    if (keys != null) {
        lines << keys.collect(quoted).join(separator)
    }
    else if (options.header instanceof List) {
        lines << options.header.collect(quoted).join(separator)
    }
    records.each { record ->
        def values = record instanceof Map ? (keys != null ? keys.collect { key -> record[key] } : record.values().toList()) : record
        lines << values.collect(quoted).join(separator)
    }
    path.text = lines.collect { line -> line + '\n' }.join('')
    return null
}

def mergeText(items, path) {
    mergeText([:], items, path)
}

// Appends the items, files or texts, to the file, without the first `skip` lines of each.
def mergeText(Map options, items, path) {
    if (!items) {
        throw new IllegalArgumentException('In `mergeText` function -- at least one item must be provided')
    }
    def keepHeader = options.keepHeader as boolean
    def skip = options.skip != null ? options.skip : (keepHeader ? 1 : 0)
    if (keepHeader && skip < 1) {
        throw new IllegalArgumentException('In function `mergeText` -- `skip` must be greater than zero when `keepHeader` is specified')
    }
    def merged = path.exists() ? path.text : ''
    items.eachWithIndex { item, index ->
        def text
        if (item instanceof java.nio.file.Path || item instanceof File) {
            text = item.text
        }
        else if (item instanceof CharSequence) {
            text = item.toString()
        }
        else if (item instanceof byte[]) {
            text = new String(item)
        }
        else {
            throw new IllegalArgumentException("In function `mergeText` -- invalid item [${item.getClass().name}]: ${item}")
        }
        def lines = []
        def start = 0
        while (start < text.length()) {
            def end = text.indexOf('\n', start)
            def next = end < 0 ? text.length() : end + 1
            lines << text.substring(start, next)
            start = next
        }
        merged += (keepHeader && index == 0 ? lines : lines.drop(skip)).join('')
        if (options.newLine) {
            merged += '\n'
        }
    }
    path.text = merged
    return null
}

// A Groovy template, a file or a text, filled from the binding.
def template(source, Map binding) {
    new groovy.text.SimpleTemplateEngine().createTemplate(boostText(source)).make(binding).toString()
}
