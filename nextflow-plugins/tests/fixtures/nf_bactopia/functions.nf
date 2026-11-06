include { bactopiaInputs; validateParameters; gather; gatherCsvtk; gatherFields; formatSamples; filterWithData; collectNextflowLogs; combineWith; getCapturedLogs } from 'plugin/nf-bactopia'

def show(String label, Closure c) {
    try {
        def v = c.call()
        println "${label} => ${v} (${v == null ? 'null' : v.getClass().name})"
    }
    catch (Throwable e) {
        def r = e.cause ?: e
        println "${label} !! ${e.getClass().name} / ${r.getClass().name}: ${r.message}"
    }
}

workflow {
    def recs = [[meta: [id: 'a'], tsv: 'a.tsv', gff: null], [meta: [id: 'b'], tsv: 'b.tsv', gff: 'b.gff'], [meta: [id: 'c'], tsv: 'a.tsv', gff: []]]
    show('validateParameters') { validateParameters(false) }
    show('bactopiaInputs paired-end') { bactopiaInputs('paired-end') }
    show('bactopiaInputs assembly') { bactopiaInputs('assembly') }
    show('bactopiaInputs bogus') { bactopiaInputs('bogus') }
    show('gather list') { gather(recs, 'tsv', [name: 'sccmec']) }
    show('gather list empty') { gather([[tsv: null]], 'tsv', [name: 'x']) }
    show('gather no name') { gather(recs, 'tsv', [:]) }
    show('gatherCsvtk list') { gatherCsvtk(recs, 'tsv', [name: 'ariba', args: '-C "$"']) }
    show('gatherFields list') { gatherFields(recs, [gff: 'gff3', tsv: 'table'], [name: 'prokka']) }
    show('filterWithData list') { filterWithData(recs, ['gff']) }
    show('collectNextflowLogs list') { collectNextflowLogs([[meta: [id: 'a'], nf_logs: ['l1', 'l2']], [meta: [id: 'b'], nf_logs: []]]) }
    show('combineWith list') { combineWith([[meta: [name: 'g'], csv: ['x']]], ['ref1', 'ref2'], 'reference') }
    show('formatSamples 1') { formatSamples([[[id: 's'], ['r1'], ['e1'], ['e2']]], 1) }
    show('formatSamples 2') { formatSamples([[[id: 's'], ['r1'], ['e1'], ['e2']]], 2) }
    show('formatSamples 4') { formatSamples([[[id: 's'], ['r1'], ['e1'], ['e2']]], 4) }
    gather(channel.fromList(recs), 'tsv', [name: 'chan']).subscribe { println "gather channel => ${it} (${it.getClass().name})" }
    filterWithData(channel.fromList(recs), ['gff']).toList().subscribe { println "filterWithData channel => ${it}" }
    combineWith(channel.of([meta: [name: 'g'], csv: ['x']]), channel.of('r1', 'r2'), 'ref').toList().subscribe { println "combineWith channel => ${it}" }
    formatSamples(channel.of([[id: 's'], ['r1'], ['e1'], ['e2']]), 1).subscribe { println "formatSamples channel => ${it}" }
}
