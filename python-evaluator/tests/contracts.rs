//! Regressions for accepted options with observable semantics.
// Copyright (c) 2026 The uwe Contributors
// SPDX-License-Identifier: MIT or Apache-2.0

use std::cell::RefCell;
use std::path::{Path, PathBuf};
use std::rc::Rc;

use python_evaluator::{Command, CommandOutcome, Error, Host, Interpreter, LocalHost, Stat};

fn evaluate(code: &str) -> Result<String, Error> {
    let host = Rc::new(LocalHost::new(std::env::temp_dir()));
    let mut interpreter = Interpreter::new(host.clone());
    interpreter.run_text("<test>", code)?;
    let output = host.output.borrow().clone();
    Ok(output)
}

#[test]
fn preserves_text_columns() {
    let output = evaluate(
        "import pandas as pd\nfrom io import StringIO\ntext = 'n;s\\n1.234,5;1.234,5\\n'\nf = pd.read_csv(StringIO(text), sep=';', decimal=',', thousands='.', dtype={'s': str})\nprint(f['n'].tolist(), f['s'].tolist())\n",
    ).expect("evaluates");
    assert_eq!(output, "[1234.5] ['1.234,5']\n");
}

#[test]
fn preserves_mixed_identifiers() {
    let output = evaluate(
        "import pandas as pd\nfrom io import StringIO\ntext = 'id\\n1.234,5\\nunknown\\n'\nprint(pd.read_csv(StringIO(text), sep=';', decimal=',', thousands='.')['id'].tolist())\n",
    ).expect("evaluates");
    assert_eq!(output, "['1.234,5', 'unknown']\n");
}

#[test]
fn enforces_csv_encodings() {
    let output = evaluate(
        "import pandas as pd\nwith open('latin.csv', 'wb') as f:\n    f.write(b'id\\ns\\xe4\\n')\nprint(pd.read_csv('latin.csv', encoding='latin-1')['id'].tolist())\ntry:\n    pd.read_csv('latin.csv')\nexcept UnicodeDecodeError:\n    print('strict rejected')\nprint(pd.read_csv('latin.csv', encoding_errors='replace')['id'].tolist())\n",
    ).expect("evaluates");
    assert_eq!(output, "['sä']\nstrict rejected\n['s�']\n");
}

#[test]
fn rejects_csv_options() {
    for option in [
        "parse_dates=['date']",
        "parse_dates=True",
        "date_format='%Y'",
        "chunksize=1",
        "iterator=True",
        "encoding='utf-16'",
        "escapechar='~'",
        "doublequote=False",
        "lineterminator=';'",
        "on_bad_lines='skip'",
        "compression='zip'",
        "engine='pyarrow'",
        "dtype_backend='pyarrow'",
        "header=[0, 1]",
        "quoting=2",
    ] {
        let error = evaluate(&format!("import pandas as pd\nfrom io import StringIO\npd.read_csv(StringIO('date\\n2026\\n'), {option})\n"))
            .expect_err("unsupported option");
        assert!(error.is_unsupported(), "{option}: {error}");
    }
    assert!(evaluate("import pandas as pd\nfrom io import StringIO\npd.read_csv(StringIO('date\\n2026\\n'), parse_dates=False, chunksize=None, iterator=False)\n").is_ok());
}

#[test]
fn propagates_numpy_nan() {
    let output = evaluate(
        r#"
import numpy as np
import pandas as pd
for values in [[1, np.nan], [np.nan, 1], [np.nan]]:
    array = np.array(values)
    print(np.isnan(np.sum(values)), np.isnan(np.min(values)), np.isnan(np.max(values)))
    print(np.isnan(array.sum()), np.isnan(array.min()), np.isnan(array.max()))
print(np.sum([]), np.sum([1, 2]), pd.Series([1, np.nan]).sum())
for operation in [np.min, np.max]:
    try:
        operation([])
    except ValueError:
        print('empty rejected')
"#,
    )
    .expect("NumPy missing-value policy");
    assert_eq!(
        output,
        format!(
            "{}0 3 1\nempty rejected\nempty rejected\n",
            "True True True\n".repeat(6)
        )
    );
}

#[test]
fn preserves_csv_comments() {
    let output = evaluate(
        r##"
import pandas as pd
from io import StringIO
text = 'id,n\n"sample#1",7\n"a""#b",8\n"line\n#tag",9\n# ignored "\nplain,10# comment\n'
frame = pd.read_csv(StringIO(text), comment='#')
print(frame['id'].tolist() == ['sample#1', 'a"#b', 'line\n#tag', 'plain'])
print(frame['n'].tolist() == [7, 8, 9, 10])
"##,
    )
    .expect("comments respect quoted fields");
    assert_eq!(output, "True\nTrue\n");
}

