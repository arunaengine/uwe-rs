//! Runs small workflows with factories and operators and checks what `view` prints against
//! the experiments of the channels spec.
// Copyright (c) 2026 The iunctus Contributors
// SPDX-License-Identifier: MIT or Apache-2.0

use std::rc::Rc;

use nextflow_evaluator::interpreter::Host;
use nextflow_evaluator::lower::lower_module;
use nextflow_parser::parse_script;

#[derive(Default)]
struct Output(String);

impl Host for Output {
    fn print(&mut self, text: &str) {
        self.0.push_str(text);
    }
}

/// What the entry workflow prints, or the error.
fn run(body: &str) -> Result<String, String> {
    let source = format!("workflow {{\n{body}\n}}");
    let parsed = parse_script(&source);
    assert!(parsed.errors.is_empty(), "{:?}", parsed.errors);
    let module = Rc::new(lower_module(&parsed.tree, &source).expect("lowers"));
    let mut output = Output::default();
    nextflow_runtime::run(&module, &mut output).map_err(|error| error.to_string())?;
    Ok(output.0)
}

fn lines(body: &str) -> Vec<String> {
    run(body).expect("runs").lines().map(String::from).collect()
}

#[test]
fn factories() {
    assert_eq!(lines("channel.of(1, 2, 3).view()"), ["1", "2", "3"]);
    assert_eq!(
        lines("channel.of(1..3, [4, 5]).view()"),
        ["1", "2", "3", "[4, 5]"]
    );
    assert_eq!(lines("channel.fromList([1, 2]).view()"), ["1", "2"]);
    assert_eq!(
        lines("channel.of('a,b'.split(',')).view()\nchannel.of(['c', 'd']).view()"),
        ["a", "b", "[c, d]"]
    );
    assert_eq!(lines("channel.value(5).view()"), ["5"]);
    assert_eq!(lines("channel.empty().view()"), Vec::<String>::new());
    let error = run("channel.of(channel.of(1))").unwrap_err();
    assert_eq!(
        error,
        "IllegalArgumentException: Argument of `channel.of` cannot be a channel object"
    );
}

/// A missing file of `fromPath` with `checkIfExists` fails when the factory starts, after
/// the workflow body ran, as in Nextflow 26.04.6.
#[test]
fn missing_files_fail_when_the_factory_starts() {
    let source = "workflow {\n    channel.fromPath('/nonexistent/x.fa', checkIfExists: true)\n    \
                  println('after')\n}";
    let parsed = parse_script(source);
    let module = Rc::new(lower_module(&parsed.tree, source).expect("lowers"));
    let mut output = Output::default();
    let error = nextflow_runtime::run(&module, &mut output)
        .unwrap_err()
        .to_string();
    assert!(error.contains("/nonexistent/x.fa"), "{error}");
    assert_eq!(output.0, "after\n");
    let printed = run("channel.fromPath('/nonexistent/x.fa')\nprintln('after')").unwrap();
    assert_eq!(printed, "after\n");
    // A null pattern fails at once, before the statements after it.
    let error = run("channel.fromPath(null, checkIfExists: true)\nprintln('after')").unwrap_err();
    assert_eq!(
        error,
        "AbortOperationException: Missing `fromPath` parameter"
    );
}

/// `buffer` with sizes, skips and criteria, as Nextflow 26.04.6 gives them.
#[test]
fn buffers() {
    let buffered = |call: &str| lines(&format!("channel.of(1..7).{call}.view()"));
    assert_eq!(buffered("buffer(size: 2)"), ["[1, 2]", "[3, 4]", "[5, 6]"]);
    assert_eq!(
        buffered("buffer(size: 2, remainder: true)"),
        ["[1, 2]", "[3, 4]", "[5, 6]", "[7]"]
    );
    assert_eq!(buffered("buffer(size: 2, skip: 1)"), ["[2, 3]", "[5, 6]"]);
    assert_eq!(
        buffered("buffer { it % 3 == 0 }"),
        ["[1, 2, 3]", "[4, 5, 6]"]
    );
    assert_eq!(buffered("buffer(2, 4)"), ["[2, 3, 4]"]);
    assert_eq!(
        lines("channel.of(1..3).buffer(size: Integer.MAX_VALUE, remainder: true).view()"),
        ["[1, 2, 3]"]
    );
}

