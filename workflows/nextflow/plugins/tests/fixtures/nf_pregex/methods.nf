include { Either; AnyOf; Literal; Optional; OneOrMore; Exactly; Range; AtLeast; Sequence; Digit; WordChar; Whitespace; AnyChar; Group; CharClass; CharRange; MultiRange; Chromosome; FastqExtension; BedExtension; ReadPair; ZeroOrMore; NamedGroup } from 'plugin/nf-pregex'

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
    def p = Sequence([Literal('sample'), Group(OneOrMore(Digit())), Literal('_'), Group('read', Either(['R1', 'R2']))])
    show('regex') { p.toRegex() }
    show('test sub') { p.test('x_sample12_R1.fq') }
    show('test no') { p.test('sample_R1') }
    show('test null') { p.test(null) }
    show('matches whole') { p.matches('sample12_R2') }
    show('matches partial') { p.matches('x_sample12_R2') }
    show('matches null') { p.matches(null) }
    show('extract') { p.extract('x_sample12_R2.fq') }
    show('extract keys') { p.extract('x_sample12_R2.fq').keySet() }
    show('extract none') { p.extract('nothing') }
    show('extract null') { p.extract(null) }
    show('extract optional group missing') { Sequence([Literal('a'), Optional(Group(Digit()))]).extract('a') }
    show('extract named order') { Sequence([NamedGroup(Digit(), 'b'), Group(Digit()), Group('a', Digit())]).extract('123') }
    show('chromosome extract') { Chromosome().extract('chr22') }
    show('chromosome test') { [Chromosome().test('chr22'), Chromosome().matches('22'), Chromosome().matches('chr23'), Chromosome().matches('chrMT')] }
    show('fastq') { [FastqExtension().test('x.FQ.GZ'), FastqExtension().matches('.fastq.gz'), FastqExtension().test('x.fa')] }
    show('bed') { [BedExtension().matches('.BED.gz'), BedExtension().matches('.bed')] }
    show('bad regex test') { CharClass('').test('a') }
    def report = p.testAll(['sample1_R1': true, 'sampleX_R1': false, 'sample2_R3': true])
    show('report class') { report.getClass().name }
    show('report counts') { [report.passed, report.failed, report.total, report.allPassed] }
    show('results') { report.results.collect { r -> [r.input, r.expected, r.actual, r.passed] } }
    println '--- report() ---'
    print report.report()
    println '--- report() all pass ---'
    print Digit().testAll(['1': true, 'a': false]).report()
    println '--- explain() p ---'
    println p.explain()
    println '--- explain() complex ---'
    println AnyOf([Exactly(CharClass('AC'), 2), Range(Literal('x'), 1, 2), AtLeast(Whitespace(), 1), ZeroOrMore(WordChar()), Optional(AnyChar()), CharRange('a', 'c'), Chromosome(), BedExtension()]).explain()
    println '--- explain() leaf literal ---'
    println Literal('abc').explain()
    println '--- explain() digit ---'
    println Digit().explain()
    println '--- explain() wordchar ---'
    println WordChar().explain()
    println '--- explain() either ---'
    println ReadPair().explain()
    println '--- visualize() p ---'
    println p.visualize()
    println '--- visualize() complex ---'
    println AnyOf([Exactly(CharClass('AC'), 2), Either(['a', 'b']), MultiRange("'a'-'c'")]).visualize()
    println '--- end ---'
}
