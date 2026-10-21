//! Runs the Python programs of `tests/fixtures/python` and compares their output with what
//! CPython 3.11 printed for them, recorded in the `.out` file next to each program.
// Copyright (c) 2026 The iunctus Contributors
// SPDX-License-Identifier: MIT or Apache-2.0

use std::os::unix::fs::PermissionsExt;
use std::path::Path;
use std::rc::Rc;

use python_evaluator::{Host, Interpreter, LocalHost};

fn run(path: &Path) -> String {
    let text = std::fs::read_to_string(path).expect("reads the program");
    let host = Rc::new(LocalHost::new(path.parent().expect("a folder")));
    let mut interpreter = Interpreter::new(host.clone());
    let result = interpreter.run_text(&path.to_string_lossy(), &text);
    let mut output = host.output.borrow().clone();
    if let Err(error) = result {
        output.push_str(&format!("{error}\n"));
    }
    output
}

#[test]
fn matches_python_output() {
    let folder = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/python");
    let mut programs: Vec<_> = std::fs::read_dir(&folder)
        .expect("reads the fixtures")
        .map(|entry| entry.expect("an entry").path())
        .filter(|path| path.extension().is_some_and(|extension| extension == "py"))
        .collect();
    programs.sort();
    assert!(!programs.is_empty());
    for program in programs {
        let expected =
            std::fs::read_to_string(program.with_extension("out")).expect("reads the output");
        let actual = run(&program);
        assert_eq!(actual, expected, "{}", program.display());
    }
}

fn evaluate(code: &str) -> Result<String, python_evaluator::Error> {
    let host = Rc::new(LocalHost::new(std::env::temp_dir()));
    let mut interpreter = Interpreter::new(host.clone());
    interpreter.run_text("<test>", code)?;
    let output = host.output.borrow().clone();
    Ok(output)
}

/// As CPython 3.13, `set.pop()` takes the first member of the table from where the last pop
/// stopped, and a new member takes the slot that a removed one left.
#[test]
fn pops_like_cpython() {
    let code = "s = {16, 32, 1, 0, 48}\nprint(s.pop(), s.pop(), list(s))\nt = {0, 8}\nt.discard(0)\nt.add(16)\nprint(list(t))\n";
    let output = evaluate(code).expect("runs");
    assert_eq!(output, "32 1 [0, 48, 16]\n[16, 8]\n");
}

/// As CPython 3.13, a set made from another set takes its layout, and the operators merge
/// sets in the order of their tables.
#[test]
fn merges_like_cpython() {
    let code = "print(list({16, 32, 1, 0, 48} | {64}))\na = {16, 32, 1, 0, 48}\nprint(list(a.union([64])), list(set(a)), list(a.copy()))\nb = set([8, 16, 24, 0])\nb.discard(16)\nprint(list(set(b)), list(b - {24}), list(b ^ {7, 15}))\n";
    let output = evaluate(code).expect("runs");
    let expected = "[32, 1, 0, 48, 16, 64]\n[32, 1, 0, 48, 16, 64] [32, 1, 0, 48, 16] [32, 1, 0, 48, 16]\n[8, 24, 0] [8, 0] [0, 7, 8, 15, 24]\n";
    assert_eq!(output, expected);
}

/// Sets change where they are: 200,000 adds, lookups and pops take seconds, not the hours of
/// a copy of the set for each call.
#[test]
fn changes_sets_quickly() {
    let code = "s = set()\nfor i in range(200000):\n    s.add(i * 7)\nfound = sum(1 for i in range(200000) if i * 7 in s)\nt = set()\nt.update(range(1000))\nwhile s:\n    s.pop()\nprint(found, len(s), len(t))\n";
    let start = std::time::Instant::now();
    let output = evaluate(code).expect("runs");
    assert_eq!(output, "200000 0 1000\n");
    let seconds = start.elapsed().as_secs();
    assert!(seconds < 60, "took {seconds} s");
}

#[test]
fn reports_exception_lines() {
    let error = evaluate("x = 1\ny = {}\ny['missing']\n").expect_err("a KeyError");
    assert_eq!(error.to_string(), "<test>:3: KeyError: 'missing'");
    assert!(!error.is_unsupported());
}

#[test]
fn propagates_unsupported_features() {
    let error = evaluate("import os\ntry:\n    os.fork()\nexcept Exception:\n    pass\n")
        .expect_err("unsupported");
    assert!(error.is_unsupported(), "{error}");
}

