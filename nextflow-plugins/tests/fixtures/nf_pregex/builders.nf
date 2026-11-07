include { Either; AnyOf; Literal; Optional; OneOrMore; ZeroOrMore; Exactly; Range; AtLeast; Sequence; AnyChar; Digit; WordChar; Whitespace; StartOfLine; EndOfLine; StartOfString; EndOfString; Group; NamedGroup; Capture; NamedCapture; CharClass; NotCharClass; CharRange; MultiRange; DNASequence; StrictDNASequence; DNASequenceWithAmbiguity; ProteinSequence; StrictProteinSequence; ProteinSequenceWithAmbiguity; PhredQuality; Chromosome; StrictChromosome; ReadPair; FastqExtension; VcfExtension; AlignmentExtension; BedExtension; GffGtfExtension; FastaExtension } from 'plugin/nf-pregex'

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
    show('Literal') { Literal('a.b*c+?^$x{1}(y)[z]|w-v\\u/#') }
    show('Literal empty') { Literal('') }
    show('Literal space') { Literal('a b') }
    show('Sequence') { Sequence([Literal('sample'), OneOrMore(Digit())]) }
    show('Sequence single') { Sequence([Digit()]) }
    show('Sequence empty') { Sequence([]) }
    show('Either') { Either(['R1', 'R2', 'a.b']) }
    show('Either one') { Either(['a.b']) }
    show('Either numbers') { Either([1, 2]) }
    show('Either empty') { Either([]) }
    show('Either pattern') { Either([Digit()]) }
    show('AnyOf') { AnyOf([Sequence([Literal('chr'), OneOrMore(Digit())]), Literal('chrX')]) }
    show('AnyOf one') { AnyOf([Digit()]) }
    show('AnyOf empty') { AnyOf([]) }
    show('AnyOf string') { AnyOf(['a']) }
    show('Optional') { Optional(Literal('x')) }
    show('Optional digit') { Optional(Digit()) }
    show('OneOrMore') { OneOrMore(Literal('ab')) }
    show('ZeroOrMore') { ZeroOrMore(WordChar()) }
    show('Exactly digit') { Exactly(Digit(), 3) }
    show('Exactly literal') { Exactly(Literal('ab'), 2) }
    show('Exactly one char literal') { Exactly(Literal('a'), 2) }
    show('Exactly escaped') { Exactly(Literal('.'), 2) }
    show('Exactly class') { Exactly(CharClass('ACGT'), 4) }
    show('Exactly anychar') { Exactly(AnyChar(), 2) }
    show('Exactly negative') { Exactly(Digit(), -1) }
    show('Exactly zero') { Exactly(Digit(), 0) }
    show('Range') { Range(Digit(), 2, 4) }
    show('Range group') { Range(Literal('ab'), 0, 1) }
    show('Range bad') { Range(Digit(), 4, 2) }
    show('Range negative') { Range(Digit(), -1, 2) }
    show('AtLeast') { AtLeast(Whitespace(), 1) }
    show('AtLeast group') { AtLeast(Sequence([Digit(), Digit()]), 2) }
    show('AtLeast negative') { AtLeast(Digit(), -1) }
    show('anchors') { Sequence([StartOfLine(), StartOfString(), AnyChar(), EndOfString(), EndOfLine()]) }
    show('Group') { Group(Digit()) }
    show('Capture') { Capture(Digit()) }
    show('Group named') { Group('num', Digit()) }
    show('NamedGroup') { NamedGroup(Digit(), 'num2') }
    show('NamedCapture') { NamedCapture('n3', Digit()) }
    show('Group bad name') { Group('my_name', Digit()) }
    show('Group digit name') { Group('1a', Digit()) }
    show('Group empty name') { Group('', Digit()) }
    show('CharClass') { CharClass('a-z]^\\') }
    show('NotCharClass') { NotCharClass('ACGT') }
    show('CharClass empty') { CharClass('') }
    show('CharRange') { CharRange('a', 'z') }
    show('CharRange special') { CharRange('-', '^') }
    show('CharRange bad order') { CharRange('z', 'a') }
    show('CharRange long') { CharRange('ab', 'z') }
    show('CharRange long end') { CharRange('a', 'yz') }
    show('MultiRange readme') { MultiRange('a-zA-Z0-9') }
    show('MultiRange quoted') { MultiRange("'a'-'z', 'A'-'Z', \"0\" - \"9\"") }
    show('MultiRange one') { MultiRange("'a'-'f'") }
    show('MultiRange empty') { MultiRange('') }
    show('MultiRange bad order') { MultiRange("'z'-'a'") }
    show('DNASequence') { DNASequence() }
    show('StrictDNASequence') { StrictDNASequence() }
    show('DNASequenceWithAmbiguity') { DNASequenceWithAmbiguity() }
    show('ProteinSequence') { ProteinSequence() }
    show('StrictProteinSequence') { StrictProteinSequence() }
    show('ProteinSequenceWithAmbiguity') { ProteinSequenceWithAmbiguity() }
    show('PhredQuality') { PhredQuality() }
    show('Chromosome') { Chromosome() }
    show('StrictChromosome') { StrictChromosome() }
    show('ReadPair') { ReadPair() }
    show('FastqExtension') { FastqExtension() }
    show('VcfExtension') { VcfExtension() }
    show('AlignmentExtension') { AlignmentExtension() }
    show('BedExtension') { BedExtension() }
    show('GffGtfExtension') { GffGtfExtension() }
    show('FastaExtension') { FastaExtension() }
    def d = Digit()
    show('then') { d.then(Literal('x')) }
    show('optional()') { Literal('ab').optional() }
    show('oneOrMore()') { d.oneOrMore() }
    show('zeroOrMore()') { d.zeroOrMore() }
    show('exactly()') { d.exactly(2) }
    show('range()') { d.range(1, 2) }
    show('atLeast()') { d.atLeast(3) }
    show('group()') { d.group() }
    show('namedGroup()') { d.namedGroup('x') }
    show('toRegex') { d.toRegex() }
    show('gstring') { "${d}" }
    show('children seq') { Sequence([Digit(), Literal('a')]).children() }
    show('children leaf') { d.children() }
    show('getters') { [Exactly(d, 2).getCount(), Range(d, 1, 3).getMin(), Range(d, 1, 3).getMax(), AtLeast(d, 4).getMin(), CharClass('ab').getChars(), PhredQuality().getRegex(), Literal('q').getText(), Group('nm', d).getName(), Either(['a', 'b']).getAlternatives()] }
    show('groovy =~') { ('sample12' =~ Sequence([Literal('sample'), Group(OneOrMore(Digit()))]))[0] }
}
