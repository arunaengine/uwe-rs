include { getReferenceAttribute; getReferencePath; getReferenceMetadata; toolsFromVersionsTopic; citationsOnTheFly; checkProfileProvided; checkConfigProvided; completionEmail } from 'plugin/nf-core-utils'

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
    def topic = [['NFCORE_PROBE:FASTQC', 'fastqc', '0.12.1'], ['NFCORE_PROBE:SORT', 'samtools', '1.21'], ['NFCORE_PROBE:SORT', 'Samtools', '1.22'], ['X', 'multiqc', '1.25']]
    probe('getReferenceAttribute datasheet') { getReferenceAttribute(param: 'fasta', datasheet: [fasta: 'sheet.fa'], genomeMap: params.genomes, genomeKey: 'GRCh38') }
    probe('getReferenceAttribute fallback') { getReferenceAttribute(param: 'gtf', datasheet: [fasta: 'sheet.fa', gtf: ''], genomeMap: params.genomes, genomeKey: 'GRCh38') }
    probe('getReferenceAttribute none') { getReferenceAttribute(param: 'gtf') }
    probe('getReferenceAttribute unknown arg') { getReferenceAttribute(param: 'gtf', foo: 1) }
    probe('getReferenceAttribute missing arg') { getReferenceAttribute(datasheet: [:]) }
    probe('getReferencePath references') { getReferencePath(references: 'refs.yml') }
    probe('getReferencePath both') { getReferencePath(references: 'refs.yml', genome: 'GRCh38') }
    probe('getReferencePath datasheet') { getReferencePath(genome: 'GRCh38', igenomesIgnore: true, datasheetPath: 'https://example.org/sheets//') }
    probe('getReferencePath genome null text') { getReferencePath(references: 'refs.yml', genome: 'null') }
    probe('getReferencePath no ignore') { getReferencePath(genome: 'GRCh38', igenomesIgnore: false, datasheetPath: 'x') }
    probe('getReferencePath empty base') { getReferencePath(genome: 'GRCh38', igenomesIgnore: true, datasheetPath: '') }
    probe('getReferenceMetadata map') { getReferenceMetadata(rows: [[genome: 'GRCh38', fasta: 'a.fa', gtf: '', bed: null]]) }
    probe('getReferenceMetadata tuple') { getReferenceMetadata(rows: [[[genome: 'GRCh38', fasta: 'a.fa'], 'x']]) }
    probe('getReferenceMetadata many') { getReferenceMetadata(rows: [[genome: 'A'], [genome: 'B'], [x: 1]]) }
    probe('getReferenceMetadata empty') { getReferenceMetadata(rows: []) }
    probe('getReferenceMetadata bad row') { getReferenceMetadata(rows: ['text']) }
    probe('toolsFromVersionsTopic') { toolsFromVersionsTopic(topic) }
    probe('toolsFromVersionsTopic flat') { toolsFromVersionsTopic(['a', 'b', 'c']) }
    probe('citationsOnTheFly') { citationsOnTheFly(topic, [fastqc, samtools], ['htslib', 'plaintool']) }
    probe('checkProfileProvided positional') { checkProfileProvided(['input.csv', '-profile', 'docker', '--flag'], true); 'returned' }
    probe('checkProfileProvided session') { checkProfileProvided([], true); 'returned' }
    probe('checkConfigProvided') { checkConfigProvided() }

    onComplete:
    probe('completionEmail platform') { completionEmail([:], params.email, null, false, params.outdir, true, []); 'returned' }
}
