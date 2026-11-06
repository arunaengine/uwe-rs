/*
 * The functions of the nf-core-utils plugin 0.5.0 to 0.7.0 that need the run, as Nextflow script.
 * Copyright (c) 2026 The uwe Contributors
 * SPDX-License-Identifier: MIT or Apache-2.0
 */

include { coreSettings; condaChannels; deliverMail; postNotification } from 'plugin/nf-core-utils'

// The pinned release as its minor number, 5 for 0.5.0; a release 1.0 or later counts as 100.
def minorRelease() {
    def parts = coreSettings().version.tokenize('.').collect { part -> part.isInteger() ? part.toInteger() : 0 }
    return parts[0] * 100 + (parts.size() > 1 ? parts[1] : 0)
}

// Fails for a function that the pinned release does not have.
def requireRelease(name, first, last = 10000) {
    def release = minorRelease()
    if (release < first || release > last) {
        error("`${name}` is not a function of nf-core-utils ${coreSettings().version}")
    }
}

// Since 0.6.0 the plugin logs to the console; before, its messages went to the log file only.
def coreInfo(text) {
    if (minorRelease() < 6) {
        log.debug(text.toString())
    }
    else {
        log.info(text.toString())
    }
}

def coreWarn(text) {
    if (minorRelease() < 6) {
        log.debug(text.toString())
    }
    else {
        log.warn(text.toString())
    }
}

def coreError(text) {
    if (minorRelease() < 6) {
        log.debug(text.toString())
    }
    else {
        log.error(text.toString())
    }
}

def logColours(monochromeLogs) {
    def names = [reset: "\033[0m", red: "\033[0;31m", green: "\033[0;32m", yellow: "\033[0;33m", purple: "\033[0;35m"]
    return names.collectEntries { name, code -> [(name): monochromeLogs ? '' : code] }
}

def pipelineName() {
    return workflow.manifest.name ?: 'unknown'
}

// The part of a name after its last `:`, like `FASTQC` of `NFCORE_PROBE:FASTQC`.
def afterColon(name) {
    def text = name.toString()
    return text.substring(text.lastIndexOf(':') + 1)
}

// --- References ---

def getReferencesFile(referencesList, param, attribute, basepath) {
    requireRelease('getReferencesFile', 0, 6)
    return referencesList.collect { references ->
        def meta = references[0]
        if (param) {
            return [[id: meta.id], param]
        }
        if (meta[attribute]) {
            return [[id: meta.id], meta[attribute].toString().replace('${params.igenomes_base}', basepath.toString())]
        }
        return null
    }
}

def getReferencesValue(referencesList, param, attribute) {
    requireRelease('getReferencesValue', 0, 6)
    return referencesList.collect { references ->
        def meta = references[0]
        return param || meta[attribute] ? [[id: meta.id], param ?: meta[attribute]] : null
    }
}

// Checks the named arguments of a reference function.
def namedArguments(function, arguments, known, required) {
    if (!(arguments instanceof Map)) {
        throw new IllegalArgumentException("${function} takes named arguments: ${known.join(', ')}")
    }
    def unknown = arguments.keySet().findAll { name -> !known.contains(name) }
    if (unknown) {
        throw new IllegalArgumentException("${function} got unknown named argument(s): ${unknown.join(', ')}. Expected: ${known.join(', ')}")
    }
    def missing = required.findAll { name -> !arguments.containsKey(name) }
    if (missing) {
        throw new IllegalArgumentException("${function} is missing named argument(s): ${missing.join(', ')}")
    }
}

def getReferenceAttribute(arguments = null) {
    requireRelease('getReferenceAttribute', 7)
    namedArguments('getReferenceAttribute', arguments, ['param', 'datasheet', 'genomeMap', 'genomeKey'], ['param'])
    def param = arguments.param
    def datasheet = arguments.datasheet
    def value = datasheet instanceof Map ? datasheet[param] : null
    if (value) {
        return value
    }
    def genomes = arguments.genomeMap
    def genome = genomes instanceof Map ? genomes[arguments.genomeKey] : null
    return genome instanceof Map ? genome[param] : null
}