#[test]
fn retains_blank_records() {
    let output = evaluate(
        r#"
import pandas as pd
from io import StringIO
frame = pd.read_csv(StringIO('id\n1\n\n2\n'), skip_blank_lines=False)
print(len(frame), pd.isna(frame['id'].iloc[1]))
print(len(pd.read_csv(StringIO('id\n1\n\n2\n'), skip_blank_lines=True)))
"#,
    )
    .expect("blank-record option");
    assert_eq!(output, "3 True\n2\n");
}

#[test]
fn checks_array_truth() {
    let output = evaluate(
        r#"
import numpy as np
for values in [[0], [0.0], [False], [2], [True]]:
    array = np.array(values)
    print(bool(array), 'yes' if array else 'no')
try:
    bool(np.array([0, 1]))
except ValueError:
    print('bool ambiguous')
try:
    if np.array([False, False]):
        print('wrong')
except ValueError:
    print('branch ambiguous')
"#,
    )
    .expect("array truth semantics");
    assert_eq!(
        output,
        "False no\nFalse no\nFalse no\nTrue yes\nTrue yes\nbool ambiguous\nbranch ambiguous\n"
    );
}

#[test]
fn generates_numeric_ranges() {
    let output = evaluate(
        r#"
import numpy as np
print(np.arange(0, 1, 0.25).tolist())
print(np.arange(start=1.0, stop=0.0, step=-0.25).tolist())
print(np.arange(3).tolist(), np.arange(3, 0, -1).tolist())
print(np.arange(0, stop=1, step=0.5).tolist())
print(np.arange(9223372036854775805, 9223372036854775807, 2).tolist())
try:
    np.arange(0, 1, 0.0)
except ZeroDivisionError:
    print('zero rejected')
try:
    np.arange(0, 1, 'bad')
except TypeError:
    print('type rejected')
"#,
    )
    .expect("integer and fractional ranges");
    assert_eq!(
        output,
        "[0.0, 0.25, 0.5, 0.75]\n[1.0, 0.75, 0.5, 0.25]\n[0, 1, 2] [3, 2, 1]\n[0.0, 0.5]\n[9223372036854775805]\nzero rejected\ntype rejected\n"
    );
}

#[test]
fn compares_packaging_versions() {
    let output = evaluate(
        r#"
from packaging.version import Version, InvalidVersion
print(Version('1.0rc1') < Version('1.0'), Version('1.0') == Version('1.0.0'))
print(Version('1.0.dev1') < Version('1.0a1'), Version('1.0.post1') > Version('1.0'))
print(Version('1!0') > Version('999'), Version('1.0+abc') > Version('1.0'))
print(Version('1.0rc1+ABC').public, Version('1!1.0rc1+ABC').base_version)
print(Version('1').minor, Version('1').micro, Version('1.0rc1').is_prerelease)
try:
    Version('not a version')
except InvalidVersion:
    print('invalid rejected')
"#,
    )
    .expect("PEP 440 semantics");
    assert_eq!(
        output,
        "True True\nTrue True\nTrue True\n1.0rc1 1!1.0\n0 0 True\ninvalid rejected\n"
    );
}

#[test]
fn maintains_stringio_state() {
    let output = evaluate(
        r#"
from io import StringIO
buffer = StringIO('äbc\ndef\n')
print(buffer.read(1), buffer.tell(), buffer.readline())
print(buffer.read(), repr(buffer.read()))
buffer.seek(0)
buffer.write('Z')
print(repr(buffer.getvalue()))
buffer.seek(2)
print(buffer.truncate(), repr(buffer.getvalue()), buffer.tell())
buffer.seek(4)
buffer.write('Q')
print(repr(buffer.getvalue()))
buffer.seek(0)
print(buffer.truncate(1), buffer.tell(), repr(buffer.getvalue()))
buffer.close()
print(buffer.closed)
try:
    buffer.read()
except ValueError:
    print('closed rejected')
with StringIO('a\nb\n') as lines:
    for line in lines:
        print(repr(line))
        break
    print(repr(lines.readline()))
print(lines.closed)
"#,
    )
    .expect("stateful Unicode buffer");
    assert_eq!(
        output,
        "ä 1 bc\n\ndef\n ''\n'Zbc\\ndef\\n'\n2 'Zb' 2\n'Zb\\x00\\x00Q'\n1 0 'Z'\nTrue\nclosed rejected\n'a\\n'\n'b\\n'\nTrue\n"
    );
}