/// A property of a value channel is a value channel of the property, as in Nextflow 26.04.6.
#[test]
fn properties_of_value_channels() {
    assert_eq!(
        lines(
            "channel.of(file('/a/WARNING_x.txt')).first().baseName.tokenize('_').flatten().first()\
             .view { \"view: $it\" }"
        ),
        ["view: WARNING"]
    );
}

/// `toInteger` and the other cast operators, as in Nextflow 26.04.6.
#[test]
fn casts_of_values() {
    assert_eq!(
        lines(
            "channel.from('1000000,2000000').splitCsv().flatten().toInteger()\
             .view { \"${it} ${it.getClass().simpleName}\" }"
        ),
        ["1000000 Integer", "2000000 Integer"]
    );
    assert_eq!(lines("channel.of('2.5').toDouble().view()"), ["2.5"]);
}

/// Legacy `exit 1, "message"` ends the run with the message, as Nextflow's `exit` logs it.
#[test]
fn exit_ends_the_run() {
    assert_eq!(
        run("exit 1, 'bad samplesheet'").unwrap_err(),
        "AbortOperationException: bad samplesheet"
    );
    // `System.exit(1)` ends the run with its code too, as in the help checks of some pipelines.
    assert_eq!(
        run("System.exit(1)").unwrap_err(),
        "AbortOperationException: the run exits with code 1"
    );
}

#[test]
fn values_one_at_a_time() {
    assert_eq!(
        lines("channel.of(1, 2, 3).map { v -> v * 2 }.view()"),
        ["2", "4", "6"]
    );
    assert_eq!(
        lines("channel.of(1, 2, 3).map { v -> v == 2 ? null : v }.view()"),
        ["1", "3"]
    );
    assert_eq!(
        lines("channel.of(1, 2, 3).filter { v -> v > 1 }.view()"),
        ["2", "3"]
    );
    assert_eq!(
        lines("channel.of('a1', 'b', 'c2').filter(~/.\\d/).view()"),
        ["a1", "c2"]
    );
}

#[test]
fn flattening() {
    assert_eq!(
        lines("channel.of([a: 1, b: 2]).flatMap { v -> v }.view()"),
        ["a=1", "b=2"]
    );
    assert_eq!(
        lines("channel.of(1, null, 2).flatMap { v -> v }.view()"),
        ["1", "2"]
    );
    assert_eq!(
        lines("channel.of([1, [2, [3]]], [a: 1]).flatten().view()"),
        ["1", "2", "3", "[a:1]"]
    );
    assert_eq!(lines("channel.of(1, null, 2).flatten().view()"), ["1", "2"]);
}

#[test]
fn gathering() {
    assert_eq!(lines("channel.of(1, 2, 3).collect().view()"), ["[1, 2, 3]"]);
    assert_eq!(
        lines("channel.of([1, [2, 3]], [4]).collect().view()"),
        ["[1, [2, 3], 4]"]
    );
    assert_eq!(
        lines("channel.of(1, 2, 3).collect { v -> [v, [v]] }.view()"),
        ["[1, [1], 2, [2], 3, [3]]"]
    );
    assert_eq!(
        lines("channel.empty().collect().view()"),
        Vec::<String>::new()
    );
    assert_eq!(
        lines("channel.empty().collect().ifEmpty('DEF').view()"),
        ["DEF"]
    );
    assert_eq!(lines("channel.empty().toList().view()"), ["[]"]);
    assert_eq!(lines("channel.value(3).toList().view()"), ["[3]"]);
    assert_eq!(lines("channel.of(4, 5).first().view()"), ["4"]);
    assert_eq!(
        lines("channel.of(1, 2).mix(channel.of(3)).toList().view()"),
        ["[1, 2, 3]"]
    );
}