def getReferencePath(arguments = null) {
    requireRelease('getReferencePath', 7)
    namedArguments('getReferencePath', arguments, ['references', 'genome', 'igenomesIgnore', 'datasheetPath'], [])
    def genome = arguments.genome
    def unset = genome == null || genome.toString() == '' || genome.toString() == 'null'
    def references = arguments.references
    if (references && !unset) {
        throw new IllegalArgumentException('Use either --references or --genome, not both.')
    }
    if (references) {
        return references
    }
    if (!unset && arguments.igenomesIgnore) {
        def base = arguments.datasheetPath?.toString() ?: ''
        if (!base) {
            throw new IllegalArgumentException("references_datasheets_path is empty; cannot build datasheet URL for genome '${genome}'")
        }
        return "${base.replaceAll('/+$', '')}/${genome}.yml".toString()
    }
    return null
}

// The genome mapping of a row of a references datasheet: a map, or the first map of a list.
def referenceRecord(row) {
    if (row instanceof Map) {
        return row
    }
    return row instanceof List && !row.isEmpty() && row[0] instanceof Map ? row[0] : null
}

def getReferenceMetadata(arguments = null) {
    requireRelease('getReferenceMetadata', 7)
    namedArguments('getReferenceMetadata', arguments, ['rows'], ['rows'])
    def rows = arguments.rows
    if (!(rows instanceof List) || rows.isEmpty()) {
        throw new IllegalArgumentException('Reference datasheet did not contain any genome records')
    }
    if (rows.size() > 1) {
        def genomes = rows.collect { row -> referenceRecord(row)?.genome }.findAll { genome -> genome }
        throw new IllegalArgumentException("Reference datasheet has multiple genomes (${genomes.join(', ')}). Use a datasheet with one genome record.")
    }
    def record = referenceRecord(rows[0])
    if (record == null) {
        throw new IllegalArgumentException("Reference datasheet record is not a genome mapping: ${rows[0]}")
    }
    return record.findAll { key, value -> value != null && value != '' }
}

// --- Startup checks ---

def checkConfigProvided() {
    def message = "[${workflow.projectName}] You are attempting to run the pipeline without any custom configuration!\n\n" +
        "This will be dependent on your local compute environment but can be achieved via one or more of the following:\n" +
        "   (1) Using an existing pipeline profile e.g. `-profile docker` or `-profile singularity`\n" +
        "   (2) Using an existing nf-core/configs for your Institution e.g. `-profile crick` or `-profile uppmax`\n" +
        "   (3) Using your own local custom config e.g. `-c /path/to/your/custom.config`\n\n" +
        "Please refer to the quick start section and usage docs for the pipeline.\n "
    // Before 0.6.0 the plugin looked for the profile and config files in the config, which
    // has neither, so it always warned.
    def standard = !workflow.profile || workflow.profile == 'standard'
    if (minorRelease() < 6 || standard && (workflow.configFiles ?: []).size() <= 1) {
        coreWarn(message)
        return false
    }
    return true
}

def checkProfileProvided(args, monochromeLogs = true) {
    def release = minorRelease()
    def arguments = (args ?: []).collect { argument -> argument.toString() }
    def index = arguments.indexOf('-profile')
    def profile = index >= 0 && index + 1 < arguments.size() ? arguments[index + 1] : null
    if (profile == null && release >= 6) {
        profile = workflow.profile?.toString()
    }
    def colours = logColours(monochromeLogs)
    def hint = "${colours.yellow}HINT${colours.reset}: A common mistake is to provide multiple values separated by spaces e.g. `-profile test, docker`.\n"
    if (profile?.endsWith(',')) {
        def message = "${colours.red}ERROR${colours.reset} ~ The `-profile` option cannot end with a trailing comma, please remove it and re-run the pipeline!\n" + hint
        if (release >= 7) {
            throw new nextflow.exception.AbortOperationException(message.toString())
        }
        throw new IllegalArgumentException(message.toString())
    }
    // Since 0.6.0 the value of `-profile` is not a positional argument.
    def positional = (0..<arguments.size()).find { position ->
        def value = release >= 6 && index >= 0 && position == index + 1
        !value && !arguments[position].startsWith('-')
    }
    if (positional != null) {
        coreWarn("nf-core pipelines do not accept positional arguments. The positional argument `${arguments[positional]}` has been detected.\n" + hint)
    }
}

