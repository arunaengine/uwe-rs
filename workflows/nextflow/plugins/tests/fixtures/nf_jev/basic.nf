include { jev; noul; choice; score } from 'plugin/nf-jev'
workflow {
def q = [q1: noul('The sample is human.'), q2: choice('Which assay?', ['RNA-seq': 'transcriptome', 'ATAC-seq': 'chromatin']), q3: score('How sure?', ['low', 'mid', 'high'])]
println 'question: ' + q.q2
println 'noul2: ' + noul('Seen?', [true: 'yes means', false: 'no means'])
def a = jev([title: 'RNA-seq of liver é "q" / x', n: 1.5, k: [1, 2]], q)
println 'answers: ' + a
println 'classes: ' + a.q2.confidence.getClass().simpleName + ' ' + a.q1.noul.getClass().simpleName
def b = jev([title: 'RNA-seq of liver é "q" / x', n: 1.5, k: [1, 2]], q)
println 'second (cache): ' + (a == b)
println jev('plain text state', [q1: noul('x')])
}