#[test]
fn buffers_file_writes() {
    let output = evaluate(
        "with open('iunctus-test-never-written.txt', 'w') as f:\n    f.write('hi')\nprint(open('iunctus-test-never-written.txt').read())\n",
    )
    .expect("runs");
    assert_eq!(output, "hi\n");
    assert!(
        !std::env::temp_dir()
            .join("iunctus-test-never-written.txt")
            .exists()
    );
}

#[test]
fn rejects_unavailable_mutations() {
    for (name, call) in [
        ("symlink", "os.symlink('source.txt', 'created.txt')"),
        ("link", "os.link('source.txt', 'created.txt')"),
        ("chmod", "os.chmod('source.txt', 0)"),
        ("utime", "os.utime('source.txt', (1, 1))"),
    ] {
        let folder = tempfile::tempdir().expect("a temporary directory");
        let source = folder.path().join("source.txt");
        std::fs::write(&source, "source").expect("writes the source");
        let before = std::fs::metadata(&source).expect("source metadata");
        let before = (
            before.permissions().mode(),
            before.modified().expect("modified"),
        );
        let host = Rc::new(LocalHost::new(folder.path()));
        let mut interpreter = Interpreter::new(host.clone());
        interpreter
            .run_text(
                "<setup>",
                "with open('virtual.txt', 'w') as file:\n    file.write('virtual')\n",
            )
            .expect("writes a virtual file");

        let error = interpreter
            .run_text("<test>", &format!("import os\n{call}\n"))
            .err()
            .expect("mutation is unsupported");
        let diagnostic = format!("os.{name} while the workflow is planned");
        assert!(error.is_unsupported(), "{name}: {error}");
        assert_eq!(error.message(), diagnostic);
        assert_eq!(
            error.to_string(),
            format!("<test>:2: Not supported yet: {diagnostic}")
        );
        assert!(!folder.path().join("created.txt").exists());
        assert_eq!(std::fs::read(&source).expect("source remains"), b"source");
        let after = std::fs::metadata(&source).expect("source metadata remains");
        assert_eq!(after.permissions().mode(), before.0);
        assert_eq!(after.modified().expect("modified remains"), before.1);
        assert_eq!(
            host.read(&folder.path().join("virtual.txt"))
                .expect("virtual file remains"),
            b"virtual"
        );

        interpreter
            .run_text(
                "<supported>",
                "import os\nos.rename('virtual.txt', 'renamed.txt')\n",
            )
            .expect("rename remains supported");
        assert_eq!(
            host.read(&folder.path().join("renamed.txt"))
                .expect("renamed virtual file"),
            b"virtual"
        );
    }
}

#[test]
fn bounds_directory_walks() {
    let folder = tempfile::tempdir().expect("a temporary directory");
    let tree = folder.path().join("tree");
    for directory in ["keep", "prune", "real/nested"] {
        std::fs::create_dir_all(tree.join(directory)).expect("makes a directory");
    }
    std::fs::write(tree.join("keep/leaf.txt"), "keep").expect("writes a kept file");
    std::fs::write(tree.join("prune/hidden.txt"), "prune").expect("writes a pruned file");
    std::fs::write(tree.join("real/nested/data.txt"), "data").expect("writes nested data");
    std::os::unix::fs::symlink("real", tree.join("link")).expect("links to a directory");
    std::os::unix::fs::symlink("..", tree.join("real/back")).expect("links to an ancestor");
    let host = Rc::new(LocalHost::new(folder.path()));
    let mut interpreter = Interpreter::new(host.clone());
    interpreter
        .run_text(
            "<test>",
            concat!(
                "import os\n",
                "os.makedirs('tree/virtual/inside')\n",
                "with open('tree/virtual/inside/memory.txt', 'w') as file:\n",
                "    file.write('memory')\n",
                "for root, dirs, files in os.walk('tree'):\n",
                "    if root == 'tree':\n",
                "        dirs.remove('prune')\n",
                "    print('top', root, dirs, files)\n",
                "for root, dirs, files in os.walk('tree', topdown=False):\n",
                "    print('bottom', root)\n",
                "for root, dirs, files in os.walk('tree', followlinks=True):\n",
                "    print('follow', root)\n",
            ),
        )
        .expect("walks the tree");
    assert_eq!(
        *host.output.borrow(),
        "top tree ['keep', 'link', 'real', 'virtual'] []\n\
         top tree/keep [] ['leaf.txt']\n\
         top tree/real ['back', 'nested'] []\n\
         top tree/real/nested [] ['data.txt']\n\
         top tree/virtual ['inside'] []\n\
         top tree/virtual/inside [] ['memory.txt']\n\
         bottom tree/keep\n\
         bottom tree/prune\n\
         bottom tree/real/nested\n\
         bottom tree/real\n\
         bottom tree/virtual/inside\n\
         bottom tree/virtual\n\
         bottom tree\n\
         follow tree\n\
         follow tree/keep\n\
         follow tree/link\n\
         follow tree/link/nested\n\
         follow tree/prune\n\
         follow tree/real\n\
         follow tree/real/nested\n\
         follow tree/virtual\n\
         follow tree/virtual/inside\n"
    );
}

