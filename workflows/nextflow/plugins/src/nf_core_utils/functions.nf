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

// --- Citations ---

// The `[tool, info]` pairs of the `tools` list of a module's meta.yml.
def metaTools(metaFile) {
    def meta = new org.yaml.snakeyaml.Yaml().load(metaFile.text)
    def tools = meta instanceof Map && meta.tools instanceof List ? meta.tools : []
    def pairs = []
    tools.each { entry ->
        if (entry instanceof Map) {
            entry.each { tool, info -> pairs << [tool.toString(), info] }
        }
    }
    return pairs
}

def shortCitation(tool, info) {
    if (info.doi) {
        return "${tool} (DOI: ${info.doi})".toString()
    }
    return info.description ? "${tool} (${info.description})".toString() : tool.toString()
}

def bibliographyEntry(tool, info) {
    if (minorRelease() < 7) {
        def parts = [info.author, info.year, info.title ?: tool, info.journal, info.doi ? "doi: ${info.doi}" : null]
        def text = parts.findAll { part -> part }.join('. ')
        if (info.homepage) {
            text += ". <a href='${info.homepage}'>${info.homepage}</a>"
        }
        return "<li>${text}</li>".toString()
    }
    def publication = info.publication instanceof Map ? info.publication :
        [author: info.author, year: info.year, title: info.title, source: info.journal].findAll { key, value -> value }
    def parts = []
    if (publication) {
        [publication.author ? publication.author.toString().replaceAll(/[.\s]+$/, '') : null,
         publication.year ? "(${publication.year})" : null,
         publication.title,
         publication.source].each { part -> if (part) { parts << part } }
    }
    else {
        parts << tool
    }
    def link = referenceLink(info)
    if (link) {
        parts << link
    }
    return "<li>${parts.join('. ')}.</li>".toString()
}

def referenceLink(info) {
    if (info.doi) {
        return "doi: <a href='https://doi.org/${info.doi}'>${info.doi}</a>"
    }
    return info.homepage ? "<a href='${info.homepage}'>${info.homepage}</a>" : null
}

def toolCitation(tool, info) {
    return [citation: shortCitation(tool, info), bibliography: bibliographyEntry(tool, info)]
}

def generateModuleToolCitation(metaFilePath) {
    def metaFile = file(metaFilePath.toString())
    if (!metaFile.exists()) {
        throw new IllegalArgumentException("meta.yml file not found at: ${metaFile.toAbsolutePath()}")
    }
    def citations = [:]
    metaTools(metaFile).each { pair ->
        def tool = pair[0]
        def info = pair[1]
        citations[tool] = info instanceof Map ? toolCitation(tool, info) : [citation: tool, bibliography: null]
    }
    return citations
}

def citationsFromFiles(paths, warning) {
    def citations = [:]
    (paths ?: []).each { path ->
        try {
            citations.putAll(generateModuleToolCitation(path))
        }
        catch (Exception e) {
            coreWarn("${warning} ${path}: ${e.message}")
        }
    }
    return citations
}

def collectCitationsFromFiles(paths) {
    return citationsFromFiles(paths, 'Could not process meta.yml at')
}

def processCitationsFromFile(paths) {
    return citationsFromFiles(paths, 'Could not process citation file')
}

def toolCitationText(citations) {
    if (!citations) {
        return 'No tools used in the workflow.'
    }
    return "Tools used in the workflow included: ${citations.values().collect { entry -> entry.citation }.join(', ')}.".toString()
}

def toolBibliographyText(citations) {
    if (!citations) {
        return 'No bibliography entries found.'
    }
    return citations.values().collect { entry -> entry.bibliography }.findAll { entry -> entry != null }.join(' ')
}