#[test]
fn pipes_and_waiting() {
    assert_eq!(
        lines("channel.of(1, 2) | map { v -> v * 10 } | view"),
        ["10", "20"]
    );
    let error = run("channel.of(1, 2, 3).first { v -> v > 10 }.view()").unwrap_err();
    let message =
        "The run cannot finish, because these steps wait for input that never comes: view";
    assert_eq!(error, format!("IllegalStateException: {message}"));
}

#[test]
fn tap_in_a_chain_assigns_the_channel() {
    let mut printed = lines(
        "channel.of(1, 2).map { v -> v * 2 }.tap { doubled }.map { v -> v + 1 }.view()\n\
         doubled.view { v -> \"d${v}\" }",
    );
    printed.sort();
    assert_eq!(printed, ["3", "5", "d2", "d4"]);
}

fn sorted(body: &str) -> Vec<String> {
    let mut lines = lines(body);
    lines.sort();
    lines
}

#[test]
fn group_tuple() {
    let source = "channel.of(['a', 1], ['b', 2], ['a', 3], ['b', 4], ['c', 5])";
    assert_eq!(
        lines(&format!("{source}.groupTuple().view()")),
        ["[a, [1, 3]]", "[b, [2, 4]]", "[c, [5]]"]
    );
    assert_eq!(
        sorted(&format!("{source}.groupTuple(size: 2).view()")),
        ["[a, [1, 3]]", "[b, [2, 4]]"]
    );
    assert_eq!(
        sorted(&format!(
            "{source}.groupTuple(size: 2, remainder: true).view()"
        )),
        ["[a, [1, 3]]", "[b, [2, 4]]", "[c, [5]]"]
    );
}

#[test]
fn join_combine_and_cross() {
    let left = "channel.of(['a', 1], ['b', 2], ['a', 3], ['c', 4])";
    let right = "channel.of(['a', 'x'], ['a', 'y'], ['b', 'z'], ['d', 'w'])";
    assert_eq!(
        sorted(&format!("{left}.join({right}).view()")),
        ["[a, 1, x]", "[a, 3, y]", "[b, 2, z]"]
    );
    assert_eq!(
        sorted(&format!("{left}.join({right}, remainder: true).view()")),
        [
            "[a, 1, x]",
            "[a, 3, y]",
            "[b, 2, z]",
            "[c, 4, null]",
            "[d, null, w]"
        ]
    );
    // A side that gave a value of only its key gets no `null` in the remainder; Nextflow
    // 26.04.6 prints these.
    assert_eq!(
        sorted("channel.of(['a'], ['b']).join(channel.of([null, null]), remainder: true).view()"),
        ["[a, null]", "[b, null]", "[null, null]"]
    );
    assert_eq!(
        sorted(
            "channel.of(['c', 0], ['a', 1], ['d']).join(channel.of(['b', 9], ['a', 2], \
             ['e', 5, 6]), remainder: true).view()"
        ),
        [
            "[a, 1, 2]",
            "[b, 9]",
            "[c, 0, null]",
            "[d, null]",
            "[e, 5, 6]"
        ]
    );
    assert_eq!(
        sorted("channel.of('a', 'b').join(channel.of('b', 'c'), remainder: true).view()"),
        ["a", "b", "c"]
    );
    assert_eq!(
        sorted("channel.of(['a', 1], ['b', 2]).combine(channel.of('x', 'y')).view()"),
        ["[a, 1, x]", "[a, 1, y]", "[b, 2, x]", "[b, 2, y]"]
    );
    assert_eq!(
        sorted(
            "channel.of(['a', 1], ['a', 2], ['b', 3]).combine(channel.of(['a', 'x'], ['b', 'y']), by: 0).view()"
        ),
        ["[a, 1, x]", "[a, 2, x]", "[b, 3, y]"]
    );
    assert_eq!(
        lines("channel.of([1, [2, 3]]).combine([4, [5]]).view()"),
        ["[1, [2, 3], 4]", "[1, [2, 3], 5]"]
    );
    assert_eq!(
        sorted(
            "channel.of(['a', 1], ['b', 2]).cross(channel.of(['a', 'x'], ['a', 'y'], ['b', 'z'])).view()"
        ),
        ["[[a, 1], [a, x]]", "[[a, 1], [a, y]]", "[[b, 2], [b, z]]"]
    );
}

