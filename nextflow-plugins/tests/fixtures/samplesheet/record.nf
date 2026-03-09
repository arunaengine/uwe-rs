// Records the expected rows with Nextflow 26.04.6 and nf-schema 2.5.1: copy this directory,
// add `plugins { id 'nf-schema@2.5.1' }` to a nextflow.config and run `nextflow run record.nf`.
include { samplesheetToList } from 'plugin/nf-schema'
workflow {
    println samplesheetToList("sheet.csv", "schema.json")
    println samplesheetToList("sheet2.csv", "schema2.json")
    println samplesheetToList("sheet2.csv", "schema3.json")
    println samplesheetToList("sheet.tsv", "schema.json")
}