def methodsDescriptionText(mqcMethodsYamlPath, citations = [:], meta = [:]) {
    def binding = [:]
    binding.putAll(meta ?: [:])
    if (!binding.containsKey('workflow')) {
        binding.workflow = workflow
    }
    if (!binding.containsKey('manifest_map')) {
        binding.manifest_map = workflow.manifest
    }
    def dois = (workflow.manifest.doi ?: '').toString().tokenize(',')
        .collect { doi -> doi.replace('https://doi.org/', '').replace(' ', '') }
        .findAll { doi -> doi }
    binding.doi_text = dois.collect { doi -> "(doi: <a href='https://doi.org/${doi}'>${doi}</a>)" }.join(', ')
    binding.nodoi_text = dois ? '' : '<li>If available, make sure to update the text to include the Zenodo DOI of version of the pipeline used. </li>'
    if (!binding.containsKey('tool_citations')) {
        binding.tool_citations = toolCitationText(citations ?: [:])
    }
    if (!binding.containsKey('tool_bibliography')) {
        binding.tool_bibliography = toolBibliographyText(citations ?: [:])
    }
    def template = file(mqcMethodsYamlPath.toString())
    return new groovy.text.SimpleTemplateEngine().createTemplate(template.text).make(binding).toString()
}

def citationReport(metaFilePaths, mqcMethodsYamlPath) {
    def citations = collectCitationsFromFiles(metaFilePaths)
    def methods = ''
    if (mqcMethodsYamlPath && file(mqcMethodsYamlPath.toString()).exists()) {
        try {
            methods = methodsDescriptionText(mqcMethodsYamlPath, citations)
        }
        catch (Exception e) {
            coreWarn("Could not generate methods description: ${e.message}")
        }
    }
    return [
        tool_citations: toolCitationText(citations),
        tool_bibliography: toolBibliographyText(citations),
        methods_description: methods,
        citations_map: citations,
    ]
}

def generateComprehensiveReport(topicVersions, legacyVersions = [], metaFilePaths = [], mqcMethodsYamlPath = null) {
    return generateVersionReport(topicVersions, legacyVersions) + citationReport(metaFilePaths, mqcMethodsYamlPath)
}

def generateVersionReport(topicVersions, legacyVersions = []) {
    return [versions_yaml: versionsText((topicVersions ?: []) + (legacyVersions ?: []), null)]
}

def generateCitationReport(metaFilePaths, mqcMethodsYamlPath = null) {
    return citationReport(metaFilePaths, mqcMethodsYamlPath)
}

def processCitationsFromTopic(topicData) {
    def citations = [:]
    (topicData ?: []).each { tuple ->
        if (tuple instanceof List && tuple.size() >= 3) {
            def tool = tuple[1].toString()
            def data = tuple[2]
            citations[tool] = data instanceof Map ? toolCitation(tool, data) :
                [citation: data.toString(), bibliography: "<li>${data}</li>".toString()]
        }
    }
    return citations
}

def processMixedCitationSources(topicCitations, citationFiles) {
    def citations = processCitationsFromTopic(topicCitations)
    citations.putAll(processCitationsFromFile(citationFiles))
    return citations
}

def convertMetaYamlToTopicFormat(metaFilePath, moduleName = null) {
    def metaFile = file(metaFilePath.toString())
    if (!metaFile.exists()) {
        return []
    }
    try {
        def module = moduleName ?: metaFile.parent.name
        return metaTools(metaFile).collect { pair -> [module, pair[0], pair[1]] }
    }
    catch (Exception e) {
        coreWarn("Could not convert meta.yml to topic format: ${e.message}")
        return []
    }
}

def getCitation(metaYmlPath) {
    def metaFile = file(metaYmlPath.toString())
    if (!metaFile.exists()) {
        coreWarn("meta.yml not found at ${metaYmlPath}")
        return []
    }
    try {
        def parts = metaYmlPath.toString().tokenize('/')
        def last = parts.lastIndexOf('meta.yml')
        def module = last > 0 ? parts[last - 1] : file('.').toAbsolutePath().name
        return metaTools(metaFile).findAll { pair -> pair[1] instanceof Map }
            .collect { pair -> [module.toUpperCase(), pair[0], pair[1]] }
    }
    catch (Exception e) {
        coreWarn("Failed to extract citation from ${metaYmlPath}: ${e.message}")
        return []
    }
}