#[test]
fn branch_multi_map_concat_and_merge() {
    assert_eq!(
        lines(
            "def result = channel.of(1, 2, 3, 4).branch { v ->\n    small: v < 2\n    big: v > 2\n        return v * 10\n}\n\
             result.small.view { v -> \"small ${v}\" }\nresult.big.view { v -> \"big ${v}\" }"
        ),
        ["small 1", "big 30", "big 40"]
    );
    assert_eq!(
        sorted(
            "channel.of(1, 2).multiMap { v ->\n    a: v\n    b: c: v * 10\n}.set { out }\nout.a.view { v -> \"a${v}\" }\nout.c.view { v -> \"c${v}\" }"
        ),
        ["a1", "a2", "c10", "c20"]
    );
    assert_eq!(
        lines("channel.value(1).concat(channel.of(2, 3)).view()"),
        ["1", "2", "3"]
    );
    assert_eq!(
        lines("channel.of(1, 2, 3).merge(channel.of('a', 'b')).view()"),
        ["[1, a]", "[2, b]"]
    );
}

#[test]
fn filtering_and_reducing_operators() {
    let source = "channel.of(1, 1, 2, 2, 1)";
    assert_eq!(lines(&format!("{source}.unique().view()")), ["1", "2"]);
    assert_eq!(
        lines(&format!("{source}.distinct().view()")),
        ["1", "2", "1"]
    );
    assert_eq!(lines(&format!("{source}.take(2).view()")), ["1", "1"]);
    assert_eq!(
        lines("channel.of(1, 2, 3).until { v -> v == 2 }.view()"),
        ["1"]
    );
    assert_eq!(
        lines("channel.of(['a', [1, 2, 3], ['x', 'y']]).transpose().view()"),
        ["[a, 1, x]", "[a, 2, y]"]
    );
    assert_eq!(
        lines("channel.of(['a', [1, 2, 3], ['x', 'y']]).transpose(remainder: true).view()"),
        ["[a, 1, x]", "[a, 2, y]", "[a, 3, null]"]
    );
    assert_eq!(
        lines(
            "channel.of(3, 1, 2).toSortedList().view()\nchannel.of(3, 1, 2).count().view()\n\
             channel.of(3, 1, 2).sum().view()\nchannel.of(3, 1, 2).min().view()\n\
             channel.of(3, 1, 2).max { v -> -v }.view()\nchannel.of(1, 2, 3).reduce { a, b -> b == 2 ? null : a + b }.view()\n\
             channel.of(1, 2, 3).last().view()"
        ),
        ["[1, 2, 3]", "3", "6", "1", "1", "4", "3"]
    );
}

