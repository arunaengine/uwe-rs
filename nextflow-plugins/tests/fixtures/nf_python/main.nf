include { pyFunction } from 'plugin/nf-python'
workflow {
  def r = pyFunction(script: 'echo_kwargs.py', s: 'text', i: 7, bd: 1.25, d: 2.5d, f: 1.5f, b: true, n: null, list: [1, 'a'], set: [3] as Set, map: [k: [1, 2]], p: file('/tmp/x.txt'), dur: Duration.of('90s'), mem: MemoryUnit.of('1 KB'), inf: Double.POSITIVE_INFINITY)
  println "echo: ${r}"
  println "classes: " + r.collect { k, v -> "${k}=${v?.getClass()?.simpleName}" }.join(' ')
  println "inline: " + pyFunction('''
      import sys
      print('to stdout'); print('to stderr', file=sys.stderr)
      nf.output(a=1, b=[True, None, 2.5, (1, 2)], c={1: 'x'}, dec=__import__('decimal').Decimal('1.10'))
  ''', x: 1)
  println "positional: " + pyFunction('nf.output(1, "two")')
  println "args-type: " + pyFunction('nf.output(t=type(nf.args).__name__, keys=sorted(nf.args))', z: 1, y: 2)
}
