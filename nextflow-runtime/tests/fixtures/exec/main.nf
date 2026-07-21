// Processes with `exec:` sections, which Nextflow runs in its own process and iunctus in its
// interpreter (tests/exec.rs). `record.sh` recorded what Nextflow 26.04.6 publishes in
// nextflow-26.04.6/.

process MAKE_SHEET {
    publishDir "results/sheets", mode: "copy"

    input:
    val sample

    output:
    path "${sample}.csv", emit: sheet
    val greeting, emit: greeting

    exec:
    greeting = "hello ${sample}"
    def sheet = task.workDir.resolve("${sample}.csv")
    sheet.text = "sample,length\n${sample},${sample.size()}\n"
}

process COUNT {
    container "debian:stable-slim"
    publishDir "results/counts", mode: "copy"

    input:
    path sheet

    output:
    path "${sheet.baseName}.count"

    script:
    """
    wc -l < ${sheet} > ${sheet.baseName}.count
    """
}

process CHECK {
    publishDir "results/checks", mode: "copy"

    input:
    path sheet

    output:
    val message, emit: message
    path "*.ok", emit: ok

    exec:
    message = "${sheet.name} checked"
    task.workDir.resolve("${sheet.baseName}.ok").text = "ok\n"
}

workflow {
    samples = channel.of("A", "BB")
    MAKE_SHEET(samples)
    COUNT(MAKE_SHEET.out.sheet)
    CHECK(MAKE_SHEET.out.sheet)
    MAKE_SHEET.out.greeting.mix(CHECK.out.message).toSortedList().view()
}
