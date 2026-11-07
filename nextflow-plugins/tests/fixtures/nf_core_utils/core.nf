include { getGenomeAttribute; getWorkflowVersion; checkConfigProvided; checkProfileProvided; completionSummary; dumpParametersToJSON; checkCondaChannels; collectVersions; softwareVersionsToYAML; processMixedVersionSources; convertLegacyYamlToEvalSyntax; generateYamlFromEvalSyntax; processVersionsFromTopic; processVersionsFromFile; workflowVersionToChannel; generateModuleToolCitation; toolCitationText; toolBibliographyText; methodsDescriptionText; collectCitationsFromFiles; generateComprehensiveReport; generateVersionReport; generateCitationReport; processCitationsFromTopic; processCitationsFromFile; processMixedCitationSources; convertMetaYamlToTopicFormat; getCitation; autoToolCitationText; autoToolBibliographyText; paramsSummaryMultiqc; completionEmail; imNotification } from 'plugin/nf-core-utils'

def root(e) {
    return e.cause != null ? root(e.cause) : e
}

def probe(String name, Closure body) {
    println "===== ${name}"
    try {
        def out = body.call()
        println(out instanceof Map || out instanceof List ? groovy.json.JsonOutput.toJson(out) : out)
    } catch (e) {
        def cause = root(e)
        println "!! ${cause.getClass().name}: ${cause.message}"
    }
}