// The `[module, tool, info]` tuples of a list of tuples or of lists of them.
def citationTuples(topicCitations) {
    def tuples = []
    (topicCitations ?: []).each { item ->
        if (item instanceof List && item.size() == 3) {
            tuples << item
        }
        else if (item instanceof List) {
            item.each { inner -> if (inner instanceof List && inner.size() == 3) { tuples << inner } }
        }
    }
    return tuples
}

def autoToolCitationText(topicCitations = []) {
    return toolCitationText(processCitationsFromTopic(citationTuples(topicCitations)))
}

def autoToolBibliographyText(topicCitations = []) {
    return toolBibliographyText(processCitationsFromTopic(citationTuples(topicCitations)))
}

def toolsFromVersionsTopic(topicVersions) {
    requireRelease('toolsFromVersionsTopic', 6)
    if (!topicVersions) {
        return []
    }
    def tuples = topicVersions.findAll { item -> item instanceof List && item.size() >= 2 }
    if (!tuples) {
        coreWarn("toolsFromVersionsTopic received ${topicVersions.size()} item(s) but no [process, tool, version] tuples \u2014 collect the versions topic with .collect(flat: false) to preserve tuples")
        return []
    }
    return tuples.collect { tuple -> tuple[1]?.toString()?.trim() }.findAll { tool -> tool }.unique().sort()
}

// The short form of an author list: the surname, with `et al.` for several authors.
def shortAuthor(author) {
    def authors = author.toString().trim().split(/\s*&\s*|\s+and\s+/) as List
    def first = authors[0].split(',').collect { part -> part.trim() }
    def harvard = first.size() > 1 && first[1] ==~ /([A-Z]\.?\s*)+/
    def surname = harvard ? first[0] : first[0].tokenize(' ')[0]
    def several = authors.size() > 1 || author.toString().toLowerCase().contains('et al') || first.size() > (harvard ? 2 : 1)
    return several ? "${surname} <em>et al.</em>".toString() : surname
}

def citationsOnTheFly(topicVersions, metaFilePaths, extraTools = []) {
    requireRelease('citationsOnTheFly', 6)
    def used = (toolsFromVersionsTopic(topicVersions) + (extraTools ?: []).collect { tool -> tool.toString() }).unique().sort()
    def exact = [:]
    (topicVersions ?: []).each { tuple ->
        if (tuple instanceof List && tuple.size() >= 3) {
            def tool = tuple[1]?.toString()?.trim()
            if (tool && !exact.containsKey(tool)) {
                exact[tool] = tuple[2]
            }
        }
    }
    def versions = [:]
    exact.each { tool, version -> versions[tool.toLowerCase()] = version }
    def infos = [:]
    (metaFilePaths ?: []).each { path ->
        getCitation(path).each { tuple ->
            if (!infos.containsKey(tuple[1])) {
                infos[tuple[1]] = tuple[2]
            }
        }
    }
    def citations = [:]
    used.each { tool ->
        def name = infos.containsKey(tool) ? tool : infos.keySet().find { known -> known.equalsIgnoreCase(tool) }
        if (name == null) {
            coreWarn("No citation metadata found for tool '${tool}' \u2014 add it to the module's meta.yml 'tools:' section")
            return
        }
        if (citations.containsKey(name)) {
            return
        }
        def info = infos[name]
        def parts = []
        def version = versions[name.toLowerCase()]
        if (version != null) {
            parts << version.toString()
        }
        def reference = shortReference(info)
        if (reference) {
            parts << reference
        }
        def citation = parts ? "${name} (${parts.join(', ')})".toString() : name
        citations[name] = [citation: citation, bibliography: bibliographyEntry(name, info)]
    }
    return citations
}

// The linked short reference of a tool for `citationsOnTheFly`, or null.
def shortReference(info) {
    def publication = info.publication instanceof Map ? info.publication : [:]
    def author = publication.author ?: info.author
    def year = publication.year ?: info.year
    def text = null
    if (author) {
        text = shortAuthor(author) + (year ? " (${year})" : '')
    }
    else if (info.doi) {
        text = "doi: ${info.doi}"
    }
    else {
        text = info.homepage ?: info.description
    }
    if (!text) {
        return null
    }
    if (info.doi) {
        return "<a href='https://doi.org/${info.doi}'>${text}</a>".toString()
    }
    return info.homepage ? "<a href='${info.homepage}'>${text}</a>".toString() : text.toString()
}

