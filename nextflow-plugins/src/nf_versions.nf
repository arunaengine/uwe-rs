/*
 * The function of the nf-versions plugin 0.8.0 that works on channels, in Nextflow script.
 */

// The items of the channel, each line indented, in one file `all_mqc_versions.yml` after the
// prefix, as `collectFile` writes and orders them.
def collateVersions(versions, prefix = '', indentRestBy = 0) {
    def indent = ' ' * indentRestBy
    versions
        .map { item -> item.toString().split('\n').collect { line -> indent + line }.join('\n') }
        .collectFile(name: 'all_mqc_versions.yml', newLine: true, seed: prefix)
}
