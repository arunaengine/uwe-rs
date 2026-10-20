//! Checks String methods against Nextflow 26.04.6 (Groovy spec, sections 8.6, 8.9 and 10.1).
// Copyright (c) 2026 The iunctus Contributors
// SPDX-License-Identifier: MIT or Apache-2.0

use std::rc::Rc;

use nextflow_evaluator::interpreter::{Host, Interpreter};
use nextflow_evaluator::lower::lower_module;
use nextflow_parser::parse_script;

struct Silent;

impl Host for Silent {
    fn print(&mut self, _: &str) {}
}

/// The value of an expression, as its Groovy text, or the error.
fn eval(expression: &str) -> Result<String, String> {
    let source = format!("def f() {{ {expression} }}");
    let parsed = parse_script(&source);
    assert!(parsed.errors.is_empty(), "{:?}", parsed.errors);
    let module = Rc::new(lower_module(&parsed.tree, &source).expect("lowers"));
    let mut host = Silent;
    let mut interpreter = Interpreter::new(&mut host);
    interpreter
        .call_function(&module, "f", Vec::new())
        .map(|value| value.to_string())
        .map_err(|error| error.to_string())
}

fn value(expression: &str) -> String {
    eval(expression).expect("evaluates")
}

#[test]
fn splitting_and_tokens() {
    assert_eq!(value("'a,b,,c'.tokenize(',')"), "[a, b, c]");
    assert_eq!(value("'  a  b '.tokenize()"), "[a, b]");
    assert_eq!(value("'x_y_z'.split('_')"), "[x, y, z]");
    assert_eq!(value("'a\\tb'.split('\\t').length"), "2");
    assert_eq!(value("'a.b.c'.split('.')"), "[]");
    assert_eq!(value("''.split(',')"), "[]");
    assert_eq!(value("'a,b,,'.split(',', -1)"), "[a, b, , ]");
    assert_eq!(value("' a  b\\tc '.split()"), "[a, b, c]");
    assert_eq!(
        value("' a '.split().getClass().getSimpleName()"),
        "String[]"
    );
    assert_eq!(value("'a\\nb\\r\\nc\\n'.readLines()"), "[a, b, c]");
    assert_eq!(value("'abc'.toList()"), "[a, b, c]");
}

#[test]
fn padding() {
    assert_eq!(
        value(
            "[\"7\".padLeft(3, '0'), 'ab'.padLeft(7, 'xyz'), 'ab'.padRight(5), 'abc'.padRight(2), \
             'ab'.center(5, '12'), 'ab'.center(6, '-'), 'a'.center(4)].join('|')"
        ),
        "007|xyzxyab|ab   |abc|1ab12|--ab--| a  "
    );
}

#[test]
fn concatenation() {
    assert_eq!(value("\"ab\".concat('.').concat('c')"), "ab.c");
}

#[test]
fn margins() {
    assert_eq!(
        value("'\\n  |one\\n    |  two\\n  three|\\n\\t|four\\n  |'.stripMargin().inspect()"),
        "'\\none\\n  two\\n  three|\\nfour\\n'"
    );
    assert_eq!(
        value(
            "['x\\n  #y\\r\\n#z'.stripMargin('#'), ''.stripMargin(), 'a\\n'.stripMargin()].inspect()"
        ),
        "['x\\ny\\nz', '', 'a\\n']"
    );
}

#[test]
fn bytes_and_digests() {
    assert_eq!(
        value(
            "['hi'.bytes, 'ü'.bytes, 'hi'.bytes.encodeBase64().toString(), \
             'abcd'.bytes.encodeBase64(), 'hi'.bytes.getClass().getName()]"
        ),
        "[[104, 105], [-61, -68], aGk=, YWJjZA==, [B]"
    );
    assert_eq!(
        value(
            "java.security.MessageDigest.getInstance('SHA-256').digest('S1'.bytes).encodeHex()\
             .toString().substring(0, 20).toUpperCase()"
        ),
        "3696AD59777E09D5F2DA"
    );
}

#[test]
fn formats() {
    assert_eq!(
        value(
            "String.format('%03d|%-5s|%5s|%.1f|%.3f|%x|%X|%,d|%+d|%08.2f|%%|%s|%b|%c', 7, 'ab', \
             'cd', 0.15, 2.0005d, 255, 255, 1234567, 5, -3.14159, null, 'x', 65)"
        ),
        "007|ab   |   cd|0.2|2.001|ff|FF|1,234,567|+5|-0003.14|%|null|true|A"
    );
    assert_eq!(value("String.format('%2$s %1$s %s', 'a', 'b')"), "b a a");
    assert_eq!(
        value("String.format('%.2f %.0f %.0f %f', 0.125, 2.5, -2.5, 1.5)"),
        "0.13 3 -3 1.500000"
    );
    assert_eq!(
        value("String.format('%s %s', [1, 2], [a: 1])"),
        "[1, 2] {a=1}"
    );
    assert_eq!(
        value("String.format(java.util.Locale.ROOT, '%.2f', 1.234G)"),
        "1.23"
    );
    assert_eq!(
        eval("String.format('%d', 'x')").unwrap_err(),
        "IllegalFormatConversionException: d != java.lang.String"
    );
}