#[test]
fn propagates_walk_errors() {
    let folder = tempfile::tempdir().expect("a temporary directory");
    let host = Rc::new(LocalHost::new(folder.path()));
    let mut interpreter = Interpreter::new(host.clone());
    interpreter
        .run_text(
            "<test>",
            concat!(
                "import os\n",
                "def report(error):\n",
                "    print(type(error).__name__, error.errno, error.filename)\n",
                "for item in os.walk('missing', onerror=report):\n",
                "    print('unexpected', item)\n",
                "print(list(os.walk('missing')))\n",
            ),
        )
        .expect("reports failed listings");
    assert_eq!(*host.output.borrow(), "FileNotFoundError 2 missing\n[]\n");

    let error = interpreter
        .run_text(
            "<test>",
            concat!(
                "import os\n",
                "def stop(error):\n",
                "    raise ValueError(error.filename)\n",
                "list(os.walk('missing', onerror=stop))\n",
            ),
        )
        .err()
        .expect("callback error propagates");
    assert!(error.is("ValueError"), "{error}");
    assert_eq!(error.message(), "missing");
}

#[test]
fn bounds_infinite_loops() {
    let host = Rc::new(LocalHost::new(std::env::temp_dir()));
    let mut interpreter = Interpreter::new(host);
    interpreter.step_limit = 10_000;
    let error = interpreter
        .run_text("<test>", "while True:\n    pass\n")
        .err()
        .expect("stops");
    assert!(error.is_unsupported());
}

#[test]
fn budgets_each_evaluation() {
    let host = Rc::new(LocalHost::new(std::env::temp_dir()));
    let mut interpreter = Interpreter::new(host);
    interpreter.step_limit = 10_000;
    let globals = interpreter
        .run_text(
            "<test>",
            "def f():\n    for i in range(4000):\n        pass\n",
        )
        .expect("defines");
    let function = globals.get("f").expect("a function");
    for _ in 0..5 {
        let result = interpreter.call(&function, python_evaluator::Args::default());
        assert!(result.is_ok(), "{:?}", result.err());
    }
    let error = interpreter
        .exec_text("<test>", "for n in range(5):\n    f()\n", &globals)
        .expect_err("one evaluation shares the budget");
    assert!(error.is_unsupported());
}

#[test]
fn recursion_is_limited() {
    let error = evaluate("def f(n):\n    return f(n + 1)\nf(0)\n").expect_err("too deep");
    assert!(error.to_string().contains("RecursionError"), "{error}");
}

#[test]
fn matches_safe_loader() {
    let output = evaluate(
        "import yaml\nprint(yaml.safe_load('a: yes\\nb: 1e3\\nc: 1.0e+3\\nd: 010\\ne: 1:30\\nf: ~\\ng: [1, x]\\nh: &x {k: 1}\\ni: *x\\n'))\n",
    )
    .expect("runs");
    assert_eq!(
        output,
        "{'a': True, 'b': '1e3', 'c': 1000.0, 'd': 8, 'e': 90, 'f': None, 'g': [1, 'x'], 'h': {'k': 1}, 'i': {'k': 1}}\n"
    );
}

/// A host that answers each command with its words, and exit code 3 for `fail`.
struct CommandHost(LocalHost);