// --- Workflow version, summary, parameters ---

def getWorkflowVersion(version = null, commitId = null) {
    def release = minorRelease()
    def number = version
    def commit = commitId
    if (!version) {
        number = workflow.manifest.version
        if (!commitId && release >= 6) {
            commit = workflow.commitId
        }
    }
    def text = ''
    if (number) {
        def prefix = number.toString().startsWith('v') ? '' : 'v'
        text += "${prefix}${number}"
    }
    if (commit) {
        def full = commit.toString()
        if (release < 6 && full.length() < 7) {
            throw new StringIndexOutOfBoundsException("Range [0, 7) out of bounds for length ${full.length()}")
        }
        text += "-g${full.take(7)}"
    }
    return text
}

// Whether the run succeeds: before its end, whether nothing failed yet.
def runSucceeded() {
    return workflow.complete == null ? !workflow.errorMessage : workflow.success
}

def completionSummary(monochromeLogs = true) {
    def colours = logColours(monochromeLogs)
    def name = "-${colours.purple}[${pipelineName()}]"
    if (!runSucceeded()) {
        coreInfo("${name}${colours.red} Pipeline completed with errors${colours.reset}-")
    }
    else if (workflow.stats.ignoredCount) {
        coreInfo("${name}${colours.yellow} Pipeline completed successfully, but with errored process(es) ${colours.reset}-")
    }
    else {
        coreInfo("${name}${colours.green} Pipeline completed successfully${colours.reset}-")
    }
}

// Writes the params as JSON to `pipeline_info/params_<time>.json` in the output directory.
def dumpParametersToJSON(outdir, parameters) {
    if (outdir == null) {
        return
    }
    if (parameters == null) {
        coreWarn('Cannot dump parameters - parameters map is null')
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
        coreError("Failed to dump parameters to JSON: ${e.message}")
    }
}

def paramsSummaryMultiqc(summaryParams) {
    def absent = minorRelease() < 6 ? '<span style="color:#999999;">N/A</a>' : '<span style="color:#999999;">N/A</span>'
    def section = ''
    summaryParams.each { group, groupParams ->
        if (groupParams) {
            section += "    <p style=\"font-size:110%\"><b>${group}</b></p>\n"
            section += "    <dl class=\"dl-horizontal\">\n"
            groupParams.keySet().sort().each { param ->
                def value = groupParams[param]
                section += "        <dt>${param}</dt><dd><samp>${value ? value : absent}</samp></dd>\n"
            }
            section += "    </dl>\n"
        }
    }
    def name = workflow.manifest.name
    def id = name?.toString()?.replace('/', '-')
    return "id: '${id}-summary'\n" +
        "description: ' - this information is collected when the pipeline is started.'\n" +
        "section_name: '${name} Workflow Summary'\n" +
        "section_href: 'https://github.com/${name}'\n" +
        "plot_type: 'html'\n" +
        "data: |\n" +
        section
}

// --- Conda ---

def checkCondaChannels() {
    def channels = condaChannels()
    if (!(channels instanceof List)) {
        coreWarn("Could not verify conda channel configuration: ${channels}")
        return true
    }
    if (channels.isEmpty()) {
        coreWarn('No conda channels found - conda configuration may not be set up')
        return true
    }
    def required = ['conda-forge', 'bioconda']
    if (channels.findAll { channel -> required.contains(channel) } == required) {
        return true
    }
    coreWarn('~' * 78 + '\n' +
        '    There is a problem with your Conda configuration!\n' +
        '    You will need to set-up the conda-forge and bioconda channels correctly.\n' +
        '    Please refer to https://bioconda.github.io/\n' +
        '    The observed channel order is\n' +
        "    ${channels}\n" +
        '    but the following channel order is required:\n' +
        "    ${required}\n" +
        '~' * 83)
    return false
}

// --- Software versions ---

// The Nextflow version of the version texts: the given one, the config, `NXF_VER`, then since
// 0.6.0 the running Nextflow.
def nextflowText(given) {
    def running = minorRelease() >= 6 ? workflow.nextflow.version : null
    return (given ?: coreSettings().nextflow ?: System.getenv('NXF_VER') ?: running ?: 'unknown').toString()
}