#[test]
fn searching_and_slicing() {
    assert_eq!(value("'file.fastq.gz'.endsWith('.gz')"), "true");
    assert_eq!(value("'file.fastq.gz'.startsWith('file')"), "true");
    assert_eq!(value("'abc'.contains('bc')"), "true");
    assert_eq!(
        value("['abcabc'.indexOf('c'), 'abcabc'.lastIndexOf('c'), 'abcabc'.indexOf('c', 3)]"),
        "[2, 5, 5]"
    );
    assert_eq!(value("'abc'.indexOf('x')"), "-1");
    assert_eq!(
        value("['abc'.substring(1), 'abc'.substring(1, 2)]"),
        "[bc, b]"
    );
    let error = eval("'abc'.substring(5)").unwrap_err();
    assert_eq!(
        error,
        "StringIndexOutOfBoundsException: begin 5, end 3, length 3"
    );
    assert_eq!(
        value("['abc'.size(), 'abc'.length(), ''.empty, 'a'.empty]"),
        "[3, 3, true, false]"
    );
    assert_eq!(value("'aaa'.count('aa')"), "2");
    assert_eq!(value("'sample_R1_001.fastq.gz'.find(/_R\\d_/)"), "_R1_");
}

#[test]
fn replacing() {
    assert_eq!(
        value("'sample_R1.fq'.replaceAll(/_R(\\d)/, '-$1')"),
        "sample-1.fq"
    );
    assert_eq!(value("'a.b.c'.replace('.', '_')"), "a_b_c");
    assert_eq!(value("'aXbX'.replaceFirst('X', '-')"), "a-bX");
    assert_eq!(
        value("'a1b2'.replaceAll(/\\d/) { d -> \"<${d}>\" }"),
        "a<1>b<2>"
    );
    assert_eq!(
        value("'k=v'.replaceAll(/(\\w)=(\\w)/) { all, k, v -> v + k }"),
        "vk"
    );
    assert_eq!(value("'ab'.matches('a.')"), "true");
    assert_eq!(value("'ab'.matches('a')"), "false");
}

#[test]
fn conversions() {
    assert_eq!(value("' 42 '.toInteger()"), "42");
    assert_eq!(
        eval("'4.0'.toInteger()").unwrap_err(),
        "NumberFormatException: For input string: \"4.0\""
    );
    assert_eq!(
        value("['1.5'.toDouble(), '1e3'.toBigDecimal(), '2'.toLong()]"),
        "[1.5, 1E+3, 2]"
    );
    assert_eq!(
        value("['yes'.toBoolean(), ' Y '.toBoolean(), '1'.toBoolean()]"),
        "[false, true, true]"
    );
    let checks =
        "['NaN'.isDouble(), '1e3'.isDouble(), ' 42'.isInteger(), '99999999999'.isInteger()]";
    assert_eq!(value(checks), "[true, true, true, false]");
}

#[test]
fn transforms_string_values() {
    assert_eq!(value("'  a b \\t'.trim()"), "a b");
    assert_eq!(
        value("'MiXeD'.toLowerCase() + 'MiXeD'.toUpperCase()"),
        "mixedMIXED"
    );
    assert_eq!(value("'hello world'.capitalize()"), "Hello world");
    assert_eq!(value("'abc'.reverse()"), "cba");
    assert_eq!(value("'    a\\n  '.stripIndent() == '  a\\n'"), "true");
    assert_eq!(value("'  a  \\n  b'.stripIndent() == 'a\\nb'"), "true");
    assert_eq!(
        value("['a'.compareTo('c'), 'ab'.compareTo('a')]"),
        "[-2, 1]"
    );
    assert_eq!(
        value("['a'.equals('a'), 'a'.equals(\"${'a'}\"), 'a'.hashCode()]"),
        "[true, true, 97]"
    );
}

#[test]
fn escapes_inspected_strings() {
    assert_eq!(
        value("[[a: \"x\\ty\", 1: [2, null, true]].inspect(), \"it's\".inspect()]"),
        "[['a':'x\\ty', 1:[2, null, true]], 'it\\'s']"
    );
}

