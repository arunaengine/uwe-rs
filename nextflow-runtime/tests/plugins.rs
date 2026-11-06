//! Scripts that use the objects, factories and operators of native plugins.
// Copyright (c) 2026 The uwe Contributors
// SPDX-License-Identifier: MIT or Apache-2.0

use std::path::Path;

use nextflow_evaluator::interpreter::Host;
use nextflow_runtime::Loader;

/// Collects what a script prints, with the launch directory of the run.
struct Output {
    text: String,
    launch: String,
}

impl Host for Output {
    fn print(&mut self, text: &str) {
        self.text.push_str(text);
    }

    fn launch_dir(&self) -> String {
        self.launch.clone()
    }
}

/// What the script prints when it runs in a folder with these files, or the error.
fn run(script: &str, files: &[(&str, &str)]) -> Result<String, String> {
    let root = tempfile::tempdir().expect("a temporary directory");
    for (name, text) in files.iter().chain(&[("main.nf", script)]) {
        let path = root.path().join(name);
        std::fs::create_dir_all(path.parent().unwrap_or(Path::new("/"))).expect("a folder");
        std::fs::write(path, text).expect("writes");
    }
    let module = Loader::default()
        .load(&root.path().join("main.nf"))
        .map_err(|error| error.to_string())?;
    let mut output = Output {
        text: String::new(),
        launch: root.path().display().to_string(),
    };
    nextflow_runtime::run(&module, &mut output).map_err(|error| error.to_string())?;
    Ok(output.text)
}

#[test]
fn runs_fgbio_objects() {
    let script = "include { readStructure; fromSampleSheet } from 'plugin/nf-fgbio'\n\
        workflow {\n\
            def rs = readStructure('4M3S12B+T')\n\
            def seg = rs.apply(2)\n\
            println \"${rs} ${rs.segments().size()} ${seg.offset} ${seg.length().get()} ${rs.templateSegments()}\"\n\
            println([rs: readStructure('8m142t'), same: readStructure('8M142T') == readStructure('8m142t')])\n\
            try {\n\
                readStructure('10B20C+T')\n\
            } catch (java.lang.reflect.InvocationTargetException ex) {\n\
                println \"caught: ${ex.getCause().getMessage()}\"\n\
            }\n\
            channel.fromSampleSheet(lane: 1, 'sheet.csv').map { s ->\n\
                \"${s.sampleName} ${s.project().getOrElse { 'none' }} ${s.lane}\"\n\
            }.view()\n\
        }\n";
    let sheet =
        "[Data]\nLane,Sample_ID,Sample_Name,Sample_Project\n1,S1,N1,P\n1,S2,N2,\n2,S3,N3,\n";
    assert_eq!(
        run(script, &[("sheet.csv", sheet)]).expect("runs"),
        "4M3S12B+T 4 7 12 Vector(+T)\n[rs:8M142T, same:true]\n\
         caught: Read structure segment had unknown type: 10B[20C]+T\n\
         N1 P Some(1)\nN2 none Some(1)\n"
    );
}

#[test]
fn collates_versions() {
    let script = "include { collateVersions } from 'plugin/nf-versions'\n\
        workflow {\n\
            def items = channel.of('zeta: \"1.0\"', 'alpha: \"2.0\"\\nbeta: \"3\"\\n', 'gamma: \"0\"')\n\
            collateVersions(items, 'TOOLS:\\n', 2).subscribe { f -> println \"${f.name}\\n${f.text}\" }\n\
        }\n";
    assert_eq!(
        run(script, &[]).expect("runs"),
        "all_mqc_versions.yml\nTOOLS:\n\n  gamma: \"0\"\n  alpha: \"2.0\"\n  beta: \"3\"\n  \
         zeta: \"1.0\"\n\n"
    );
    let plain = script.replace("items, 'TOOLS:\\n', 2", "items");
    assert_eq!(
        run(&plain, &[]).expect("runs"),
        "all_mqc_versions.yml\nalpha: \"2.0\"\nbeta: \"3\"\nzeta: \"1.0\"\ngamma: \"0\"\n\n"
    );
}

