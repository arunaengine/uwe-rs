//! Checks `file()` and path methods against Nextflow 26.04.6 (Groovy spec, sections 6.4 and 10.7).
// Copyright (c) 2026 The iunctus Contributors
// SPDX-License-Identifier: MIT or Apache-2.0

use std::rc::Rc;

use nextflow_evaluator::interpreter::{Host, Interpreter};
use nextflow_evaluator::lower::lower_module;
use nextflow_parser::parse_script;

struct Launch;

impl Host for Launch {
    fn print(&mut self, _: &str) {}

    fn launch_dir(&self) -> String {
        "/launch".into()
    }
}

/// The value of the body of a function, as its Groovy text, or the error.
fn eval(body: &str) -> Result<String, String> {
    let source = format!("def f() {{\n{body}\n}}");
    let parsed = parse_script(&source);
    assert!(parsed.errors.is_empty(), "{:?}", parsed.errors);
    let module = Rc::new(lower_module(&parsed.tree, &source).expect("lowers"));
    let mut host = Launch;
    let mut interpreter = Interpreter::new(&mut host);
    interpreter
        .call_function(&module, "f", Vec::new())
        .map(|value| value.to_string())
        .map_err(|error| error.to_string())
}

fn value(body: &str) -> String {
    eval(body).expect("evaluates")
}

#[test]
fn remote_paths_print_without_scheme() {
    let s3 = "def s = file('s3://bucket/dir/x.txt')\n[s.toString(), s.toUriString(), s.name, s.parent, s.scheme]";
    assert_eq!(
        value(s3),
        "[/bucket/dir/x.txt, s3://bucket/dir/x.txt, x.txt, /bucket/dir, s3]"
    );
    let web = "def h = file('https://example.com/a/b.txt')\n[h.toString(), h.toUriString(), h.parent, h.scheme]";
    assert_eq!(
        value(web),
        "[/a/b.txt, https://example.com/a/b.txt, /a, https]"
    );
}

#[test]
fn local_paths_and_names() {
    let names =
        "def l = file('rel/../x.tar.gz')\n[l, l.baseName, l.simpleName, l.extension, l.name]";
    assert_eq!(value(names), "[/launch/x.tar.gz, x.tar, x, gz, x.tar.gz]");
    assert_eq!(
        value(
            "[file('/tmp/a/'), file('/').name, file('.bashrc').baseName, file('README').extension]"
        ),
        "[/tmp/a, , , ]"
    );
    assert_eq!(
        value(
            "[file('/tmp/x.gz') + '.bai', file('/tmp') / 'a/b', file('/tmp/a').resolveSibling('b')]"
        ),
        "[/tmp/x.gz.bai, /tmp/a/b, /tmp/b]"
    );
    assert_eq!(
        value(
            "[file('/tmp/a').parent.parent, file('/tmp/a').parent.parent.parent, \"${file('/tmp/a b')}\"]"
        ),
        "[/, null, /tmp/a b]"
    );
    assert_eq!(
        value(
            "[file('/tmp/a') == file('/tmp/a'), file('/tmp/a') == '/tmp/a', file('/a') < file('/b')]"
        ),
        "[true, false, true]"
    );
}

#[test]
fn gunzip_names() {
    let body = "def archive = file('/data/sample.fastq.gz')
def nameWithoutGz = archive.extension == 'gz' ? archive.baseName : archive.name
def extension = file(nameWithoutGz).extension
def name = file(nameWithoutGz).baseName
name + \".${extension}\"";
    assert_eq!(value(body), "sample.fastq");
}

#[test]
fn invalid_patterns() {
    assert_eq!(
        eval("file('')").unwrap_err(),
        "IllegalArgumentException: Argument of `file()` function cannot be empty"
    );
    assert_eq!(
        eval("file(null)").unwrap_err(),
        "IllegalArgumentException: Argument of `file()` function cannot be null"
    );
}

#[test]
fn writing_copying_and_deleting_files() {
    let directory = tempfile::tempdir().expect("a temporary directory");
    let dir = directory.path().display();
    let body = format!(
        "def f = new File('{dir}', 'a.txt')\n\
         f.text = 'hi'\n\
         nextflow.extension.FilesEx.copyTo(f.toPath(), '{dir}/sub/b.txt')\n\
         def g = file('{dir}/sub/b.txt')\n\
         [g.text, f.delete(), f.exists(), g.copyTo(file('{dir}/c.txt')).text, f.delete()]"
    );
    assert_eq!(value(&body), "[hi, true, false, hi, false]");
}