#[test]
fn simple_template_engine() {
    assert_eq!(
        value(
            "new groovy.text.SimpleTemplateEngine().createTemplate('Hi ${name}, $workflow.manifest.name v${x + 1} \\\\$HOME end.')\
             .make([name: 'you', workflow: [manifest: [name: 'demo']], x: 1]).toString()"
        ),
        "Hi you, demo v2 $HOME end."
    );
}

/// Code in a template, as nf-core's email templates have, which Groovy 4 renders as
/// `'Run yes and - 1,- 2 "quoted" x\n\n2'` with both engines.
#[test]
fn template_code() {
    let template = r#"'Run <% if (ok) { out << "yes" } else { out << """no""" } %> and <%= list.collect { "- $it" }.join(",") %> "quoted" $name\\n\n<% out << list.size() %>'"#;
    assert_eq!(
        value(&format!(
            "new groovy.text.GStringTemplateEngine().createTemplate({template})\
             .make([ok: true, list: [1, 2], name: 'x']).toString()"
        )),
        "Run yes and - 1,- 2 \"quoted\" x\n\n2"
    );
}

/// Characters from `as char` compute and compare with their codes, as in Nextflow 26.04.6.
#[test]
fn characters() {
    assert_eq!(
        value(
            "def w = 'B12'\ndef row = (w[0] as char) - ('A' as char) + 1\n\
             def c = ((('A' as char) as int) + 2) as char\n\
             [row, row.getClass().simpleName, '' + c, c.getClass().simpleName, \
             ('A' as char) == ('A' as char), ('A' as char) < ('B' as char), (('C' as char) as int)]"
        ),
        "[2, Integer, C, Character, true, true, 67]"
    );
}

/// Groovy's `md5()` and `sha256()` of text, as Nextflow 26.04.6 prints them.
#[test]
fn digests_of_text() {
    assert_eq!(value("'abc'.md5()"), "900150983cd24fb0d6963f7d28e17f72");
    assert_eq!(
        value("'a\\u00e9'.sha256()"),
        "561951c2b8c47984b8b4b8ae1f173a03d9c703f66cf36f145e27bc6145499f74"
    );
    assert_eq!(
        value("def m = [id: 1]; \"$m\".md5()"),
        "6b2b8a6ecb0c99efd1e6c0ebac6f90e5"
    );
}

/// `File.separator` and `File.pathSeparator` of Linux, as nf-core/eager reads them.
#[test]
fn file_separators() {
    assert_eq!(value("'/a/b'.lastIndexOf(File.separator)"), "2");
    assert_eq!(value("File.pathSeparator"), ":");
}

/// Statements in an interpolation run as a closure, whose last value goes into the string;
/// Nextflow 26.04.6 prints `v many end`.
#[test]
fn statements_in_interpolations() {
    assert_eq!(
        value(
            "def x = [1, 2]\n\"\"\"v ${\n  if (x.size() > 1) { 'many' } else { 'one' }\n} end\"\"\""
        ),
        "v many end"
    );
}

/// Groovy's `sprintf` and `stripIndent(true)`; Nextflow 26.04.6 gives `a-b` and `a\nb`.
#[test]
fn formats_string_arguments() {
    assert_eq!(
        value("[sprintf('%s-%s', 'a', 'b'), '  a\\n  b'.stripIndent(true)]"),
        "[a-b, a\nb]"
    );
}

/// More of Groovy's string methods, as Nextflow 26.04.6 prints them.
#[test]
fn translates_and_finds() {
    let cases = [
        ("'Abc'.uncapitalize()", "abc"),
        ("'hello'.tr('a-y', 'b-z')", "ifmmp"),
        ("'abc'.tr('abc', 'x')", "xxx"),
        ("'abc'.toCharArray().size()", "3"),
        ("'abca'.toSet().size()", "3"),
        ("'abc'.findAll(/[ab]/)", "[a, b]"),
        ("'a1b22'.findAll(/(\\d)(\\d)?/)", "[1, 22]"),
        ("'a1b2'.findAll(/\\d/) { it + 'x' }", "[1x, 2x]"),
        ("'abc'.codePointAt(0)", "97"),
        ("['abc'.getAt(0..1), 'abc'.getAt(1)]", "[ab, b]"),
        ("'abc'.contentEquals('abc')", "true"),
        ("'abc'.getBytes('UTF-8').size()", "3"),
    ];
    for (code, expected) in cases {
        assert_eq!(value(code), expected, "{code}");
    }
}