impl python_evaluator::Host for CommandHost {
    fn cwd(&self) -> std::path::PathBuf {
        self.0.cwd()
    }
    fn set_cwd(&self, path: &Path) {
        self.0.set_cwd(path)
    }
    fn read(&self, path: &Path) -> std::io::Result<Vec<u8>> {
        self.0.read(path)
    }
    fn stat(&self, path: &Path) -> Option<python_evaluator::Stat> {
        self.0.stat(path)
    }
    fn list(&self, path: &Path) -> std::io::Result<Vec<String>> {
        self.0.list(path)
    }
    fn write(&self, path: &Path, data: &[u8], append: bool) -> std::io::Result<()> {
        self.0.write(path, data, append)
    }
    fn make_dir(&self, path: &Path) -> std::io::Result<()> {
        self.0.make_dir(path)
    }
    fn remove(&self, path: &Path) -> std::io::Result<()> {
        self.0.remove(path)
    }
    fn print(&self, text: &str) {
        self.0.print(text)
    }
    fn environment(&self) -> Vec<(String, String)> {
        self.0.environment()
    }
    fn run_command(
        &self,
        command: &python_evaluator::Command,
    ) -> Result<python_evaluator::CommandOutcome, python_evaluator::Error> {
        let words = command.argv.join(" ");
        Ok(python_evaluator::CommandOutcome {
            exit_code: if words.contains("fail") { 3 } else { 0 },
            stdout: format!("ran {words}\n").into_bytes(),
            stderr: Vec::new(),
        })
    }
}

#[test]
fn dispatches_host_commands() {
    let host = Rc::new(CommandHost(LocalHost::new(std::env::temp_dir())));
    let mut interpreter = Interpreter::new(host.clone());
    interpreter
        .run_text(
            "<test>",
            "import os, subprocess\n\
             r = subprocess.run(['tool', '--version'], capture_output=True, text=True)\n\
             print(r.returncode, repr(r.stdout), r.stderr)\n\
             print(subprocess.check_output('tool a', shell=True), subprocess.call(['fail']))\n\
             try:\n    subprocess.run(['fail'], check=True)\n\
             except subprocess.CalledProcessError as error:\n    print(error.returncode, error.cmd, error)\n\
             print(os.system('fail'), os.popen('tool b').read().strip(), subprocess.getoutput('tool c'))\n",
        )
        .expect("runs");
    assert_eq!(
        *host.0.output.borrow(),
        "0 'ran tool --version\\n' \n\
         b'ran /bin/sh -c tool a\\n' 3\n\
         3 ['fail'] Command '['fail']' returned non-zero exit status 3.\n\
         768 ran /bin/sh -c tool b ran /bin/sh -c tool c\n"
    );
    let error = evaluate("import subprocess\nsubprocess.run(['tool'])\n").expect_err("no commands");
    assert!(error.is_unsupported(), "{error}");
}

#[test]
fn rejects_missing_modules() {
    // Packages without a stand-in run in a container; tests/packages.rs covers them.
    let error = evaluate("import turtle\n").expect_err("no stand-in");
    assert!(error.is_unsupported(), "{error}");
}

#[test]
fn bounds_integer_roots() {
    assert_eq!(
        evaluate("import math\nprint(math.isqrt(9223372036854775807), math.isqrt(10 ** 100), math.isqrt(0))\n")
            .expect("integer roots"),
        format!("3037000499 1{} 0\n", "0".repeat(50))
    );
    for code in ["math.isqrt()", "math.isqrt(1.5)"] {
        assert!(
            evaluate(&format!("import math\n{code}\n"))
                .expect_err("invalid argument")
                .is("TypeError")
        );
    }
    assert!(
        evaluate("import math\nmath.isqrt(-1)\n")
            .expect_err("negative root")
            .is("ValueError")
    );
}

#[test]
fn bounds_recursive_representations() {
    assert_eq!(
        evaluate("a = []\na.append(a)\nd = {}\nd['self'] = d\nprint(a, d)\nprint([a, a])\n")
            .expect("recursive repr"),
        "[[...]] {'self': {...}}\n[[[...]], [[...]]]\n"
    );
}

#[test]
fn rejects_index_shapes() {
    for expression in [
        "pd.DataFrame({'x': [1]}).set_index(pd.Index(['a', 'b']))",
        "pd.MultiIndex.from_arrays([[1, 2], ['a']])",
        "pd.MultiIndex.from_arrays([[1], ['a']], names=['one'])",
        "pd.Series([1], index=['a', 'b'])",
        "pd.DataFrame([[1]], index=['a', 'b'])",
    ] {
        let error = evaluate(&format!("import pandas as pd\n{expression}\n"))
            .expect_err("invalid index shape");
        assert!(error.is("ValueError"), "{expression}: {error}");
    }
    assert_eq!(
        evaluate("import pandas as pd\nprint(pd.Series(index=['a', 'b']).isna().tolist())\n")
            .expect("empty series"),
        "[True, True]\n"
    );
}