def workflowBlock(nextflowVersion) {
    return "Workflow:\n    ${pipelineName()}: ${getWorkflowVersion()}\n    Nextflow: ${nextflowText(nextflowVersion)}"
}

def yamlText(map) {
    return new org.yaml.snakeyaml.Yaml().dumpAsMap(map).trim()
}

def collectVersions(input, nextflowVersion = null) {
    if (input instanceof Map) {
        return versionsText([input], nextflowVersion)
    }
    return versionsText(versionEntries(input), nextflowVersion)
}

def versionEntries(input) {
    if (input instanceof List) {
        return input
    }
    if (input instanceof Collection) {
        return input as List
    }
    return [input]
}

// The software versions as YAML, by process and tool, then the workflow and Nextflow
// versions. A channel of versions gives a channel of that text.
def softwareVersionsToYAML(versionsOrOptions, options = [:]) {
    log.debug('softwareVersionsToYAML() is deprecated. Use collectVersions() instead.')
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
    def yaml = sorted ? yamlText(sorted) : ''
    return [yaml, workflowBlock(nextflowVersion)].findAll { text -> text && text != '{}' }.join('\n').trim()
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
                def name = afterColon(entry[0] ?: '')
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
        coreWarn("Could not process version entry ${entry}: ${e.message}")
    }
}

def addYamlVersions(text, merged) {
    def loaded = new org.yaml.snakeyaml.Yaml().load(text)
    if (loaded instanceof Map) {
        mergeVersions(loaded.collectEntries { key, value -> [afterColon(key), value] }, merged)
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
                mergeTools(afterColon(key), value, merged)
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
        def tool = key == null ? null : afterColon(key)
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

def processMixedVersionSources(topicVersions, versionsFiles) {
    log.debug('processMixedVersionSources() is deprecated. Use collectVersions() instead.')
    def parts = [processVersionsFromTopic(topicVersions), processVersionsFromFile(versionsFiles), workflowBlock(null)]
    return parts.findAll { part -> part && part != '{}' }.unique().join('\n')
}

def processVersionsFromTopic(topicData) {
    log.debug('processVersionsFromTopic() is deprecated. Use collectVersions() instead.')
    return yamlText(flatVersions(topicData))
}

// Tool to version from `[process, tool, version]` tuples, the last version winning.
def flatVersions(tuples) {
    def versions = [:]
    (tuples ?: []).each { tuple ->
        if (tuple instanceof List && tuple.size() >= 3) {
            def version = tuple[2]
            versions[tuple[1].toString()] = version instanceof CharSequence ? version.toString() : version
        }
    }
    return versions
}

def processVersionsFromFile(paths) {
    log.debug('processVersionsFromFile() is deprecated. Use collectVersions() instead.')
    def merged = [:]
    (paths ?: []).each { path ->
        def source = path instanceof Path ? path : new File(path.toString())
        if (!source.exists() || !source.isFile()) {
            return
        }
        try {
            def loaded = new org.yaml.snakeyaml.Yaml().load(source.text)
            if (loaded instanceof Map) {
                loaded.each { key, value -> merged[afterColon(key)] = value }
            }
        }
        catch (Exception e) {
            coreWarn("Could not process versions file ${path}: ${e.message}")
        }
    }
    return yamlText(merged)
}

def convertLegacyYamlToEvalSyntax(yaml, processName = 'LEGACY') {
    try {
        def loaded = new org.yaml.snakeyaml.Yaml().load(yaml)
        if (!(loaded instanceof Map)) {
            return []
        }
        return loaded.collect { key, value -> [processName, afterColon(key), value.toString()] }
    }
    catch (Exception e) {
        coreWarn("Could not convert legacy YAML to eval syntax: ${e.message}")
        return []
    }
}

def generateYamlFromEvalSyntax(evalData, includeWorkflow = true) {
    def yaml = yamlText(flatVersions(evalData))
    return includeWorkflow ? "${yaml}\n${workflowBlock(null)}".toString() : yaml
}

def workflowVersionToChannel() {
    return [
        ['Workflow', workflow.manifest.name ?: 'Workflow', getWorkflowVersion()],
        ['Workflow', 'Nextflow', nextflowText(null)],
    ]
}