/// `StringBuilder` and `StringBuffer` change their text in place, as Nextflow 26.04.6 shows.
#[test]
fn builds_text() {
    let code = "def sb = new StringBuilder()\nsb.append('ab').append(1).append(true)\nsb << 'x'\n\
                def seen = [sb.toString(), sb.length(), \"${sb}\"]\nsb.insert(0, 'Z')\n\
                seen << sb.toString()\nseen << sb.reverse().toString()\nsb.setLength(2)\n\
                seen << sb.toString()\nsb.deleteCharAt(0)\nseen << sb.toString()\n\
                seen << sb.getClass().name\nseen";
    assert_eq!(
        value(code),
        "[ab1truex, 8, ab1truex, Zab1truex, xeurt1baZ, xe, e, java.lang.StringBuilder]"
    );
    let code = "def sb = new StringBuilder('init')\ndef seen = [sb.size(), sb == 'init']\n\
                sb.setCharAt(0, 'I' as char)\nseen << sb.toString()\nsb.replace(0, 1, 'XX')\n\
                seen << sb.toString()\nsb.delete(0, 2)\nseen << sb.toString()\n\
                seen << (sb + 'z')\nseen << new StringBuffer('q').append('r').toString()\n\
                seen << sb.indexOf('i') << sb.charAt(1) << sb.substring(1)\nseen";
    assert_eq!(
        value(code),
        "[4, false, Init, XXnit, nit, nitz, qr, 1, i, it]"
    );
}

/// Dates, times, instants and durations of `java.time`, and `SimpleDateFormat`, as Nextflow
/// 26.04.6 prints them.
#[test]
fn formats_java_times() {
    let pattern = |text: &str| format!("java.time.format.DateTimeFormatter.ofPattern('{text}')");
    let cases = [
        ("java.time.LocalDate.parse('2026-03-05')".to_owned(), "2026-03-05"),
        (format!("java.time.LocalDate.parse('2026-03-05').format({})", pattern("yyyyMMdd")), "20260305"),
        ("java.time.LocalDate.parse('2026-03-05').getYear()".into(), "2026"),
        ("java.time.LocalDate.parse('2026-03-05').plusDays(30)".into(), "2026-04-04"),
        ("java.time.LocalDate.parse('2026-03-05').getClass().name".into(), "java.time.LocalDate"),
        ("java.time.LocalDateTime.parse('2026-03-05T07:08:09')".into(), "2026-03-05T07:08:09"),
        (
            format!("java.time.LocalDateTime.parse('2026-03-05T07:08:09').format({})", pattern("yyyy-MM-dd_HH-mm-ss")),
            "2026-03-05_07-08-09",
        ),
        ("java.time.LocalDateTime.parse('2026-03-05T07:08:09').toLocalDate()".into(), "2026-03-05"),
        ("java.time.Instant.parse('2026-03-05T07:08:09Z')".into(), "2026-03-05T07:08:09Z"),
        ("java.time.Instant.parse('2026-03-05T07:08:09Z').toEpochMilli()".into(), "1772694489000"),
        ("java.time.Instant.ofEpochMilli(1000)".into(), "1970-01-01T00:00:01Z"),
        ("java.time.Duration.ofSeconds(3725)".into(), "PT1H2M5S"),
        ("java.time.Duration.ofSeconds(3725).toMinutes()".into(), "62"),
        (
            "java.time.Duration.between(java.time.Instant.parse('2026-03-05T07:08:09Z'), \
             java.time.Instant.parse('2026-03-05T08:08:10Z'))"
                .into(),
            "PT1H1S",
        ),
        ("new java.text.SimpleDateFormat('yyyy-MM-dd').format(new Date(0)).size()".into(), "10"),
        ("java.time.LocalDate.now().getClass().name".into(), "java.time.LocalDate"),
        (
            "java.time.format.DateTimeFormatter.ISO_LOCAL_DATE.format(java.time.LocalDate.parse('2026-03-05'))".into(),
            "2026-03-05",
        ),
        ("java.time.LocalDate.of(2026, 1, 2)".into(), "2026-01-02"),
        ("java.time.LocalDateTime.parse('2026-03-05T07:08:09').getHour()".into(), "7"),
        (
            "java.time.LocalDate.parse('2026-03-05').isBefore(java.time.LocalDate.parse('2026-04-01'))".into(),
            "true",
        ),
        ("java.time.LocalDateTime.parse('2026-03-05T07:08')".into(), "2026-03-05T07:08"),
        ("java.time.LocalDateTime.of(2026, 3, 5, 7, 8, 0)".into(), "2026-03-05T07:08"),
    ];
    for (code, expected) in cases {
        assert_eq!(value(&code), expected, "{code}");
    }
}