#[test]
fn runs_ped_objects() {
    let script = r#"include { initializePed } from 'plugin/nf-ped'

def show(String label, Closure c) {
    try {
        def v = c.call()
        println "${label} => ${v} (${v == null ? 'null' : v.getClass().name})"
    }
    catch (Throwable e) {
        println "${label} !! ${e.getClass().name}: ${e.message}"
    }
}

workflow {
    def ped = initializePed(file('a.ped'))
    show('entries') { ped.getEntries().collect { it.toString().replace('\t', '|') } }
    show('families') { ped.getFamilies() }
    show('individuals') { ped.getIndividuals() }
    show('byFamily fam1') { ped.getEntriesByFamily('fam1').collect { it.individual } }
    show('familiesFromIndividual kid') { ped.getFamiliesFromIndividual('kid') }
    show('individualsFromFamily fam1') { ped.getIndividualsFromFamily('fam1') }
    show('byFamily none') { ped.getEntriesByFamily('nope') }
    def m = ped.getEntriesByIndividual('mom').toList()[0]
    show('mom fields') { [m.family, m.individual, m.father, m.mother, m.sex, m.phenotype, m.additionalFields] }
    def overwrite = initializePed([overwrite: true], [file('a.ped')])
    show('overwrite import') { overwrite.getEntries().collect { it.toString().replace('\t', '|') } }
    def spaced = initializePed(file('a.ped'))
    spaced.importPed([sep: ' '], file('b.ped'))
    show('import b skip dup') { spaced.getEntries().collect { it.toString().replace('\t', '|') } }
    show('regex sep') { initializePed([sep: '\\s+'], file('b.ped')).getEntries().size() }
    def k = initializePed(file('a.ped')).getEntriesByIndividual('kid').toList()[0]
    k.setFather('NA')
    k.sex = 2
    k.setPhenotype('9')
    k.setMother('0')
    k.setAdditionalFields(['x'])
    show('after setters') { k.toString().replace('\t', '|') }
    k.setFamily('0')
    show('null family') { "${k}".replace('\t', '|') }
    def e = ped.getEntries().toList()[0]
    def other = initializePed()
    other.addEntry(e)
    other.addEntry(e)
    show('addEntry twice') { other.getEntries().size() }
    other.addEntries(ped.getEntries())
    show('addEntries') { other.getEntries().size() }
    other.setEntries(ped.getEntries().toList())
    show('setEntries list') { [other.getEntries().size(), other.getEntries().getClass().name] }
    show('identity') { initializePed(file('a.ped')).getEntries().toList()[0] == initializePed(file('a.ped')).getEntries().toList()[0] }
    show('writePed again') { initializePed(file('b.ped'), sep: ' ').writePed('out/explicit.ped').text.readLines().size() }
    show('string arg') { initializePed('a.ped') }
}
"#;
    let a = "#FID\tIID\tPAT\tMAT\tSEX\tPHENO\nfam1\tkid\tdad\tmom\t1\t2\nfam1\tdad\t0\t0\t1\t1\n\
             fam1\tmom\tNA\t\t2\t-9\nfam2\tsolo\t0\t0\tother\t0\textra1\textra2\n\
             fam1\tkid\tX\tY\t2\t1\n";
    let b = "fam1 kid newdad newmom 2 2\nfam3 z 0 0 2 1\n";
    let existing = "1\t2\t3\t4\t5\t6\n1\t2\t3\t4\t5\t7\n1\t2\t3\t4\t5\t8\n";
    let files = [("a.ped", a), ("b.ped", b), ("out/explicit.ped", existing)];
    assert_eq!(
        run(script, &files).expect("runs"),
        "entries => [fam1|kid|dad|mom|1|2, fam1|dad|0|0|1|1, fam1|mom|0|0|2|0, \
         fam2|solo|0|0|0|0|extra1|extra2] (java.util.ArrayList)\n\
         families => [fam1, fam2] (java.util.LinkedHashSet)\n\
         individuals => [kid, dad, mom, solo] (java.util.LinkedHashSet)\n\
         byFamily fam1 => [kid, dad, mom] (java.util.ArrayList)\n\
         familiesFromIndividual kid => [fam1] (java.util.LinkedHashSet)\n\
         individualsFromFamily fam1 => [kid, dad, mom] (java.util.LinkedHashSet)\n\
         byFamily none => [] (java.util.LinkedHashSet)\n\
         mom fields => [fam1, mom, null, null, 2, 0, []] (java.util.ArrayList)\n\
         overwrite import => [fam1|dad|0|0|1|1, fam1|mom|0|0|2|0, \
         fam2|solo|0|0|0|0|extra1|extra2, fam1|kid|X|Y|2|1] (java.util.ArrayList)\n\
         import b skip dup => [fam1|kid|dad|mom|1|2, fam1|dad|0|0|1|1, fam1|mom|0|0|2|0, \
         fam2|solo|0|0|0|0|extra1|extra2, fam3|z|0|0|2|1] (java.util.ArrayList)\n\
         regex sep => 2 (java.lang.Integer)\n\
         after setters => fam1|kid|0|0|2|0|x (java.lang.String)\n\
         null family => null|kid|0|0|2|0|x (java.lang.String)\n\
         addEntry twice => 1 (java.lang.Integer)\n\
         addEntries => 4 (java.lang.Integer)\n\
         setEntries list => [4, java.util.LinkedHashSet] (java.util.ArrayList)\n\
         identity => false (java.lang.Boolean)\n\
         writePed again => 3 (java.lang.Integer)\n\
         string arg !! groovy.lang.MissingMethodException: No signature of method: \
         nvnieuwk.ped.PedExtension.initializePed() is applicable for argument types: (String) \
         values: [a.ped]\nPossible solutions: initializePed(), initializePed(java.nio.file.Path), \
         initializePed(java.util.Map), initializePed(java.util.Map, java.nio.file.Path), \
         initializePed(java.util.Map, java.util.List)\n"
    );
}

