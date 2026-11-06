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