// --- Completion e-mail and chat notification ---

// A time as Nextflow prints its start and end times, like `2026-11-06T09:29:44.776+01:00`.
def isoDate(date) {
    if (date == null) {
        return null
    }
    def zone = date.format('Z')
    return date.format("yyyy-MM-dd'T'HH:mm:ss.SSS") + zone.substring(0, 3) + ':' + zone.substring(3)
}

// The groups of the summary params in name order, merged, then facts of the run under the
// names that the e-mail or the chat message gives them.
def runSummary(summaryParams, names) {
    def summary = [:]
    (summaryParams ?: [:]).keySet().sort().each { group -> summary.putAll(summaryParams[group] ?: [:]) }
    def facts = [
        isoDate(workflow.start), isoDate(workflow.complete), workflow.scriptFile, workflow.scriptId,
        workflow.repository, workflow.commitId, workflow.revision,
        workflow.nextflow.version, workflow.nextflow.build, workflow.nextflow.timestamp,
    ]
    (0..<names.size()).each { index ->
        if (facts[index] || index < 4 || index > 6) {
            summary[names[index]] = facts[index]
        }
    }
    return summary
}

def runBinding(summary, commandLine) {
    return [
        version: getWorkflowVersion(), runName: workflow.runName, success: workflow.success,
        dateComplete: isoDate(workflow.complete), duration: workflow.duration,
        exitStatus: workflow.exitStatus, errorMessage: workflow.errorMessage ?: 'None',
        errorReport: workflow.errorReport ?: 'None', commandLine: commandLine,
        projectDir: workflow.projectDir, summary: summary,
    ]
}

def renderTemplate(template, binding) {
    return new groovy.text.SimpleTemplateEngine().createTemplate(template.text).make(binding).toString()
}

def completionEmail(summaryParams, email, emailOnFail, plaintextEmail, outdir, monochromeLogs = true, multiqcReports = []) {
    def name = pipelineName()
    def runName = workflow.runName ?: 'unknown'
    def subject = workflow.success ? "[${name}] Successful: ${runName}" : "[${name}] FAILED: ${runName}"
    def names = [
        'Date Started', 'Date Completed', 'Pipeline script file path', 'Pipeline script hash ID',
        'Pipeline repository Git URL', 'Pipeline repository Git Commit', 'Pipeline Git branch/tag',
        'Nextflow Version', 'Nextflow Build', 'Nextflow Compile Timestamp',
    ]
    def binding = runBinding(runSummary(summaryParams, names), workflow.commandLine)
    def assets = file("${workflow.projectDir}/assets")
    def textTemplate = assets.resolve('email_template.txt')
    def htmlTemplate = assets.resolve('email_template.html')
    def text = 'Default email content'
    def html = '<html><body>Default email content</body></html>'
    try {
        if (textTemplate.exists()) {
            text = renderTemplate(textTemplate, binding)
        }
        else {
            coreWarn("Email template not found: ${textTemplate.toAbsolutePath()}")
        }
        if (htmlTemplate.exists()) {
            html = renderTemplate(htmlTemplate, binding)
        }
        else {
            coreWarn("HTML email template not found: ${htmlTemplate.toAbsolutePath()}")
        }
    }
    catch (Exception e) {
        coreWarn("Failed to render email templates: ${e.message}")
    }
    writeReports(outdir, html, text)
    def address = email ?: (workflow.success ? null : emailOnFail)
    if (!address) {
        return
    }
    def platform = System.getenv('TOWER_WORKFLOW_ID')
    def forced = params.force_completion_email instanceof Boolean && params.force_completion_email
    if (minorRelease() >= 7 && platform && !forced) {
        coreInfo("[${name}] Skipping summary e-mail for a Seqera Platform launch; set params.force_completion_email = true to send it. Platform notifications must be enabled separately.")
        return
    }
    def message = [to: address.toString(), subject: subject.toString(), text: text]
    if (!plaintextEmail) {
        message.html = html
        def logo = assets.resolve("${name.replace('/', '-')}_logo_light.png")
        if (logo.exists()) {
            message.logo = logo
        }
        message.attachment = multiqcAttachment(name, multiqcReports)
    }
    def outcome = deliverMail(message)
    def colours = logColours(monochromeLogs)
    if (outcome.status == 'sent') {
        coreInfo("-${colours.purple}[${name}]${colours.green} Sent summary e-mail to ${address}-")
    }
    else if (outcome.status == 'timeout') {
        coreWarn("[${name}] Timed out sending the summary e-mail to ${address} after 30 seconds; delivery status is unknown")
    }
    else if (outcome.status == 'skipped') {
        log.warn("[${name}] Did not send the summary e-mail to ${address}: ${outcome.message}")
    }
    else {
        coreWarn("[${name}] Could not send the summary e-mail to ${address}: ${outcome.message}")
    }
}