/// The output of a recorded nf-pregex probe script, and the recorded output.
fn pregex_probe(name: &str) -> (String, String) {
    let folder = concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/../nextflow-plugins/tests/fixtures/nf_pregex"
    );
    let read = |file: String| std::fs::read_to_string(format!("{folder}/{file}")).expect("reads");
    let output = run(&read(format!("{name}.nf")), &[]).expect("runs");
    (output, read(format!("{name}.out")))
}

/// The text without the lines from the one that starts with `first` to the one before `next`.
fn without(text: &str, first: &str, next: &str) -> String {
    let start = text.find(first).expect("the first line");
    let end = text[start..].find(next).expect("the next line") + start;
    format!("{}{}", &text[..start], &text[end..])
}

#[test]
fn builds_pregex_patterns() {
    let (output, recorded) = pregex_probe("builders");
    // uwe's lists have no unmodifiable class of their own.
    let recorded = recorded.replace(
        "(java.util.Collections$UnmodifiableRandomAccessList)",
        "(java.util.ArrayList)",
    );
    assert_eq!(output, recorded);
}

#[test]
fn runs_pregex_methods() {
    let (output, recorded) = pregex_probe("methods");
    // uwe's key sets are sets, and its regex errors have their own wording.
    let recorded = recorded.replace("LinkedHashMap$LinkedKeySet", "LinkedHashSet");
    let invalid = "bad regex test !! java.util.regex.PatternSyntaxException / \
                   java.util.regex.PatternSyntaxException: ";
    assert!(output.contains(invalid), "{output}");
    assert_eq!(
        without(&output, "bad regex test", "report class"),
        without(&recorded, "bad regex test", "report class")
    );
    let script = "include { Digit } from 'plugin/nf-pregex'\n\
        workflow {\n\
            try { Digit().exactly(-1) } catch (e) { println \"${e.getClass().name}: ${e.message}\" }\n\
        }\n";
    assert_eq!(
        run(script, &[]).expect("runs"),
        "java.lang.IllegalArgumentException: Count must be non-negative\n"
    );
}
