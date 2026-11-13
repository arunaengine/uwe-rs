include { fromJson; toJson; fromYaml; toYaml; mergeCsv; mergeText; template } from 'plugin/nf-boost'

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

def json(v) {
    return groovy.json.JsonOutput.toJson(v)
}

workflow {
    def out = file("${workflow.launchDir}/out")
    out.mkdirs()
    show('fromJson path') { fromJson(file('in.json')) }
    show('fromJson text') { fromJson('[1, 2.0, 1e3, 12345678901234, "s", null]') }
    show('fromJson types') { fromJson('[1, 2.0, 1e3, 12345678901234, 123456789012345678901234]').collect { it.getClass().simpleName } }
    show('fromJson bad') { fromJson('{bad') }
    show('toJson') { json(toJson([a: 1, b: [1, 2.5, 'x'], c: [d: null, e: true], f: 'q"\n/é'])) }
    show('toJson pretty') { json(toJson([a: 1, b: [1, 2], c: [:], d: []], true)) }
    show('toJson scalar') { toJson('x') }
    show('fromYaml path') { fromYaml(file('in.yaml')) }
    show('fromYaml types') { fromYaml(file('in.yaml')).collectEntries { k, v -> [k, v == null ? 'null' : v.getClass().simpleName] } }
    show('fromYaml text multi') { fromYaml('a: 1\n---\nb: 2\n') }
    show('toYaml map') { json(toYaml([a: 1, b: [1, 'x'], c: [d: null, e: true], s: 'multi\nline', q: 'yes', n: 1.5])) }
    show('toYaml list') { json(toYaml(['a', 'b'])) }
    show('toYaml nested list') { json(toYaml([[a: 1], [b: [c: 2]]])) }
    show('toYaml scalar') { json(toYaml('x')) }
    mergeCsv([[a: 1, b: 'x'], [a: 2, b: 'y,z'], [a: 3, b: 'q"r']], out.resolve('maps-noheader.csv'))
    mergeCsv([[a: 1, b: 'x'], [b: 'y', a: 2]], out.resolve('maps-header.csv'), header: true)
    mergeCsv([[1, 'x'], [2, 'y']], out.resolve('tuples-header-list.csv'), header: ['c1', 'c2'], sep: '\t')
    mergeCsv([[1, null, 2.5, true]], out.resolve('types.csv'))
    show('mergeCsv empty') { mergeCsv([], out.resolve('empty.csv')) }
    show('mergeCsv returns') { mergeCsv([[1]], out.resolve('ret.csv')) }
    mergeText([file('t1.csv'), file('t2.csv'), file('t3.txt')], out.resolve('text-plain.txt'))
    mergeText([file('t1.csv'), file('t2.csv')], out.resolve('text-keepheader.txt'), keepHeader: true)
    mergeText(['a', 'b\n', 'c'], out.resolve('text-newline.txt'), newLine: true)
    mergeText([file('t1.csv'), file('t2.csv')], out.resolve('text-skip.txt'), skip: 1)
    mergeText(['x'], out.resolve('text-append.txt'))
    mergeText(['y'], out.resolve('text-append.txt'))
    show('mergeText keepHeader skip 0') { mergeText(['a'], out.resolve('bad.txt'), keepHeader: true, skip: 0) }
    show('mergeText empty') { mergeText([], out.resolve('bad2.txt')) }
    show('mergeText bad item') { mergeText([1], out.resolve('bad3.txt')) }
    show('template path') { json(template(file('tpl.txt'), [name: 'W', items: [1, 2]])) }
    show('template text') { json(template('${a}-${b}', [a: 1, b: 'two'])) }
    show('template missing var') { template('${zz}', [a: 1]) }
}
