//! Reads PEP sample tables from Excel files as pandas does with the settings of peppy.
// Copyright (c) 2026 The uwe Contributors
// SPDX-License-Identifier: MIT or Apache-2.0

use std::io::Write;

use workflow_graph::Launch;

/// The parts of a small workbook whose first sheet holds `rows`; style 1 is a date.
fn workbook(path: &std::path::Path, rows: &str) {
    let parts = [
        (
            "[Content_Types].xml",
            r#"<?xml version="1.0" encoding="UTF-8"?><Types xmlns="http://schemas.openxmlformats.org/package/2006/content-types"><Default Extension="rels" ContentType="application/vnd.openxmlformats-package.relationships+xml"/><Default Extension="xml" ContentType="application/xml"/><Override PartName="/xl/workbook.xml" ContentType="application/vnd.openxmlformats-officedocument.spreadsheetml.sheet.main+xml"/><Override PartName="/xl/worksheets/sheet1.xml" ContentType="application/vnd.openxmlformats-officedocument.spreadsheetml.worksheet+xml"/><Override PartName="/xl/styles.xml" ContentType="application/vnd.openxmlformats-officedocument.spreadsheetml.styles+xml"/></Types>"#.to_owned(),
        ),
        (
            "_rels/.rels",
            r#"<?xml version="1.0" encoding="UTF-8"?><Relationships xmlns="http://schemas.openxmlformats.org/package/2006/relationships"><Relationship Id="rId1" Type="http://schemas.openxmlformats.org/officeDocument/2006/relationships/officeDocument" Target="xl/workbook.xml"/></Relationships>"#.to_owned(),
        ),
        (
            "xl/workbook.xml",
            r#"<?xml version="1.0" encoding="UTF-8"?><workbook xmlns="http://schemas.openxmlformats.org/spreadsheetml/2006/main" xmlns:r="http://schemas.openxmlformats.org/officeDocument/2006/relationships"><sheets><sheet name="samples" sheetId="1" r:id="rId1"/></sheets></workbook>"#.to_owned(),
        ),
        (
            "xl/_rels/workbook.xml.rels",
            r#"<?xml version="1.0" encoding="UTF-8"?><Relationships xmlns="http://schemas.openxmlformats.org/package/2006/relationships"><Relationship Id="rId1" Type="http://schemas.openxmlformats.org/officeDocument/2006/relationships/worksheet" Target="worksheets/sheet1.xml"/><Relationship Id="rId2" Type="http://schemas.openxmlformats.org/officeDocument/2006/relationships/styles" Target="styles.xml"/></Relationships>"#.to_owned(),
        ),
        (
            "xl/styles.xml",
            r#"<?xml version="1.0" encoding="UTF-8"?><styleSheet xmlns="http://schemas.openxmlformats.org/spreadsheetml/2006/main"><cellXfs count="2"><xf numFmtId="0"/><xf numFmtId="14" applyNumberFormat="1"/></cellXfs></styleSheet>"#.to_owned(),
        ),
        (
            "xl/worksheets/sheet1.xml",
            format!(r#"<?xml version="1.0" encoding="UTF-8"?><worksheet xmlns="http://schemas.openxmlformats.org/spreadsheetml/2006/main"><sheetData>{rows}</sheetData></worksheet>"#),
        ),
    ];
    let mut archive = zip::ZipWriter::new(std::fs::File::create(path).expect("a workbook"));
    for (name, text) in parts {
        archive
            .start_file(name, zip::write::SimpleFileOptions::default())
            .expect("a part");
        archive.write_all(text.as_bytes()).expect("the part");
    }
    archive.finish().expect("the workbook");
}

/// pandas 2 reads this sheet with peppy's `read_excel(dtype=str, keep_default_na=False,
/// na_values=[""])`: whole numbers without `.0`, `True`, dates with their time, the blank row
/// left out, and the empty cell of `s2` as `""` in the sample table.
#[test]
fn reads_excel_tables() {
    let folder = tempfile::tempdir().expect("a folder");
    let text = |cell: &str, value: &str| {
        format!(r#"<c r="{cell}" t="inlineStr"><is><t>{value}</t></is></c>"#)
    };
    let rows = [
        format!(
            r#"<row r="1">{}{}{}{}{}</row>"#,
            text("A1", "sample_name"),
            text("B1", "count"),
            text("C1", "ratio"),
            text("D1", "flag"),
            text("E1", "day")
        ),
        format!(
            r#"<row r="2">{}<c r="B2"><v>3</v></c><c r="C2"><v>0.5</v></c><c r="D2" t="b"><v>1</v></c><c r="E2" s="1"><v>45292</v></c></row>"#,
            text("A2", "s1")
        ),
        format!(
            r#"<row r="4">{}<c r="C4"><v>2.25</v></c><c r="D4" t="b"><v>0</v></c><c r="E4" s="1"><v>45351</v></c></row>"#,
            text("A4", "s2")
        ),
    ];
    std::fs::create_dir_all(folder.path().join("pep")).expect("a project folder");
    workbook(&folder.path().join("pep/samples.xlsx"), &rows.concat());
    std::fs::write(
        folder.path().join("pep/config.yaml"),
        "pep_version: 2.0.0\nsample_table: samples.xlsx\n",
    )
    .expect("a project");
    std::fs::write(
        folder.path().join("Snakefile"),
        "pepfile: 'pep/config.yaml'\nrule all:\n    input: expand('{s}.txt', s=pep.sample_table['sample_name'])\nrule make:\n    output: '{s}.txt'\n    params: v=lambda w: '|'.join(pep.get_sample(w.s)[key] for key in ['count', 'ratio', 'flag', 'day'])\n    shell: 'echo {params.v} > {output}'\n",
    )
    .expect("a Snakefile");
    let launch = Launch {
        target: folder.path().to_path_buf(),
        scratch: folder.path().join("scratch"),
        simulated: true,
        ..Launch::default()
    };
    let mut driver = snakemake_runtime::driver(&launch).expect("a driver");
    let mut scripts: Vec<String> = (driver.start().expect("starts").iter())
        .filter_map(|task| task.script.clone())
        .collect();
    scripts.sort();
    assert_eq!(scripts.len(), 2, "{scripts:?}");
    assert!(
        scripts[0].contains("3|0.5|True|2024-01-01 00:00:00 > s1.txt"),
        "{scripts:?}"
    );
    assert!(
        scripts[1].contains("|2.25|False|2024-02-29 00:00:00 > s2.txt"),
        "{scripts:?}"
    );
}