#[test]
fn from_and_from_path() {
    assert_eq!(lines("channel.from(1, 2).view()"), ["1", "2"]);
    assert_eq!(lines("channel.from([3, 4]).view()"), ["3", "4"]);
    let directory = tempfile::tempdir().expect("a temporary directory");
    for name in ["b_1.fq", "a_1.fq", "a_2.fq"] {
        std::fs::write(directory.path().join(name), "").expect("writes a file");
    }
    let d = directory.path().display();
    assert_eq!(
        lines(&format!(
            "channel.fromPath('{d}/*_1.fq').map {{ f -> f.name }}.view()"
        )),
        ["a_1.fq", "b_1.fq"]
    );
    assert_eq!(
        lines(&format!(
            "channel.fromPath(['{d}/a_2.fq', '{d}/none.fq']).map {{ f -> f.name }}.view()"
        )),
        ["a_2.fq", "none.fq"]
    );
    let error = run(&format!(
        "channel.fromPath('{d}/*.txt', checkIfExists: true).view()"
    ))
    .unwrap_err();
    assert!(
        error.ends_with(&format!("No files match pattern `{d}/*.txt`")),
        "{error}"
    );
}

/// `fromFilePairs` groups the files by the start of their names; Nextflow 26.04.6 gives the
/// same tuples for these files.
#[test]
fn from_file_pairs() {
    let directory = tempfile::tempdir().expect("a temporary directory");
    for name in ["s_a_R1.fq", "s_a_R2.fq", "b_1.fq", "b_2.fq", "c_1.fq"] {
        std::fs::write(directory.path().join(name), "").expect("writes a file");
    }
    let d = directory.path().display();
    let names = "map { k, fs -> [k, fs*.name] }";
    let sorted = "toSortedList { a, b -> a[0] <=> b[0] }";
    assert_eq!(
        lines(&format!(
            "channel.fromFilePairs('{d}/*_{{1,2}}.fq').{names}.view()\n\
             channel.fromFilePairs('{d}/*_R{{1,2}}.fq', flat: true).map {{ it.collect {{ v -> v instanceof Path ? v.name : v }} }}.view()\n\
             channel.fromFilePairs('{d}/*.fq', size: 1).{names}.{sorted}.view()\n\
             channel.fromFilePairs('{d}/*_{{1,2}}.fq', size: -1).{names}.{sorted}.view()"
        )),
        [
            "[b, [b_1.fq, b_2.fq]]",
            "[s_a, s_a_R1.fq, s_a_R2.fq]",
            "[[b_1, [b_1.fq]], [b_2, [b_2.fq]], [c_1, [c_1.fq]], [s_a_R1, [s_a_R1.fq]], [s_a_R2, [s_a_R2.fq]]]",
            "[[b, [b_1.fq, b_2.fq]], [c, [c_1.fq]]]",
        ]
    );
}

#[test]
fn collect_file_writes_files_at_the_end() {
    let directory = tempfile::tempdir().expect("a temporary directory");
    let d = directory.path().display();
    assert_eq!(
        run(&format!(
            "channel.of('b', 'a').collectFile(name: 'x.txt', newLine: true, storeDir: '{d}')\n\
             .map {{ f -> f.text }}.view()"
        ))
        .unwrap(),
        "a\nb\n\n"
    );
    assert_eq!(
        lines(&format!(
            "channel.of(['y.txt', 'one'], ['x.txt', 'two']).collectFile(storeDir: '{d}').map {{ f -> f.name }}.view()"
        )),
        ["x.txt", "y.txt"]
    );
}

/// Legacy syntax: a Java lambda with parameters in parentheses is a closure, as in the
/// `.map((sample_info, path) -> ...)` of an RNA sequencing workflow; Nextflow 26.04.6 prints
/// `[2, 1]`.
#[test]
fn lambdas_with_parameters() {
    assert_eq!(
        lines("channel.of([1, 2]).map((a, b) -> [b, a]).view()"),
        ["[2, 1]"]
    );
}

/// A local closure with the name of an operator is called, as nf-core/references calls its
/// `def reduce = { ... }`; Nextflow 24.10.1, the release it names, runs it.
#[test]
fn local_closures_with_operator_names() {
    assert_eq!(
        lines("def reduce = { m -> m + 1 }\nchannel.of(1).map { reduce(it) }.view()"),
        ["2"]
    );
}

