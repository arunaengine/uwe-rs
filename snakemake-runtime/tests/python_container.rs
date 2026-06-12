//! Runs code that imports Python packages without a stand-in in a real container (decision
//! 19). It needs a container engine and the network, so it runs only when
//! `IUNCTUS_CONTAINER_TESTS=1`; `IUNCTUS_ENGINE` names the engine (`docker` by default).
// Copyright (c) 2026 The iunctus Contributors
// SPDX-License-Identifier: MIT or Apache-2.0

use std::rc::Rc;

use python_evaluator::{Interpreter, LocalHost};
use snakemake_runtime::{ContainerStarter, PythonContainers};

#[test]
fn packages_run_in_a_container() {
    if std::env::var_os("IUNCTUS_CONTAINER_TESTS").is_none() {
        eprintln!("skipped: set IUNCTUS_CONTAINER_TESTS=1 to run code in a container");
        return;
    }
    let folder = tempfile::tempdir().expect("a temporary directory");
    std::fs::write(folder.path().join("data.fa"), ">a\nACGT\n>b\nGG\n").expect("writes");
    let host = Rc::new(LocalHost::new(folder.path()));
    let mut interpreter = Interpreter::new(host.clone());
    let settings = PythonContainers {
        engine: std::env::var("IUNCTUS_ENGINE").unwrap_or_else(|_| "docker".into()),
        image: None,
        package_cache: Some(folder.path().join("conda-packages")),
        show_output: false,
    };
    interpreter
        .packages
        .set_starter(Rc::new(ContainerStarter::new(
            settings,
            vec![folder.path().to_path_buf()],
        )));
    let code = "\
from Bio import SeqIO
from Bio.Seq import Seq
records = list(SeqIO.parse('data.fa', 'fasta'))
print([record.id for record in records], len(records[0].seq), records[1].seq == 'GG')
print(str(Seq('AACG').reverse_complement()))
try:
    next(SeqIO.parse('missing.fa', 'fasta'))
except FileNotFoundError:
    print('missing')
try:
    import surely_no_such_package_of_iunctus
except ImportError:
    print('fallback')
";
    if let Err(error) = interpreter.run_text("<test>", code) {
        panic!("{error}");
    }
    assert_eq!(
        host.output.borrow().as_str(),
        "['a', 'b'] 4 True\nCGTT\nmissing\nfallback\n"
    );
    interpreter.packages.stop();
    assert!(!interpreter.packages.is_running());
}