#[test]
fn preserves_buffered_appends() {
    let folder = tempfile::tempdir().expect("a folder");
    let path = folder.path().join("existing.txt");
    std::fs::write(&path, "old").expect("initial content");
    let host = Rc::new(LocalHost::new(folder.path()));
    let mut interpreter = Interpreter::new(host.clone());
    interpreter.run_text("<test>", "with open('existing.txt', 'a') as f:\n    f.write('new')\nwith open('existing.txt', 'a') as f:\n    f.write('again')\nprint(open('existing.txt').read())\n").expect("appends");
    assert_eq!(*host.output.borrow(), "oldnewagain\n");
    assert_eq!(std::fs::read_to_string(path).expect("disk content"), "old");
}

#[test]
fn releases_truth_iterators() {
    let output = evaluate(
        "class Item:\n    def __bool__(self):\n        next(it, None)\n        return True\nit = iter([Item(), 0])\nprint(any(it))\nit = iter([Item(), 0])\nprint(all(it))\n",
    ).expect("reentrant item truthiness");
    assert_eq!(output, "True\nTrue\n");
}

#[test]
fn suspends_generator_effects() {
    let output = evaluate(
        "events = []\ndef source():\n    events.append('first')\n    x = yield 1\n    events.append(x)\n    yield 2\n    raise ValueError('tail')\ng = source()\nprint(events)\nprint(next(g), events)\nprint(next(g), events)\ntry:\n    next(g)\nexcept ValueError as error:\n    print(error)\nprint(next(g, 'done'))\n",
    ).expect("suspended generator");
    assert_eq!(output, "[]\n1 ['first']\n2 ['first', None]\ntail\ndone\n");
}

#[test]
fn resumes_generator_flow() {
    let output = evaluate(
        "def source():\n    for i in range(3):\n        for j in range(3):\n            if j == 0:\n                continue\n            if j == 2:\n                break\n            yield (i, j)\n        else:\n            yield 'wrong'\n    else:\n        yield 'outer'\n    k = 0\n    while k < 2:\n        k += 1\n        yield k\n    else:\n        yield from ['end', 'tail']\nprint(list(source()))\ndef forever():\n    while True:\n        yield True\ng = forever()\nprint(any(g), next(g))\nfor item in forever():\n    print(item)\n    break\ndef false_first():\n    yield False\n    raise ValueError('unconsumed')\nprint(all(false_first()))\n",
    ).expect("resumed loops");
    assert_eq!(
        output,
        "[(0, 1), (1, 1), (2, 1), 'outer', 1, 2, 'end', 'tail']\nTrue True\nTrue\nFalse\n"
    );
}

#[test]
fn preserves_delegation_timing() {
    let output = evaluate(
        "events = []\ndef inner():\n    events.append('first')\n    yield 1\n    events.append('second')\n    yield 2\ndef outer():\n    yield from inner()\n    events.append('end')\ng = outer()\nprint(events)\nprint(next(g), events)\nprint(next(g), events)\nprint(next(g, None), events)\n",
    ).expect("incremental delegation");
    assert_eq!(
        output,
        "[]\n1 ['first']\n2 ['first', 'second']\nNone ['first', 'second', 'end']\n"
    );
}

#[test]
fn rejects_suspension_shapes() {
    for code in [
        "def source():\n    yield 1\n    x = 1 < 2 < (yield 2)\ng = source()\nnext(g)\nnext(g)\n",
        "def source():\n    while (yield 1):\n        pass\nnext(source())\n",
        "def source():\n    yield 1\nsource().send(None)\n",
    ] {
        let error = evaluate(code).expect_err("unsupported suspension");
        assert!(error.is_unsupported(), "{error}");
        assert!(
            error.to_string().contains("suspension") || error.to_string().contains("send"),
            "{error}"
        );
    }
    let output = evaluate("def source():\n    next(g)\n    yield 1\ng = source()\ntry:\n    next(g)\nexcept ValueError as error:\n    print(error)\n").expect("reentrant advancement is an exception");
    assert_eq!(output, "generator already executing\n");
}

#[test]
fn rejects_reentrant_advancement() {
    for body in [
        "next(g)",
        "any(g)",
        "all(g)",
        "list(g)",
        "for item in g:\n        pass",
        "consume()",
        "list(itertools.islice(g, 1))",
    ] {
        let code = format!(
            "import itertools\ndef consume():\n    for item in g:\n        pass\ndef source():\n    {body}\n    yield 1\ng = source()\nnext(g)\n"
        );
        let error = evaluate(&code).expect_err("reentrant advancement");
        assert!(error.is("ValueError"), "{body}: {error}");
        assert!(error.to_string().contains("generator already executing"));
    }
}

