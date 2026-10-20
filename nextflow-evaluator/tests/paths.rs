//! Checks `file()` and path methods against Nextflow 26.04.6 (Groovy spec, sections 6.4 and 10.7).
// Copyright (c) 2026 The iunctus Contributors
// SPDX-License-Identifier: MIT or Apache-2.0

use std::rc::Rc;

use nextflow_evaluator::exception::Exception;
use nextflow_evaluator::interpreter::glob::{GlobOptions, glob};
use nextflow_evaluator::interpreter::{Error, Host, Interpreter};
use nextflow_evaluator::lower::lower_module;
use nextflow_evaluator::value::PathValue;
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
fn formats_remote_paths() {
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
fn preserves_relative_files() {
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
fn resolves_local_paths() {
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
fn mutates_host_files() {
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

/// `withWriter`, `length()` and `bytes` of a `java.io.File`, as nf-core's old completion
/// handlers write reports and read the logo; Groovy 4 gives
/// `[done, 'abc\n', 4, YWJjCg==, YW|Jj|Cg|==]`.
#[test]
fn writes_file_bytes() {
    let directory = tempfile::tempdir().expect("a temporary directory");
    let dir = directory.path().display();
    let body = format!(
        "def f = new File('{dir}/w.txt')\n\
         def r = f.withWriter {{ w -> w << 'a' << 'b'; w.println('c'); 'done' }}\n\
         [r, f.text.inspect(), f.length(), f.bytes.encodeBase64().toString(), \
         f.bytes.encodeBase64().toString().tokenize('\\n')*.toList()*.collate(2)*.collect {{ it.join() }}\
         .flatten().join('|')]"
    );
    assert_eq!(value(&body), "[done, 'abc\\n', 4, YWJjCg==, YW|Jj|Cg|==]");
}

#[test]
fn copies_directory_files() {
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

/// `path.matches(glob)` matches the whole path, as in nf-core/eager's `r1.matches('NA')`;
/// Nextflow 26.04.6 gives `[false, true, false, true, false, true, true]`.
#[test]
fn paths_match_globs() {
    assert_eq!(
        value(
            "def f = file('/tmp/w.txt')\ndef h = file('https://example.com/data/a.fq.gz')\n\
             [f.matches('NA'), f.matches('/tmp/**'), f.matches('*.txt'), f.matches('**.txt'), \
             h.matches('NA'), h.matches('/data/*.gz'), h.matches('**.gz')]"
        ),
        "[false, true, false, true, false, true, true]"
    );
}

#[test]
fn converts_file_paths() {
    assert_eq!(
        value("def f = file('/x/y/z.txt').fileName\n[f.name, f, f.getClass().getSimpleName()]"),
        "[z.txt, z.txt, UnixPath]"
    );
}

#[test]
fn splits_csv_files() {
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
fn reads_stream_bytes() {
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
fn rejects_file_children() {
    let directory = tempfile::tempdir().expect("a temporary directory");
    let dir = directory.path().display();
    let body = format!(
        "file('{dir}/a').text = 'x'\n[file('{dir}/a/b').exists(), file('{dir}/a/b').isDirectory(), \
         file('{dir}/a').listFiles(), file('{dir}/c').listFiles()]"
    );
    assert_eq!(value(&body), "[false, false, null, null]");
}

#[test]
fn reads_file_metadata() {
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
fn matches_java_globs() {
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
fn propagates_glob_failures() {
    struct FailingListing;

    impl Host for FailingListing {
        fn print(&mut self, _: &str) {}

        fn list(&mut self, directory: &PathValue) -> Result<Vec<(String, bool)>, Error> {
            if directory.path == "/bucket/denied" {
                return Err(Error::Thrown {
                    exception: Rc::new(Exception::new("IOException", "listing denied")),
                    span: Default::default(),
                });
            }
            Ok(vec![("root.fq".into(), false), ("denied".into(), true)])
        }
    }

    let error = glob(
        &mut FailingListing,
        "s3://bucket/**.fq",
        &GlobOptions::default(),
    )
    .unwrap_err();
    assert_eq!(error.to_string(), "IOException: listing denied");
}

#[test]
fn excludes_absent_prefixes() {
    let directory = tempfile::tempdir().expect("a temporary directory");
    let pattern = format!("{}/absent/*.fq", directory.path().display());
    assert!(
        glob(&mut Launch, &pattern, &GlobOptions::default())
            .unwrap()
            .is_empty()
    );
}

#[test]
fn matches_output_globs() {
    use nextflow_evaluator::interpreter::glob::matches;
    assert!(matches("*.{mzML,mgf}", "run.mgf"));
    assert!(matches("out_[ab].txt", "out_b.txt"));
    assert!(!matches("*.txt", "sub/a.txt"));
    assert!(matches("**.txt", "sub/a.txt"));
}

/// `new Yaml().load(new FileReader(name))` reads the file, as TreeVal reads its input YAML.
#[test]
fn reads_yaml_files() {
    let directory = tempfile::tempdir().expect("a temporary directory");
    let dir = directory.path().display();
    let body = format!(
        "def f = new File('{dir}/in.yaml')\n\
         f.text = 'a: 1\\nb: [x, y]\\n'\n\
         new org.yaml.snakeyaml.Yaml().load(new FileReader(f.toString()))"
    );
    assert_eq!(value(&body), "[a:1, b:[x, y]]");
}

#[test]
fn resolves_yaml_aliases() {
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
fn reads_yaml_decimals() {
    assert_eq!(
        value(
            "def y = new groovy.yaml.YamlSlurper().parseText('a: 2.50\\nn: 3\\nl: [x, 1.5]')\n\
             [y, y.a.getClass().getSimpleName(), y.n.getClass().getSimpleName()]"
        ),
        "[[a:2.5, n:3, l:[x, 1.5]], BigDecimal, Integer]"
    );
}

#[test]
fn preserves_yaml_values() {
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
fn formats_nested_yaml() {
    assert_eq!(
        value(
            "new org.yaml.snakeyaml.Yaml().dumpAsMap([versions: [[process: 'P', tools: [samtools: '1.20']], [process: 'Q', tools: [bwa: '0.7.17']]]])"
        ),
        "versions:\n- process: P\n  tools:\n    samtools: '1.20'\n- process: Q\n  tools:\n    bwa: 0.7.17\n"
    );
    assert_eq!(
        value("new org.yaml.snakeyaml.Yaml().dumpAsMap([nested: [[1, 2], [[:], []]]])"),
        "nested:\n- - 1\n  - 2\n- - {}\n  - []\n"
    );
}

#[test]
fn preserves_yaml_text() {
    for (text, expected) in [
        ("first\nsecond", "text: |-\n  first\n  second\n"),
        ("first\nsecond\n", "text: |\n  first\n  second\n"),
        ("first\nsecond\n\n", "text: |+\n  first\n  second\n\n"),
        ("  first\nsecond", "text: |2-\n    first\n  second\n"),
        ("\n\n", "text: |2+\n\n\n"),
    ] {
        let body = format!(
            "def y = new org.yaml.snakeyaml.Yaml()\ndef v = [text: {text:?}]\ndef dumped = y.dumpAsMap(v)\nassert y.load(dumped) == v\ndumped"
        );
        assert_eq!(value(&body), expected);
    }
    assert_eq!(
        value(
            "def y = new org.yaml.snakeyaml.Yaml()\ndef v = [items: ['first\\nsecond', [:], []]]\ndef dumped = y.dumpAsMap(v)\nassert y.load(dumped) == v\ndumped"
        ),
        "items:\n- |-\n  first\n  second\n- {}\n- []\n"
    );
}

#[test]
fn parses_uri_parts() {
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
fn reads_compressed_streams() {
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
fn dispatches_class_methods() {
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

/// A task input staged in a folder, like `input1/a.fastq.gz` of `stageAs: "input*/*"`, has its
/// staged name, and the base name, simple name and extension of its last part: Nextflow
/// 26.04.6 gives `input1/a.fastq.gz a.fastq a gz`.
#[test]
fn preserves_staged_names() {
    let path = nextflow_evaluator::value::PathValue {
        path: "/data/a.fastq.gz".into(),
        staged: Some("input1/a.fastq.gz".into()),
        relative: None,
        prefix: None,
    };
    assert_eq!(path.name(), "input1/a.fastq.gz");
    assert_eq!(path.base_name(), "a.fastq");
    assert_eq!(path.simple_name(), "a");
    assert_eq!(path.extension(), "gz");
}

/// Legacy syntax: `try (resource = ...) { ... }` declares the resource for its body, as
/// pgsc_calc's `count_scores` does; Nextflow 26.04.6 gives `a`.
#[test]
fn try_with_resources() {
    let directory = tempfile::tempdir().expect("a temporary directory");
    let dir = directory.path().display();
    let body = format!(
        "def f = new File('{dir}/r.txt')\n\
         f.text = 'a\\nb'\n\
         def line = null\n\
         try (def r = new BufferedReader(new FileReader(f.toString()))) {{\n    line = r.readLine()\n}}\n\
         line"
    );
    assert_eq!(value(&body), "a");
}

/// `getAbsolutePath()` of a `java.io.File` gives the text of its absolute path, as nf-synteny
/// uses it; Groovy gives `/tmp/a.fa` and the launch directory with `a.fa` for these.
#[test]
fn resolves_absolute_files() {
    let body = "[new File('/tmp/a.fa').getAbsolutePath(), new File('a.fa').getAbsolutePath().endsWith('/a.fa'), \
                new File('a.fa').getAbsolutePath() instanceof String]";
    assert_eq!(value(body), "[/tmp/a.fa, true, true]");
}

/// `GZIPInputStream` reads the header first, as Java does: an empty file is the end of the
/// stream, and other bytes are no gzip; Nextflow 26.04.6 gives `[java.io.EOFException,
/// java.util.zip.ZipException: Not in GZIP format]`, and `catch (IOException e)` takes both.
#[test]
fn gzip_headers() {
    let directory = tempfile::tempdir().expect("a temporary directory");
    let dir = directory.path().display();
    let body = format!(
        "new File('{dir}/e.gz').text = ''\n\
         new File('{dir}/t.gz').text = 'abc'\n\
         ['e.gz', 't.gz'].collect {{ n ->\n\
             try {{ new java.util.zip.GZIPInputStream(file(\"{dir}/\" + n).newInputStream()); 'ok' }}\n\
             catch (IOException e) {{ e.toString() }}\n\
         }}"
    );
    assert_eq!(
        value(&body),
        "[java.io.EOFException, java.util.zip.ZipException: Not in GZIP format]"
    );
}

/// The default operator scratch stays while its thread runs and is removed when it ends.
#[test]
fn removes_operator_scratch() {
    let folder = std::thread::spawn(|| {
        let folder = Launch.operator_directory().expect("reserves scratch");
        let folder = std::path::PathBuf::from(&folder.path);
        std::fs::write(folder.join("chunk.txt"), "a\n").expect("writes a chunk");
        assert!(folder.is_dir());
        folder
    })
    .join()
    .expect("the thread ends");
    assert!(!folder.exists(), "{}", folder.display());
}
