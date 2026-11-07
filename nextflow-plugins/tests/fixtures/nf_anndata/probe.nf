include { anndata } from 'plugin/nf-anndata'

def cls(v) {
    return v == null ? 'null' : v.getClass().simpleName
}

def head(arr) {
    if (arr == null) {
        return 'null'
    }
    def list = arr as List
    def shown = list.take(8).collect { v -> "${v}<${cls(v)}>" }
    return "n=${list.size()} " + shown.join(', ') + (list.size() > 8 ? ', ...' : '')
}

def show(String label, Closure c) {
    try {
        def v = c.call()
        println "  ${label} => ${v} (${cls(v)})"
    }
    catch (Throwable e) {
        def r = e.cause ?: e
        println "  ${label} !! ${e.getClass().name} / ${r.getClass().name}: ${r.message}"
    }
}

def frame(String which, df) {
    show("${which}.colnames") { df.colnames as List }
    show("${which}.colnames class") { df.colnames.getClass().simpleName }
    show("${which}.rownames") { head(df.rownames) }
    show("${which}.size") { df.size }
    show("${which}.index.name") { df.index.name }
    show("${which}.index.values") { head(df.index.values) }
    show("${which}.index.size()") { df.index.size() }
    show("${which}.index.n_unique()") { df.index.n_unique() }
    show("${which}.index.unique() sorted") { df.index.unique().collect { it }.sort { a, b -> a <=> b }.take(8) }
    show("${which}.index.contains(first)") { df.index.contains(df.rownames ? df.rownames[0] : 'x') }
    show("${which}.index.contains(nope)") { df.index.contains('__nope__') }
    df.colnames.each { c ->
        show("${which}.get('${c}').data") { head(df.get(c).data) }
        show("${which}.get('${c}').n_unique()") { df.get(c).n_unique() }
        show("${which}.get('${c}').unique() sorted") { df.get(c).unique().collect { it == null ? 'null' : it.toString() }.sort().take(8) }
    }
    show("${which}.get('__missing__')") { df.get('__missing__') }
}

workflow {
    params.files.toString().tokenize(',').each { p ->
        println "=== ${p.replaceAll('.*/test_cases/', '').replaceAll('.*/data/', '')}"
        def ad
        try {
            ad = anndata(file(p))
        }
        catch (Throwable e) {
            def r = e.cause ?: e
            println "  anndata() !! ${e.getClass().name} / ${r.getClass().name}: ${r.message?.replace(params.base.toString(), '<data>')}"
            return
        }
        show('class') { ad.getClass().name }
        show('n_obs') { ad.n_obs }
        show('n_vars') { ad.n_vars }
        show('obs_names') { head(ad.obs_names) }
        show('var_names') { head(ad.var_names) }
        [layers: ad.layers, obsm: ad.obsm, varm: ad.varm, obsp: ad.obsp, varp: ad.varp, uns: ad.uns].each { k, v ->
            show(k) { v.collect { it }.sort() }
        }
        show('layers class') { ad.layers.getClass().simpleName }
        show('yaml') { groovy.json.JsonOutput.toJson(ad.yaml) }
        show('toString lines sorted') { ad.toString().readLines().with { ls -> [ls[0]] + ls.drop(1).sort() } }
        frame('obs', ad.obs)
        frame('var', ad.var)
        show('close()') { ad.close() }
    }
}