/// The records of a large CSV text go out in batches, in order, with their tuple.
#[test]
fn large_csv_texts_split_in_order() {
    assert_eq!(
        lines(
            "channel.of(['k', 'n\\n' + (1..25000).join('\\n')])\
             .splitCsv(header: true, elem: 1)\
             .map { k, row -> row.n.toInteger() }\
             .toList()\
             .map { l -> [l.size(), l == (1..25000).toList()] }\
             .view()"
        ),
        ["[25000, true]"]
    );
}

#[test]
fn splitters() {
    // Quotes group text only with the `quote` option.
    assert_eq!(
        lines(
            "channel.of('1,\"x,y\"').splitCsv().view()\n\
             channel.of('1,\"x,y\"').splitCsv(quote: '\"').view()"
        ),
        ["[1, \"x, y\"]", "[1, x,y]"]
    );
    assert_eq!(
        lines("channel.of(\"a,b\\n1,2\\n3,4\\n\").splitCsv(header: true).view()"),
        ["[a:1, b:2]", "[a:3, b:4]"]
    );
    assert_eq!(
        lines("channel.of(\"a,b\\n1,2\\n3,4\\n\").splitCsv(by: 2).view()"),
        ["[[a, b], [1, 2]]", "[[3, 4]]"]
    );
    assert_eq!(
        lines("channel.of(\"x\\ny\\nz\\n\").splitText(by: 2).view { v -> v.inspect() }"),
        ["'x\\ny\\n'", "'z\\n'"]
    );
    assert_eq!(
        lines("channel.of('>a\\nAC\\n>b\\nGT\\n').splitFasta().view { v -> v.inspect() }"),
        ["'>a\\nAC\\n'", "'>b\\nGT\\n'"]
    );
    assert_eq!(
        lines("channel.of(['id', \"a;b\\n1;2\\n\"]).splitCsv(elem: 1, sep: ';').view()"),
        ["[id, [a, b]]", "[id, [1, 2]]"]
    );
    assert_eq!(
        lines(
            "channel.of(1, 2).subscribe(onNext: { v -> println \"n${v}\" }, onComplete: { println 'done' })"
        ),
        ["n1", "n2", "done"]
    );
    // Up to Nextflow 25.10, `subscribe` gives its source, which older pipelines use.
    let mut printed = lines(
        "def ch = channel.of(1, 2).subscribe { v -> println \"s${v}\" }\n\
         ch.toList().view()",
    );
    printed.sort();
    assert_eq!(printed, ["[1, 2]", "s1", "s2"]);
}

#[test]
fn split_json_emits_elements_and_entries() {
    // Nextflow 26.04.6 prints these.
    let document =
        r#"{"a": {"x": 1, "y": [1, -2, 1.5, 1e3, 12345678901]}, "b": [ {"k": "v"}, 7 ]}"#;
    assert_eq!(
        lines(&format!(
            "channel.of('{document}').splitJson(path: 'a').view {{ v -> v.inspect() }}"
        )),
        [
            "['key':'x', 'value':1]",
            "['key':'y', 'value':[1, '-2', 1.5, '1e3', 12345678901]]"
        ]
    );
    assert_eq!(
        lines(&format!(
            "channel.of('{document}').splitJson(path: 'a.y').view {{ v -> v.inspect() }}"
        )),
        ["1", "'-2'", "1.5", "'1e3'", "12345678901"]
    );
    assert_eq!(
        lines(&format!(
            "channel.of('{document}').splitJson(path: 'b[0]').view()"
        )),
        ["[key:k, value:v]"]
    );
    assert!(
        lines(&format!(
            "channel.of('{document}').splitJson(path: 'c').view()"
        ))
        .is_empty()
    );
    assert_eq!(
        run("channel.of('{\"b\": 1}').splitJson(path: 'b').view()").unwrap_err(),
        "IllegalStateException: Error while parsing JSON content - cause: Expected JSON to be an \
         object or an array but got NUMBER for json-path \"b\"."
    );
}