workflow {
    main:
    def fastqc = "${projectDir}/modules/nf-core/fastqc/meta.yml"
    def samtools = "${projectDir}/modules/nf-core/samtools/sort/meta.yml"
    def topic = [['NFCORE_PROBE:FASTQC', 'fastqc', '0.12.1'], ['NFCORE_PROBE:SORT', 'samtools', '1.21'], ['NFCORE_PROBE:SORT', 'samtools', 1.22]]
    probe('getGenomeAttribute fasta') { getGenomeAttribute('fasta') }
    probe('getGenomeAttribute missing') { getGenomeAttribute('bwa') }
    probe('getWorkflowVersion') { getWorkflowVersion() }
    probe('getWorkflowVersion 2.0') { getWorkflowVersion('2.0') }
    probe('getWorkflowVersion v2.0 commit') { getWorkflowVersion('v2.0', 'abcdef1234567890') }
    probe('getWorkflowVersion short commit') { getWorkflowVersion('2.0', 'abc') }
    probe('checkConfigProvided') { checkConfigProvided() }
    probe('checkProfileProvided positional') { checkProfileProvided(['input.csv', '-profile', 'test,'], true); 'returned' }
    probe('completionSummary') { completionSummary(true); 'returned' }
    probe('dumpParametersToJSON') { dumpParametersToJSON(params.outdir, [a: 1, b: null, c: file('x.txt'), d: 2.GB, e: 3.h, f: [g: 'h']]); file(params.outdir).resolve('pipeline_info').list().findAll { n -> n.startsWith('params_') }.size() }
    probe('checkCondaChannels') { checkCondaChannels() }
    probe('collectVersions') { collectVersions(topic + ["${projectDir}/versions_fastqc.yml", "${projectDir}/versions_flat.yml", [TOOLX: [toolx: '3.0']], 'inline: 9.9']) }
    probe('collectVersions nf') { collectVersions([], '25.10.0') }
    probe('softwareVersionsToYAML list') { softwareVersionsToYAML(topic) }
    probe('softwareVersionsToYAML named') { softwareVersionsToYAML(softwareVersions: topic, nextflowVersion: '24.04.1') }
    probe('processMixedVersionSources') { processMixedVersionSources(topic, ["${projectDir}/versions_fastqc.yml", "${projectDir}/versions_flat.yml", '/missing.yml']) }
    probe('convertLegacyYamlToEvalSyntax') { convertLegacyYamlToEvalSyntax('"P:Q:fastqc": 0.12.1\nsamtools: 1.21\n') }
    probe('convertLegacyYamlToEvalSyntax process') { convertLegacyYamlToEvalSyntax('fastqc: 0.12.1', 'FASTQC') }
    probe('generateYamlFromEvalSyntax') { generateYamlFromEvalSyntax(topic) }
    probe('generateYamlFromEvalSyntax noworkflow') { generateYamlFromEvalSyntax(topic, false) }
    probe('processVersionsFromTopic') { processVersionsFromTopic(topic) }
    probe('processVersionsFromFile') { processVersionsFromFile(["${projectDir}/versions_fastqc.yml", "${projectDir}/versions_flat.yml"]) }
    probe('workflowVersionToChannel') { workflowVersionToChannel() }
    probe('generateModuleToolCitation fastqc') { generateModuleToolCitation(fastqc) }
    probe('generateModuleToolCitation samtools') { generateModuleToolCitation(samtools) }
    probe('generateModuleToolCitation missing') { generateModuleToolCitation('/missing/meta.yml') }
    def cites = collectCitationsFromFiles([fastqc, samtools, '/missing/meta.yml'])
    probe('collectCitationsFromFiles') { cites }
    probe('toolCitationText') { toolCitationText(cites) }
    probe('toolCitationText empty') { toolCitationText([:]) }
    probe('toolBibliographyText') { toolBibliographyText(cites) }
    probe('toolBibliographyText empty') { toolBibliographyText([:]) }
    probe('methodsDescriptionText') { methodsDescriptionText("${projectDir}/assets/methods_description_template.yml", cites) }
    probe('generateComprehensiveReport') { generateComprehensiveReport(topic, ["${projectDir}/versions_flat.yml"], [fastqc], "${projectDir}/assets/methods_description_template.yml") }
    probe('generateVersionReport') { generateVersionReport(topic) }
    probe('generateCitationReport') { generateCitationReport([samtools]) }
    probe('processCitationsFromTopic') { processCitationsFromTopic([['FASTQC', 'fastqc', [doi: '10.1/x', author: 'A B', year: 2020]], ['X', 'plain', 'Plain citation text']]) }
    probe('processCitationsFromFile') { processCitationsFromFile([fastqc, '/missing/meta.yml']) }
    probe('processMixedCitationSources') { processMixedCitationSources([['X', 'plain', 'Plain citation text']], [fastqc]) }
    probe('convertMetaYamlToTopicFormat') { convertMetaYamlToTopicFormat(samtools) }
    probe('convertMetaYamlToTopicFormat named') { convertMetaYamlToTopicFormat(fastqc, 'MYMOD') }
    probe('getCitation') { getCitation(samtools) }
    probe('autoToolCitationText') { autoToolCitationText([getCitation(fastqc), getCitation(samtools)]) }
    probe('autoToolCitationText empty') { autoToolCitationText([]) }
    probe('autoToolBibliographyText') { autoToolBibliographyText(getCitation(samtools)) }
    probe('paramsSummaryMultiqc') { paramsSummaryMultiqc(['Input/output options': [outdir: 'results', input: null, zeta: 'z'], 'Empty': [:], 'Core Nextflow options': [runName: 'x']]) }

    onComplete:
    def summary = ['Input/output options': [outdir: 'results'], 'Core Nextflow options': [runName: workflow.runName]]
    probe('completionEmail') { completionEmail(summary, params.email, null, false, params.outdir, true, [file("${projectDir}/multiqc_report.html")]); 'returned' }
    probe('completionEmail plaintext') { completionEmail(summary, params.email, null, true, params.outdir, true, []); 'returned' }
    probe('imNotification slack') { imNotification(summary, params.hook_url + '/hooks.slack.com/services/x'); 'returned' }
    probe('imNotification teams') { imNotification(summary, params.hook_url + '/teams --hook_url y'); 'returned' }
    probe('imNotification failing') { imNotification(summary, params.hook_url + '/fail'); 'returned' }
    probe('imNotification empty') { imNotification(summary, ''); 'returned' }
}