// Writes the rendered e-mail to `pipeline_info/pipeline_report.html` and `.txt`.
def writeReports(outdir, html, text) {
    if (outdir == null) {
        coreWarn('Cannot write email reports - output directory is null')
        return
    }
    try {
        def directory = file(outdir.toString()).resolve('pipeline_info')
        directory.mkdirs()
        directory.resolve('pipeline_report.html').text = html
        directory.resolve('pipeline_report.txt').text = text
    }
    catch (Exception e) {
        coreWarn("Failed to write email report files: ${e.message}")
    }
}

// The MultiQC report to attach, when it is smaller than `params.max_multiqc_email_size`.
def multiqcAttachment(name, multiqcReports) {
    def reports = multiqcReports instanceof List ? multiqcReports : (multiqcReports == null ? [] : [multiqcReports])
    if (reports.isEmpty()) {
        coreWarn("[${name}] No reports found from process 'MULTIQC'")
        return null
    }
    if (reports.size() > 1) {
        coreWarn("[${name}] Found multiple reports from process 'MULTIQC', will use only one")
    }
    if (params.max_multiqc_email_size == null) {
        return null
    }
    def limit
    try {
        limit = MemoryUnit.of(params.max_multiqc_email_size.toString())
    }
    catch (Exception e) {
        coreWarn("Failed to parse max_multiqc_email_size: ${e.message}")
        return null
    }
    def report = reports[0] instanceof Path ? reports[0] : file(reports[0].toString())
    try {
        return report.size() < limit.toBytes() ? report : null
    }
    catch (Exception e) {
        coreWarn("Could not read the MultiQC report ${report.toUriString()}: ${e.message}")
        return null
    }
}

def imNotification(summaryParams, hookUrl) {
    if (!hookUrl?.toString()?.trim()) {
        coreWarn('Cannot send IM notification - hook URL is null or empty')
        return
    }
    def url = hookUrl.toString()
    def names = [
        'start', 'complete', 'scriptfile', 'scriptid', 'repository', 'commitid', 'revision',
        'nxf_version', 'nxf_build', 'nxf_timestamp',
    ]
    def commandLine = (workflow.commandLine ?: '').toString().replaceFirst(/\s+--hook_url\s+\S+/, '')
    def binding = runBinding(runSummary(summaryParams, names), commandLine)
    def template = file("${workflow.projectDir}/assets").resolve(url.contains('hooks.slack.com') ? 'slackreport.json' : 'adaptivecard.json')
    def body
    if (template.exists()) {
        try {
            body = renderTemplate(template, binding)
        }
        catch (Exception e) {
            coreWarn("Failed to render IM notification template (${e.getClass().simpleName})")
            return
        }
    }
    else {
        coreWarn("IM notification template not found: ${template.toAbsolutePath()}")
        def outcome = workflow.success ? 'completed successfully' : 'failed'
        body = groovy.json.JsonOutput.toJson([text: "Pipeline ${workflow.runName} ${outcome}".toString()])
    }
    def answer = postNotification(url, body)
    if (answer.code != null && answer.code >= 200 && answer.code < 300) {
        coreInfo('IM notification sent successfully')
    }
    else if (answer.code != null) {
        coreWarn("IM notification failed with response code ${answer.code}")
    }
    else {
        coreWarn("Failed to send IM notification (${answer.error})")
    }
}
