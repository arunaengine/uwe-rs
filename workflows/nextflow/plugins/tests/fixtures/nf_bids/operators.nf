include { groupTupleBy; joinBy; combineBy } from 'plugin/nf-bids'

def out(String label, ch) {
    ch.toList().subscribe { items -> println "${label} => ${items}" }
}

workflow {
    def items = [[subject: 'sub-01', run: 2, f: 'a'], [subject: 'sub-02', run: 1, f: 'c'], [subject: 'sub-01', run: 1, f: 'b'], [subject: 'sub-01', run: 3, f: 'd']]
    out('groupTupleBy', channel.fromList(items).groupTupleBy { it.subject })
    out('groupTupleBy composite', channel.fromList(items).groupTupleBy { [it.subject, it.run > 1] })
    out('groupTupleBy sort closure', channel.fromList(items).groupTupleBy({ it.subject }, [sort: { it.run }]))
    out('groupTupleBy size 2', channel.fromList(items).groupTupleBy({ it.subject }, [size: 2]))
    out('groupTupleBy size 2 no remainder', channel.fromList(items).groupTupleBy({ it.subject }, [size: 2, remainder: false]))
    out('groupTupleBy tuples', channel.of(['x', 1], ['y', 2], ['x', 3]).groupTupleBy { it[0] })
    def left = [[id: 'A', l: 1], [id: 'B', l: 2], [id: 'C', l: 3], [id: 'A', l: 4]]
    def right = [[id: 'A', r: 'x'], [id: 'B', r: 'y'], [id: 'D', r: 'z']]
    out('joinBy', channel.fromList(left).joinBy(channel.fromList(right)) { it.id })
    out('joinBy remainder', channel.fromList(left).joinBy(channel.fromList(right), { it.id }, [remainder: true]))
    out('joinBy two keys', channel.fromList([[sid: 'A', v: 1]]).joinBy(channel.fromList([[pid: 'A', w: 2]]), { it.sid }, { it.pid }))
    out('joinBy clash', channel.fromList([[id: 'A', v: 1]]).joinBy(channel.fromList([[id: 'A', v: 2]])) { it.id })
    out('joinBy tuples', channel.of(['A', 1]).joinBy(channel.of(['A', 2])) { it[0] })
    out('combineBy', channel.fromList([[id: 'sub-01', age: 25], [id: 'sub-02', age: 30]]).combineBy(channel.fromList([[id: 'sub-01', session: 'ses-01'], [id: 'sub-01', session: 'ses-02'], [id: 'sub-02', session: 'ses-01'], [id: 'sub-03', session: 'x']]), { it.id }))
    out('combineBy two keys', channel.fromList([[a: 1, x: 'l']]).combineBy(channel.fromList([[b: 1, y: 'r'], [b: 1, y: 's']]), { it.a }, { it.b }))
}
