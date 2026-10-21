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
    ] {
        let error = evaluate(&format!("import pandas as pd\nfrom io import StringIO\npd.read_csv(StringIO('date\\n2026\\n'), {option})\n"))
            .expect_err("unsupported option");
        assert!(error.is_unsupported(), "{option}: {error}");
    }
    assert!(evaluate("import pandas as pd\nfrom io import StringIO\npd.read_csv(StringIO('date\\n2026\\n'), parse_dates=False, chunksize=None, iterator=False)\n").is_ok());
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