#[test]
fn suspends_generator_expressions() {
    let output = evaluate(
        "events = []\nclass Source:\n    def __init__(self):\n        self.position = 0\n    def __iter__(self):\n        events.append('iter')\n        return self\n    def __next__(self):\n        events.append('next')\n        if self.position == 2:\n            raise ValueError('tail')\n        value = self.position\n        self.position += 1\n        return value\ndef accept(x):\n    events.append(('filter', x))\n    return x == 0\ndef inner(x):\n    events.append(('inner', x))\n    return [x, x + 10]\ndef record(x):\n    events.append(('value', x))\n    return x\ng = (record(y) for x in Source() if accept(x) for y in inner(x) if y < 11)\nprint(events)\nprint(next(g), events)\nprint(next(g), events)\ntry:\n    next(g)\nexcept ValueError as error:\n    print(error, events)\nprint(next(g, 'end'))\n",
    ).expect("lazy generator expression");
    assert_eq!(
        output,
        "['iter']\n0 ['iter', 'next', ('filter', 0), ('inner', 0), ('value', 0)]\n10 ['iter', 'next', ('filter', 0), ('inner', 0), ('value', 0), ('value', 10)]\ntail ['iter', 'next', ('filter', 0), ('inner', 0), ('value', 0), ('value', 10), 'next', ('filter', 1), 'next']\nend\n"
    );
}

#[test]
fn captures_generator_bindings() {
    let output = evaluate(
        "items = [1]\ncaptured = items\nfactor = 2\ng = (factor * x for x in items)\nitems = [99]\ncaptured.append(2)\nfactor = 3\nprint(next(g), next(g))\nfactor = 4\ncaptured.append(3)\nprint(next(g))\nprint(next(g, 'end'))\ncaptured.append(4)\nprint(next(g, 'end'))\ndef make():\n    scale = 2\n    g = (scale * x for x in [3])\n    scale = 4\n    return g\nprint(next(make()))\n",
    ).expect("outer capture and closures");
    assert_eq!(output, "3 6\n12\nend\nend\n12\n");
}

#[test]
fn stops_custom_iterators() {
    let output = evaluate(
        "events = []\nclass Source:\n    def __init__(self, first):\n        self.first = first\n        self.used = False\n    def __iter__(self):\n        events.append('iter')\n        return self\n    def __next__(self):\n        events.append('next')\n        if self.used:\n            raise ValueError('unconsumed')\n        self.used = True\n        return self.first\nprint(any(Source(1)), all(Source(0)))\nfor value in Source(7):\n    print(value)\n    break\nprint(events)\nprint(any(x for x in Source(1)), all(x for x in Source(0)))\n",
    ).expect("lazy custom iteration");
    assert_eq!(
        output,
        "True False\n7\n['iter', 'next', 'iter', 'next', 'iter', 'next']\nTrue False\n"
    );
}

#[test]
fn preserves_lazy_iteration() {
    let output = evaluate(
        "events = []\nclass Sequence:\n    def __getitem__(self, index):\n        events.append(index)\n        if index == 2:\n            raise IndexError('end')\n        return index + 1\ng = (x for x in Sequence())\nprint(events)\nprint(next(g), events)\nprint(next(g), events)\nprint(next(g, None), events)\nitems = [1]\nfor item in items:\n    print(item)\n    if item == 1:\n        items.append(2)\nit = iter(items)\nprint(list(it))\nitems.append(3)\nprint(next(it, 'end'))\n",
    ).expect("lazy sequence and live list");
    assert_eq!(
        output,
        "[]\n1 [0]\n2 [0, 1]\nNone [0, 1, 2]\n1\n2\n[1, 2]\nend\n"
    );
}

#[test]
fn validates_outer_iterators() {
    let output = evaluate(
        "class Broken:\n    def __iter__(self):\n        raise ValueError('create')\ntry:\n    g = (x for x in Broken())\nexcept ValueError as error:\n    print(error)\nclass Invalid:\n    def __iter__(self):\n        return [1]\ntry:\n    g = (x for x in Invalid())\nexcept TypeError as error:\n    print(error)\n",
    ).expect("outer protocol checks");
    assert_eq!(output, "create\niter() returned non-iterator\n");
}