#[test]
fn set_in_a_pipe_binds_all_outputs() {
    assert_eq!(
        lines(
            "channel.of(1, 2) | branch { v ->\n    small: v < 2\n    big: true\n} | set { ch }\n\
             ch.small.view { v -> \"small ${v}\" }"
        ),
        ["small 1"]
    );
}

#[test]
fn chains_declare_before_their_arguments_and_pipes_call_functions() {
    let source = "def twice(ch) {\n    ch.map { v -> v * 2 }\n}\n\
                  workflow {\n    channel.of(1, 2).tap { t }.combine(t).map { a, b -> a + b }.toSortedList().view()\n    \
                  channel.of(3) | twice | view\n}";
    let parsed = parse_script(source);
    assert!(parsed.errors.is_empty(), "{:?}", parsed.errors);
    let module = Rc::new(lower_module(&parsed.tree, source).expect("lowers"));
    let mut output = Output::default();
    nextflow_runtime::run(&module, &mut output).expect("runs");
    let mut printed: Vec<&str> = output.0.lines().collect();
    printed.sort_unstable();
    assert_eq!(printed, ["6", "[2, 3, 3, 4]"]);
}

#[test]
fn published_outputs_are_read_to_the_end() {
    let source = "workflow {\n    main:\n    ch = channel.of(1, 2).map { v -> v * 2 }\n    ch.view()\n\n    \
                  publish:\n    doubled = ch\n}\n\noutput {\n    doubled {\n        path '.'\n    }\n}\n";
    let parsed = parse_script(source);
    assert!(parsed.errors.is_empty(), "{:?}", parsed.errors);
    let module = Rc::new(lower_module(&parsed.tree, source).expect("lowers"));
    assert_eq!(&*module.output_names[0], "doubled");
    let mut output = Output::default();
    nextflow_runtime::run(&module, &mut output).expect("runs");
    assert_eq!(output.0, "2\n4\n");
}

#[test]
fn splitter_records_and_closures() {
    assert_eq!(
        lines(
            "channel.of(\">s1 first seq\\nACGT\\nGG\\n>s2\\nTT\\n\").splitFasta(record: [id: true, header: true, \
             desc: true, seqString: true, sequence: true, text: true]).view { r -> r.inspect() }"
        ),
        [
            "['id':'s1', 'desc':'first seq', 'header':'s1 first seq', 'text':'>s1 first seq\\nACGT\\nGG\\n', \
             'seqString':'ACGTGG', 'sequence':'ACGT\\nGG\\n']",
            "['id':'s2', 'desc':null, 'header':'s2', 'text':'>s2\\nTT\\n', 'seqString':'TT', 'sequence':'TT\\n']"
        ]
    );
    assert_eq!(
        lines("channel.of(\"@r1 x\\nACGT\\n+\\nIIII\\n\").splitFastq(record: true).view()"),
        ["[readHeader:r1 x, readString:ACGT, qualityHeader:, qualityString:IIII]"]
    );
    assert_eq!(
        lines(
            "channel.of(\"a\\nb\\nc\\n\").splitText(limit: 2) { v -> v.trim().toUpperCase() }.view()"
        ),
        ["A", "B"]
    );
    assert_eq!(
        lines(
            "channel.of(\"a\\nb\\n\", \"c\\n\").countLines().view()\n\
             channel.of(\">a\\nAC\\n>b\\nGT\\n\").countFasta().view { v -> \"fasta ${v}\" }"
        ),
        ["3", "fasta 2"]
    );
}

#[test]
fn methods_of_value_channels_act_on_the_value() {
    assert_eq!(
        lines(
            "def a = channel.of(1, 2, 3).collect()\n\
             a.minus(channel.of(2).collect()).view()\n\
             a.toSet().view { v -> v.getClass().getSimpleName() }\n\
             (a.size() + 10).view()"
        ),
        ["[1, 3]", "HashSet", "13"]
    );
    // GPars' own `length()` counts the values a channel holds, which is none yet.
    assert_eq!(
        lines("def n = channel.of(1).collect()\nprintln(n.length())"),
        ["0"]
    );
}