#[test]
fn enforces_pandas_contracts() {
    let output = evaluate(
        "import pandas as pd\nf = pd.DataFrame({'k': ['A', 'A'], 'n': [1, 2]})\ntry:\n    f.set_index('k', verify_integrity=True)\nexcept ValueError:\n    print('duplicate index rejected')\nprint(f.set_index('k').shape)\nfor obj in [f, f['n'], f.index, pd.Series([])]:\n    try:\n        bool(obj)\n    except ValueError:\n        print('ambiguous')\nr = pd.DataFrame({'k': ['A'], 'v': [3]})\ntry:\n    f.merge(r, on='k', validate='1:1')\nexcept Exception:\n    print('left duplicates rejected')\ntry:\n    r.merge(f, on='k', validate='m:1')\nexcept Exception:\n    print('right duplicates rejected')\nprint(f.merge(r, on='k', validate='m:1').shape)\nprint(r.merge(f, on='k', validate='1:m').shape)\nprint(f.merge(f, on='k', validate='m:m').shape)\n",
    ).expect("evaluates");
    assert_eq!(
        output,
        "duplicate index rejected\n(2, 1)\nambiguous\nambiguous\nambiguous\nambiguous\nleft duplicates rejected\nright duplicates rejected\n(2, 3)\n(2, 3)\n(4, 3)\n"
    );
}

#[test]
fn preserves_flushed_chunks() {
    let output = evaluate(
        "f = open('flush.txt', 'w')\nf.write('old')\nf.flush()\nprint(open('flush.txt').read())\nf.write('new')\nf.flush()\nf.flush()\nprint(open('flush.txt').read())\nf.close()\nprint(open('flush.txt').read())\nf = open('flush.txt', 'a')\nf.write('A')\nf.flush()\nf.flush()\nf.write('B')\nf.flush()\nf.close()\nprint(open('flush.txt').read())\n",
    ).expect("evaluates");
    assert_eq!(output, "old\noldnew\noldnew\noldnewAB\n");
}

struct CommandHost {
    local: LocalHost,
    commands: RefCell<Vec<Command>>,
}

impl Host for CommandHost {
    fn cwd(&self) -> PathBuf {
        self.local.cwd()
    }
    fn set_cwd(&self, path: &Path) {
        self.local.set_cwd(path)
    }
    fn read(&self, path: &Path) -> std::io::Result<Vec<u8>> {
        self.local.read(path)
    }
    fn stat(&self, path: &Path) -> Option<Stat> {
        self.local.stat(path)
    }
    fn list(&self, path: &Path) -> std::io::Result<Vec<String>> {
        self.local.list(path)
    }
    fn write(&self, path: &Path, data: &[u8], append: bool) -> std::io::Result<()> {
        self.local.write(path, data, append)
    }
    fn make_dir(&self, path: &Path) -> std::io::Result<()> {
        self.local.make_dir(path)
    }
    fn remove(&self, path: &Path) -> std::io::Result<()> {
        self.local.remove(path)
    }
    fn print(&self, text: &str) {
        self.local.print(text)
    }
    fn environment(&self) -> Vec<(String, String)> {
        self.local.environment()
    }
    fn run_command(&self, command: &Command) -> Result<CommandOutcome, Error> {
        self.commands.borrow_mut().push(command.clone());
        Ok(CommandOutcome::default())
    }
}

#[test]
fn resolves_subprocess_directory() {
    let host = Rc::new(CommandHost {
        local: LocalHost::new("/workflow"),
        commands: RefCell::new(Vec::new()),
    });
    let mut interpreter = Interpreter::new(host.clone());
    interpreter.run_text("<test>", "import subprocess as s\ns.run(['true'], cwd='data/../samples')\ns.check_output(['true'], cwd='/other')\ns.call(['true'], cwd='samples')\ns.check_call(['true'], cwd='samples')\ns.Popen(['true'], cwd='samples')\ns.run(['true'])\n").expect("dispatches");
    let paths: Vec<PathBuf> = host
        .commands
        .borrow()
        .iter()
        .map(|command| command.cwd.clone())
        .collect();
    assert_eq!(
        paths,
        [
            "/workflow/samples",
            "/other",
            "/workflow/samples",
            "/workflow/samples",
            "/workflow/samples",
            "/workflow"
        ]
        .map(PathBuf::from)
    );
}

#[test]
fn rejects_subprocess_options() {
    for api in ["run", "check_output", "call", "check_call", "Popen"] {
        for option in ["env={}", "input=b'hello'", "timeout=1"] {
            let host = Rc::new(CommandHost {
                local: LocalHost::new("/workflow"),
                commands: RefCell::new(Vec::new()),
            });
            let mut interpreter = Interpreter::new(host.clone());
            let error = interpreter
                .run_text(
                    "<test>",
                    &format!("import subprocess\nsubprocess.{api}(['true'], {option})\n"),
                )
                .err()
                .expect("unsupported option");
            assert!(error.is_unsupported(), "{api} {option}: {error}");
            assert!(host.commands.borrow().is_empty());
        }
    }
}
