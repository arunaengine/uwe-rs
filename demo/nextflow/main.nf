// The iunctus demo in Nextflow: three greetings, each made and shouted by its own task (on
// any node of the cluster), then one report that joins them, made with Python.
// Run it with `iunctus run main.nf`; the results go to `results/`.

params.names = 'Ada,Grace,Linus'
params.outdir = 'results'

process GREET {
    container 'debian:stable-slim'
    publishDir "${params.outdir}/greetings", mode: 'copy'

    input:
    val name

    output:
    path "${name}.txt"

    script:
    """
    printf 'Hello, %s!\\ngreeted on %s\\n' '${name}' "\$(cat /proc/sys/kernel/hostname)" > ${name}.txt
    """
}

process SHOUT {
    container 'debian:stable-slim'

    input:
    path greeting

    output:
    path "${greeting.baseName}.loud.txt"

    script:
    """
    {
        head -n 1 ${greeting} | tr a-z A-Z
        tail -n +2 ${greeting}
        echo "shouted on \$(cat /proc/sys/kernel/hostname)"
    } > ${greeting.baseName}.loud.txt
    """
}

process REPORT {
    container 'python:3.13-slim'
    publishDir params.outdir, mode: 'copy'

    input:
    path loud

    output:
    path 'report.txt'

    script:
    """
    python3 - Nextflow ${loud} <<'END'
    import pathlib, sys
    language, files = sys.argv[1], sorted(sys.argv[2:], key=lambda f: pathlib.Path(f).name)
    lines = [f"The iunctus demo in {language}: {len(files)} greetings", ""]
    for name in files:
        text = pathlib.Path(name).read_text().splitlines()
        lines.append(f"{text[0]:<14} {', '.join(text[1:])}")
    pathlib.Path("report.txt").write_text("\\n".join(lines) + "\\n")
    END
    """
}

workflow {
    names = channel.fromList(params.names.tokenize(','))
    REPORT(SHOUT(GREET(names)).collect(sort: true))
    REPORT.out.view { report -> report.text }
}