#[test]
fn collate_and_small_library_methods() {
    assert_eq!(
        lines("channel.of(1, 2, 3, 4, 5).collate(2).view()"),
        ["[1, 2]", "[3, 4]", "[5]"]
    );
    assert_eq!(
        lines("channel.of(1, 2, 3, 4, 5).collate(3, 1).view()"),
        ["[1, 2, 3]", "[2, 3, 4]", "[3, 4, 5]", "[4, 5]", "[5]"]
    );
    assert_eq!(
        lines(
            "println([\"abcdef\".take(3), \"abc\".take(10), \"a-b-a\".minus(\"a\"), \"abcdef\".drop(2), \
             \"abcdef\".takeRight(2), \"abcdef\".dropRight(2)])\nSystem.out.println(\"out line\")\n\
             def m = new LinkedHashMap([a: 1])\nm.b = 2\nprintln m\nsleep(1)"
        ),
        ["[abc, abc, -b-a, cdef, ef, abcd]", "out line", "[a:1, b:2]"]
    );
}

#[test]
fn group_keys_give_the_group_size() {
    assert_eq!(
        lines(
            "channel.of([groupKey([id: 'a'], 2), 1], [groupKey([id: 'b'], 3), 2], [groupKey([id: 'a'], 2), 3])\n\
             .groupTuple().view { key, values -> \"${key} ${key.id} ${key.groupSize} ${values}\" }\n\
             def k = groupKey('x', 5)\nprintln([k == 'x', k.toUpperCase(), k.getGroupSize(), \"${k}\"])\n\
             println(groupKey([id: 1], 2) + [x: 1])"
        ),
        ["[true, X, 5, x]", "[id:1, x:1]", "[id:a] a 2 [1, 3]"]
    );
}

#[test]
fn splitters_read_gzip_files() {
    use std::io::Write;
    let directory = tempfile::tempdir().expect("a temporary directory");
    let gz = directory.path().join("rows.csv.gz");
    let mut encoder = flate2::write::GzEncoder::new(Vec::new(), flate2::Compression::default());
    encoder.write_all(b"a,b\n1,2\n").unwrap();
    std::fs::write(&gz, encoder.finish().unwrap()).unwrap();
    assert_eq!(
        lines(&format!(
            "channel.fromPath('{}').splitCsv(header: true).view()",
            gz.display()
        )),
        ["[a:1, b:2]"]
    );
}

/// The split methods of paths give the records of a file, or with `file:` chunk files, named
/// as Nextflow 26.04.6 names them: `seqs.1.fa` in a folder, else after the path.
#[test]
fn paths_split_into_records_and_files() {
    let directory = tempfile::tempdir().expect("a temporary directory");
    let fasta = directory.path().join("seqs.fa");
    std::fs::write(&fasta, ">a\nAC\n>b\nGT\n").expect("writes the file");
    let chunks = directory.path().join("chunks");
    std::fs::create_dir(&chunks).expect("creates the folder");
    let body = format!(
        "def f = file('{}')\nprintln(f.splitFasta().size())\nprintln(f.countFasta())\n\
         println(f.splitFasta(file: file('{}')).collect {{ it.name }})\n\
         println(f.splitFasta(file: file('{}/part.fa')).collect {{ it.name }})",
        fasta.display(),
        chunks.display(),
        directory.path().display()
    );
    assert_eq!(
        lines(&body),
        ["2", "2", "[seqs.1.fa, seqs.2.fa]", "[part.1.fa, part.2.fa]"]
    );
    let second = std::fs::read_to_string(chunks.join("seqs.2.fa")).expect("reads the chunk");
    assert_eq!(second, ">b\nGT\n");
}
