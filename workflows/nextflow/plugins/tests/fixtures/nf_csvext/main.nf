include { csv_concat; csv_sort; csv_trim; csv_prettyprint; csv_create } from 'plugin/nf-csvext'
def show(String label, p) { println "== ${label} (${p.getClass().simpleName}, name ${p.name.replaceAll(/[0-9]+/, 'N')})"; print p.text }
workflow {
  def a = file("${projectDir}/data/a.csv"); def b = file("${projectDir}/data/b.csv")
  show('concat header', csv_concat(a, b, header: true))
  show('concat no header', csv_concat(a, b))
  show('concat list', csv_concat(a, [b, b], header: true))
  show('sort by age', csv_sort(a, column: 'age'))
  show('sort by 0', csv_sort(a))
  show('sort by AGE (case)', csv_sort(a, column: 'AGE'))
  show('trim column', csv_trim(a, column: 'age'))
  show('trim columns text', csv_trim(a, columns: 'age,city'))
  show('trim columns list', csv_trim(a, columns: ['age', 'city']))
  show('trim index', csv_trim(a, column: 1))
  show('pretty', csv_prettyprint(a, newSep: ';'))
  channel.of([id: 1, name: 'x', n: 0], [id: 2, name: 'y']).csv_create(headers: ['name', 'id', 'n', 'z'], sep: ';').view { "create map: " + it }
  channel.of(['a', 1], ['b', 2]).csv_create(sep: '|').view { "create list: " + it }
  channel.of('p', 'q').csv_create([headers: ['h']], { it.toUpperCase() }).view { "create closure: " + it }
}