#[test]
fn globs_match_like_java_globs() {
    let directory = tempfile::tempdir().expect("a temporary directory");
    let d = directory.path().display();
    for file in [
        "a_1.fq",
        "a_2.fq",
        "b_1.fq",
        "sub/c_1.fq",
        "sub/deep/e_1.fq",
        ".hidden.fq",
    ] {
        let path = directory.path().join(file);
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(path, "").unwrap();
    }
    let names = |pattern: &str| value(&format!("file('{d}/{pattern}').collect {{ it.name }}"));
    assert_eq!(names("*_1.fq"), "[a_1.fq, b_1.fq]");
    assert_eq!(names("**/*.fq"), "[c_1.fq, e_1.fq]");
    assert_eq!(names("**.fq"), "[a_1.fq, a_2.fq, b_1.fq, c_1.fq, e_1.fq]");
    assert_eq!(names("{a,b}_1.fq"), "[a_1.fq, b_1.fq]");
    assert_eq!(names("*/*.fq"), "[c_1.fq]");
    assert_eq!(
        value(&format!("file('{d}/*_1.fq').getClass().simpleName")),
        "LinkedList"
    );
    assert_eq!(value(&format!("files('{d}/x.fq')")), format!("[{d}/x.fq]"));
    assert_eq!(
        eval(&format!("file('{d}/x.fq', checkIfExists: true)")).unwrap_err(),
        format!("NoSuchFileException: No such file or directory: {d}/x.fq")
    );
}

#[test]
fn output_globs_have_choices_and_classes() {
    use nextflow_evaluator::interpreter::glob::matches;
    assert!(matches("*.{mzML,mgf}", "run.mgf"));
    assert!(matches("out_[ab].txt", "out_b.txt"));
    assert!(!matches("*.txt", "sub/a.txt"));
    assert!(matches("**.txt", "sub/a.txt"));
}

#[test]
fn yaml_load_and_dump() {
    let directory = tempfile::tempdir().expect("a temporary directory");
    let file = directory.path().join("v.yml");
    std::fs::write(&file, "\"P:GUNZIP\":\n    gunzip: 1.13\n    other: 2.0.1\n").unwrap();
    let body = format!(
        "def yaml = new org.yaml.snakeyaml.Yaml()\n\
         def loaded = yaml.load(file('{}'))\n\
         def versions = loaded.collectEntries {{ k, v -> [k.tokenize(':')[-1], v] }}\n\
         [loaded, yaml.dumpAsMap(versions).trim(), yaml.load('') == null]",
        file.display()
    );
    assert_eq!(
        value(&body),
        "[[P:GUNZIP:[gunzip:1.13, other:2.0.1]], GUNZIP:\n  gunzip: 1.13\n  other: 2.0.1, true]"
    );
}

#[test]
fn uris_have_their_parts() {
    assert_eq!(
        value(
            "def u = new URI('https://user@example.org:8080/a/b.txt?x=1#top')\n\
             def f = file('/tmp/x.txt').toUri()\n\
             [u.scheme, u.host, u.port, u.path, u.query, u.fragment, u.authority, u.toString(), \
             f.scheme, f.path, f.host, f.toString(), new URI('data/x.csv').scheme]"
        ),
        "[https, example.org, 8080, /a/b.txt, x=1, top, user@example.org:8080, \
         https://user@example.org:8080/a/b.txt?x=1#top, file, /tmp/x.txt, null, file:///tmp/x.txt, null]"
    );
}

#[test]
fn streams_readers_and_gzip() {
    use std::io::Write;
    let directory = tempfile::tempdir().expect("a temporary directory");
    let plain = directory.path().join("x.txt");
    std::fs::write(&plain, "a\nb\nc\n").unwrap();
    let gz = directory.path().join("x.txt.gz");
    let mut encoder = flate2::write::GzEncoder::new(Vec::new(), flate2::Compression::default());
    encoder.write_all(b"a\nb\nc\n").unwrap();
    std::fs::write(&gz, encoder.finish().unwrap()).unwrap();
    let (plain, gz) = (plain.display(), gz.display());
    let body = format!(
        "def seen = []\nfile('{plain}').eachLine {{ l -> seen << l }}\n\
         def unzipped = []\nnew java.util.zip.GZIPInputStream(file('{gz}').newInputStream()).eachLine {{ l, n -> unzipped << \"${{n}}${{l}}\" }}\n\
         def first\nfile('{gz}').withInputStream {{ first = new BufferedReader(new InputStreamReader(new java.util.zip.GZIPInputStream(it), 'ASCII')).readLine() }}\n\
         [seen, unzipped, first, file('{plain}').newInputStream().withReader {{ r -> r.readLine() + r.readLine() }}]"
    );
    assert_eq!(value(&body), "[[a, b, c], [1a, 2b, 3c], a, ab]");
}
