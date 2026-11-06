/*
 * The functions of the nf-core-utils plugin 0.5.0 that nf-core pipelines call, ported to
 * Nextflow script. The plugin writes its log messages to the log file only, never to the
 * console, so this port leaves them out.
 */

// The attribute of the selected genome in `params.genomes`, or null.
def getGenomeAttribute(attribute) {
    def genomes = params.genomes
    def genome = params.genome
    if (!(genomes instanceof Map) || !(genome instanceof String)) {
        return null
    }
    if (!genomes.containsKey(genome)) {
        return null
    }
    def selected = genomes[genome]
    if (!(selected instanceof Map) || !selected.containsKey(attribute)) {
        return null
    }
    return selected[attribute]
}

// The version with a `v` in front, and the short commit id if one is given.
def getWorkflowVersion(version = null, commitId = null) {
    def number = version ?: workflow.manifest.version
    def text = ''
    if (number) {
        def prefix = number[0] != 'v' ? 'v' : ''
        text += "${prefix}${number}"
    }
    if (commitId) {
        text += "-g${commitId.substring(0, 7)}"
    }
    return text
}

// The plugin looks for `profile` and `configFiles` in the config, which has neither, so it
// always finds no custom configuration.
def checkConfigProvided() {
    return false
}

// Fails for a `-profile` value that ends with a comma. A positional argument only gives a
// message in the log file.
def checkProfileProvided(args, monochromeLogs = true) {
    def index = args.indexOf('-profile')
    def profile = index >= 0 && index + 1 < args.size() ? args[index + 1] : null
    if (profile?.endsWith(',')) {
        def red = monochromeLogs ? '' : "\033[0;31m"
        def yellow = monochromeLogs ? '' : "\033[0;33m"
        def reset = monochromeLogs ? '' : "\033[0m"
        throw new IllegalArgumentException(
            "${red}ERROR${reset} ~ The `-profile` option cannot end with a trailing comma, please remove it and re-run the pipeline!\n" +
            "${yellow}HINT${reset}: A common mistake is to provide multiple values separated by spaces e.g. `-profile test, docker`.\n"
        )
    }
}

// The plugin only logs the outcome of the run.
def completionSummary(monochromeLogs = true) {
}

// Writes the params as JSON to `pipeline_info/params_<time>.json` in the output directory.
def dumpParametersToJSON(outdir, parameters) {
    if (outdir == null || parameters == null) {
        return
    }
    try {
        def timestamp = new java.util.Date().format('yyyy-MM-dd_HH-mm-ss')
        def generator = new groovy.json.JsonGenerator.Options()
            .excludeNulls()
            .addConverter(Path) { path -> path.toUriString() }
            .addConverter(Duration) { duration -> duration.toMillis() }
            .addConverter(MemoryUnit) { memory -> memory.toBytes() }
            .build()
        def directory = file(outdir).resolve('pipeline_info')
        directory.mkdirs()
        def json = groovy.json.JsonOutput.prettyPrint(generator.toJson(parameters))
        directory.resolve("params_${timestamp}.json").text = json
    }
    catch (Exception e) {
    }
}

// The software versions as YAML, by process and tool, then the workflow and Nextflow
// versions. A channel of versions gives a channel of that text.
def softwareVersionsToYAML(versionsOrOptions, options = [:]) {
    def versions = versionsOrOptions
    def nextflowVersion = options?.nextflowVersion
    if (versionsOrOptions instanceof Map && versionsOrOptions.containsKey('softwareVersions')) {
        versions = versionsOrOptions.softwareVersions
        nextflowVersion = versionsOrOptions.nextflowVersion ?: nextflowVersion
    }
    if (versions instanceof List) {
        return versionsText(versions, nextflowVersion)
    }
    return versions.toList().map { list -> versionsText(list, nextflowVersion) }
}

def versionsText(versions, nextflowVersion) {
    def merged = [:]
    versions.each { entry -> addVersions(entry, merged) }
    def sorted = merged.sort().collectEntries { name, tools -> [(name): tools.sort()] }
    def yaml = sorted ? new org.yaml.snakeyaml.Yaml().dumpAsMap(sorted).trim() : ''
    def name = workflow.manifest.name ?: 'unknown'
    def version = nextflowVersion?.toString() ?: System.getenv('NXF_VER') ?: 'unknown'
    def workflowYaml = "Workflow:\n    ${name}: ${getWorkflowVersion()}\n    Nextflow: ${version}"
    return [yaml, workflowYaml].findAll { text -> text && text != '{}' }.join('\n').trim()
}

// Adds the versions of one entry: YAML text or a file of it, a map, or a list of
// `[process, tool, version]` or of such lists.
def addVersions(entry, merged) {
    if (entry == null) {
        return
    }
    try {
        if (entry instanceof Map) {
            mergeVersions(entry, merged)
        }
        else if (entry instanceof Path) {
            if (entry.exists() && entry.isFile()) {
                addYamlVersions(entry.text, merged)
            }
        }
        else if (entry instanceof List) {
            if (!entry.isEmpty() && entry[0] instanceof List) {
                entry.each { item -> addVersions(item, merged) }
            }
            else if (entry.size() >= 3) {
                def process = entry[0]?.toString() ?: ''
                def name = process.contains(':') ? process.substring(process.lastIndexOf(':') + 1) : process
                def tool = entry[1]?.toString()
                if (name && tool) {
                    addVersion(merged, name, tool, entry[2])
                }
            }
        }
        else {
            def text = entry.toString().trim()
            if (text) {
                def source = new File(text)
                addYamlVersions(source.exists() && source.isFile() ? source.text : text, merged)
            }
        }
    }
    catch (Exception e) {
    }
}

def addYamlVersions(text, merged) {
    def yaml = new org.yaml.snakeyaml.Yaml()
    def loaded = yaml.load(text)
    if (!(loaded instanceof Map)) {
        return
    }
    def versions = loaded.collectEntries { key, value ->
        key instanceof String ? [key.tokenize(':')[-1], value] : [key, value]
    }
    def processed = yaml.dumpAsMap(versions).trim()
    if (processed) {
        def map = yaml.load(processed)
        if (map instanceof Map) {
            mergeVersions(map, merged)
        }
    }
}

// Process blocks like `FASTQC: {fastqc: 0.12.1}` by process; tools outside a block go under
// `Software`.
def mergeVersions(parsed, merged) {
    if (!parsed) {
        return
    }
    if (parsed.values().any { value -> value instanceof Map }) {
        parsed.each { key, value ->
            if (value instanceof Map) {
                mergeTools(key?.toString(), value, merged)
            }
            else if (key) {
                mergeTools('Software', [(key.toString()): value], merged)
            }
        }
    }
    else {
        mergeTools('Software', parsed, merged)
    }
}

def mergeTools(name, tools, merged) {
    if (!name || !tools) {
        return
    }
    tools.each { key, value ->
        def text = key?.toString()
        def tool = text ? (text.contains(':') ? text.tokenize(':').last() : text) : null
        if (tool) {
            addVersion(merged, name, tool, value)
        }
    }
}

def addVersion(merged, name, tool, version) {
    if (!merged.containsKey(name)) {
        merged[name] = [:]
    }
    merged[name][tool] = version instanceof CharSequence ? version.toString() : version
}
