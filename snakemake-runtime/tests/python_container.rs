//! Tests external Python packages in containers when UWE_CONTAINER_TESTS=1; requires network.
// Copyright (c) 2026 The uwe Contributors
// SPDX-License-Identifier: MIT or Apache-2.0

use std::rc::Rc;

use python_evaluator::{Interpreter, LocalHost};
use snakemake_runtime::{ContainerStarter, PythonContainers};

#[test]
#[ignore = "needs a container engine and network; set UWE_CONTAINER_TESTS=1 and run with --ignored"]
fn executes_container_packages() {
    assert_eq!(
        std::env::var("UWE_CONTAINER_TESTS").as_deref(),
        Ok("1"),
        "set UWE_CONTAINER_TESTS=1 to run code in a container"
    );
    let folder = tempfile::tempdir().expect("a temporary directory");
    std::fs::write(folder.path().join("data.fa"), ">a\nACGT\n>b\nGG\n").expect("writes");
    let host = Rc::new(LocalHost::new(folder.path()));
    let mut interpreter = Interpreter::new(host.clone());
    let settings = PythonContainers {
        engine: std::env::var("UWE_ENGINE").unwrap_or_else(|_| "docker".into()),
        image: None,
        cache: folder.path().join("python-cache"),
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
