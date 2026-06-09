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
fn uris_resolve_references() {
    // Groovy 4.0.31 on Java 17 prints this.
    let uris = "def u = new URI('https://h/a/b/sheet.csv')\n\
                [u.resolve('data/x.fq'), u.resolve('../y'), u.resolve('/abs'), u.resolve('s3://b/k'), \
                u.resolve('//other/p'), u.resolve('?q=1'), u.resolve('#f'), \
                new URI('file:/d/s.csv').resolve('x'), new URI('/d/s.csv').resolve('./x/./y'), \
                new URI('a/b').resolve('../../c'), u.resolve('x/..')].collect { it.toString() }";
    assert_eq!(
        value(uris),
        "[https://h/a/b/data/x.fq, https://h/a/y, https://h/abs, s3://b/k, https://other/p, \
         https://h/a/b/?q=1, https://h/a/b/sheet.csv#f, file:/d/x, /d/x/y, ../c, https://h/a/b/]"
    );
}

#[test]
fn relative_files_keep_their_text() {
    // Groovy 4.0.31 prints this.
    let files = "def f = new File('a/b.txt')\n[f, f.isAbsolute(), new File('/x').isAbsolute(), \
                 f.name, f.toPath().isAbsolute(), new File('/x/y').toPath().fileName.isAbsolute(), \
                 f.toPath().toAbsolutePath().isAbsolute()]";
    assert_eq!(
        value(files),
        "[a/b.txt, false, true, b.txt, false, false, true]"
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

/// `withWriter` and `bytes`, as nf-core's old completion handlers write reports and read the
/// logo; Groovy 4 gives `[done, 'abc\n', YWJjCg==, YW|Jj|Cg|==]`.
#[test]
fn writers_and_bytes_of_files() {
    let directory = tempfile::tempdir().expect("a temporary directory");
    let dir = directory.path().display();
    let body = format!(
        "def f = new File('{dir}/w.txt')\n\
         def r = f.withWriter {{ w -> w << 'a' << 'b'; w.println('c'); 'done' }}\n\
         [r, f.text.inspect(), f.bytes.encodeBase64().toString(), \
         f.bytes.encodeBase64().toString().tokenize('\\n')*.toList()*.collate(2)*.collect {{ it.join() }}\
         .flatten().join('|')]"
    );
    assert_eq!(value(&body), "[done, 'abc\\n', YWJjCg==, YW|Jj|Cg|==]");
}

#[test]
fn directories_are_made_and_files_copied_into_them() {
    let directory = tempfile::tempdir().expect("a temporary directory");
    let dir = directory.path().display();
    let body = format!(
        "def a = [file('{dir}/out/input').mkdir(), file('{dir}/out2').mkdir(), \
         file('{dir}/out2').mkdir(), file('{dir}/out2/a/b').mkdirs(), file('{dir}/out2/a/b').mkdirs()]\n\
         file('{dir}/x.txt').text = 'hi'\n\
         file('{dir}/x.txt').copyTo('{dir}/out2/a')\n\
         a + [file('{dir}/out2/a/x.txt').text, file('{dir}/x.txt').mkdirs()]"
    );
    assert_eq!(value(&body), "[false, true, false, true, true, hi, false]");
}

#[test]
fn file_names_as_paths() {
    assert_eq!(
        value("def f = file('/x/y/z.txt').fileName\n[f.name, f, f.getClass().getSimpleName()]"),
        "[z.txt, z.txt, UnixPath]"
    );
}

#[test]
fn csv_files_split_into_records() {
    let directory = tempfile::tempdir().expect("a temporary directory");
    let dir = directory.path().display();
    let body = format!(
        "file('{dir}/c.txt').text = 'chr1 1\\nchr2 2\\n\\n'\n\
         file('{dir}/h.csv').text = 'a,b\\n1,\"x,y\"\\n 2 , z\\n'\n\
         [file('{dir}/c.txt').splitCsv(header: false, sep: ' '), \
         file('{dir}/h.csv').splitCsv(header: true, strip: true, limit: 1), \
         file('{dir}/h.csv').splitCsv(skip: 1, quote: '\"')]"
    );
    assert_eq!(
        value(&body),
        "[[[chr1, 1], [chr2, 2]], [[a:1, b:\"x]], [[1, x,y], [ 2 ,  z]]]"
    );
}

#[test]
fn input_streams_read_bytes() {
    let directory = tempfile::tempdir().expect("a temporary directory");
    let dir = directory.path().display();
    let body = format!(
        "file('{dir}/two.txt').text = 'ab'\n\
         def s = java.nio.file.Files.newInputStream(file('{dir}/two.txt'))\n\
         [s.read(), s.read(), s.read()]"
    );
    assert_eq!(value(&body), "[97, 98, -1]");
}

#[test]
fn paths_below_files_do_not_exist() {
    let directory = tempfile::tempdir().expect("a temporary directory");
    let dir = directory.path().display();
    let body = format!(
        "file('{dir}/a').text = 'x'\n[file('{dir}/a/b').exists(), file('{dir}/a/b').isDirectory(), \
         file('{dir}/a').listFiles(), file('{dir}/c').listFiles()]"
    );
    assert_eq!(value(&body), "[false, false, null, null]");
}

#[test]
fn file_sizes_and_json_files() {
    let directory = tempfile::tempdir().expect("a temporary directory");
    let dir = directory.path().display();
    let body = format!(
        "file('{dir}/s.json').text = '{{\"a\": {{\"b\": 12}}}}'\n\
         [java.nio.file.Files.size(file('{dir}/s.json')), \
         new groovy.json.JsonSlurper().parse(file('{dir}/s.json')).a.b]"
    );
    assert_eq!(value(&body), "[16, 12]");
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
fn yaml_anchors_aliases_and_merge_keys() {
    let text = "base: &base\n  a: 1\n  b: two\nmore: &more\n  <<: *base\n  c: 3\ns: &s hello\n\
                use:\n  x: 0\n  <<: [*more, {d: 4, a: 9}]\n  b: over\n  '<<': quoted\nref: *s\n";
    let body = format!("new org.yaml.snakeyaml.Yaml().load({text:?})");
    assert_eq!(
        value(&body),
        "[base:[a:1, b:two], more:[a:1, b:two, c:3], s:hello, \
         use:[x:0, a:1, b:over, c:3, d:4, <<:quoted], ref:hello]"
    );
}

#[test]
fn yaml_slurper_reads_decimals() {
    assert_eq!(
        value(
            "def y = new groovy.yaml.YamlSlurper().parseText('a: 2.50\\nn: 3\\nl: [x, 1.5]')\n\
             [y, y.a.getClass().getSimpleName(), y.n.getClass().getSimpleName()]"
        ),
        "[[a:2.5, n:3, l:[x, 1.5]], BigDecimal, Integer]"
    );
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
    assert_eq!(
        value(
            "[new URI('https', null, 'host', -1, '/p/', null, null), \
             new URI('s3', 'me', 'bucket', 9000, '/key', 'q=1', 'f'), \
             new URI('https://me@host/p').userInfo, new URI('https://host/p').userInfo]"
        ),
        "[https://host/p/, s3://me@bucket:9000/key?q=1#f, me, null]"
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

#[test]
fn regex_classes_and_file_walks() {
    let directory = tempfile::tempdir().expect("a temporary directory");
    for file in ["x.txt", "a/y.txt", "a/b/z.txt"] {
        let path = directory.path().join(file);
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(path, "").unwrap();
    }
    let d = directory.path().display();
    let body = format!(
        "def p = java.util.regex.Pattern.compile(\"(?:^|\\\\s)${{java.util.regex.Pattern.quote('--k.x')}}\\\\s+(\\\\S+)\")\n\
         def m = p.matcher('a --k.x 5 b')\n\
         def first = [m.find(), m.group(1), java.util.regex.Pattern.quote('a.b'), java.util.regex.Pattern.matches('a.c', 'abc')]\n\
         def files = []\nfile('{d}').toFile().eachFileRecurse(groovy.io.FileType.FILES) {{ f -> files << f.name }}\n\
         def dirs = []\nfile('{d}').eachFileRecurse(groovy.io.FileType.DIRECTORIES) {{ f -> dirs << f.name }}\n\
         def all = []\nfile('{d}').eachFile {{ f -> all << f.name }}\n\
         [first, files.sort(), dirs.sort(), all.sort()]"
    );
    assert_eq!(
        value(&body),
        "[[true, 5, \\Qa.b\\E, true], [x.txt, y.txt, z.txt], [a, b], [a, x.txt]]"
    );
}

/// A task input staged in a folder, like `input1/a.fastq.gz` of `stageAs: "input*/*"`, has the
/// name of its last part, as Java's `Path.getFileName` gives it.
#[test]
fn staged_names_in_folders() {
    let path = nextflow_evaluator::value::PathValue {
        path: "/data/a.fastq.gz".into(),
        staged: Some("input1/a.fastq.gz".into()),
        relative: None,
        prefix: None,
    };
    assert_eq!(path.name(), "a.fastq.gz");
    assert_eq!(path.base_name(), "a.fastq");
    assert_eq!(path.extension(), "gz");
}
