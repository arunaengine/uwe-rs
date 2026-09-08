//! Operator reservations retain their outputs through overlapping runs and later staging.
// Copyright (c) 2026 The iunctus Contributors
// SPDX-License-Identifier: MIT or Apache-2.0

use std::cell::RefCell;
use std::path::PathBuf;
use std::rc::Rc;

use nextflow_evaluator::exception::Exception;
use nextflow_evaluator::interpreter::{Error, Host};
use nextflow_evaluator::lower::lower_module;
use nextflow_evaluator::value::{PathValue, Value};
use nextflow_parser::parse_script;
use nextflow_runtime::driver::{Nextflow, Settings};
use nextflow_runtime::{Options, Run};
use workflow_graph::{Driver, Launch};

struct Disk {
    directory: PathBuf,
    allocations: Rc<RefCell<Vec<PathBuf>>>,
    refuse: bool,
}

impl Host for Disk {
    fn print(&mut self, _: &str) {}

    fn launch_dir(&self) -> String {
        self.directory.to_string_lossy().into_owned()
    }

    fn operator_directory(&mut self) -> Result<PathValue, Error> {
        let directory = if self.refuse {
            return Err(Error::Thrown {
                exception: Rc::new(Exception::new("IOException", "unexpected reservation")),
                span: Default::default(),
            });
        } else {
            tempfile::Builder::new()
                .prefix("reserved-")
                .tempdir_in(&self.directory)
                .map_err(|error| Error::Thrown {
                    exception: Rc::new(Exception::new(
                        "IOException",
                        format!("cannot reserve: {error}"),
                    )),
                    span: Default::default(),
                })?
                .keep()
        };
        self.allocations.borrow_mut().push(directory.clone());
        Ok(PathValue::parse(&directory.to_string_lossy(), "/"))
    }
}

fn module(source: &str) -> Rc<nextflow_evaluator::lowered::Module> {
    let parsed = parse_script(source);
    assert!(parsed.errors.is_empty(), "{:?}", parsed.errors);
    Rc::new(lower_module(&parsed.tree, source).unwrap())
}

#[test]
fn overlapping_runs_keep_collect_split_and_path_split_bytes() {
    let directory = tempfile::tempdir().unwrap();
    let input = directory.path().join("source.txt");
    let source = format!(
        "process P {{\n input:\n path collected\n path split\n path pathSplit\n script:\n 'true'\n}}\nworkflow {{\n def chunks = file('{}').splitText(file: true)\n P(channel.of(params.text).collectFile(name: 'saved.txt', sort: false), channel.of(params.text + '\\n').splitText(file: true), chunks)\n}}\n",
        input.display()
    );
    let module = module(&source);
    let allocations = Rc::new(RefCell::new(Vec::new()));
    let mut host = Disk {
        directory: directory.path().to_owned(),
        allocations: allocations.clone(),
        refuse: false,
    };
    let options = |text: &str| {
        let options = Options::default();
        nextflow_config::tree::assign(
            &options.config.params(),
            &["text".into()],
            Value::string(text),
        )
        .unwrap();
        options
    };
    std::fs::write(&input, "first\n").unwrap();
    let (first, requests) = Run::start(&module, &options("first"), &mut host).unwrap();
    let first_paths: Vec<_> = requests[0]
        .staged
        .iter()
        .map(|(path, _)| path.path.clone())
        .collect();
    assert_eq!(first_paths.len(), 3);
    std::fs::write(&input, "second\n").unwrap();
    let (second, requests) = Run::start(&module, &options("second"), &mut host).unwrap();
    let second_paths: Vec<_> = requests[0]
        .staged
        .iter()
        .map(|(path, _)| path.path.clone())
        .collect();
    assert_eq!(allocations.borrow().len(), 2, "one reservation per Run");
    for (first, second) in first_paths.iter().zip(&second_paths) {
        assert_ne!(first, second);
    }
    drop(first);
    drop(second);
    for (paths, text) in [(&first_paths, "first"), (&second_paths, "second")] {
        let bytes: Vec<_> = paths
            .iter()
            .map(|path| std::fs::read_to_string(path).unwrap())
            .collect();
        assert_eq!(
            bytes,
            [text.to_owned(), format!("{text}\n"), format!("{text}\n")]
        );
    }
}

#[test]
fn explicit_destinations_bypass_operator_reservation() {
    let directory = tempfile::tempdir().unwrap();
    let output = directory.path().join("explicit");
    std::fs::create_dir(&output).unwrap();
    let input = directory.path().join("source.txt");
    std::fs::write(&input, "path\n").unwrap();
    let source = format!(
        "workflow {{\n channel.of('store').collectFile(name: 'store.txt', storeDir: '{}', sort: false).view()\n channel.of('name').collectFile(name: '{}', sort: false).view()\n channel.of('channel\\n').splitText(file: file('{}')).view()\n channel.of('directory\\n').splitText(file: file('{}')).view()\n println(file('{}').splitText(file: file('{}')))\n}}\n",
        output.display(),
        output.join("named.txt").display(),
        output.join("chunk.txt").display(),
        output.display(),
        input.display(),
        output.join("path.txt").display()
    );
    let allocations = Rc::new(RefCell::new(Vec::new()));
    let mut host = Disk {
        directory: directory.path().to_owned(),
        allocations: allocations.clone(),
        refuse: true,
    };
    let (mut run, requests) = Run::start(&module(&source), &Options::default(), &mut host).unwrap();
    assert!(requests.is_empty());
    run.finish(&mut host).unwrap();
    assert!(allocations.borrow().is_empty());
    for (name, expected) in [
        ("store.txt", "store"),
        ("named.txt", "name"),
        ("chunk.1.txt", "channel\n"),
        ("chunk.1", "directory\n"),
        ("path.1.txt", "path\n"),
    ] {
        assert_eq!(
            std::fs::read_to_string(output.join(name)).unwrap(),
            expected
        );
    }
}

#[test]
fn reservation_errors_propagate_without_fallback() {
    let directory = tempfile::tempdir().unwrap();
    let not_a_directory = directory.path().join("file");
    std::fs::write(&not_a_directory, "preserved").unwrap();
    for body in [
        "channel.of('x').collectFile(sort: false)".to_owned(),
        "channel.of('x').splitText(file: true)".to_owned(),
        format!(
            "file('{}').splitText(file: true)",
            not_a_directory.display()
        ),
    ] {
        let source = format!("workflow {{\n {body}\n}}\n");
        let mut host = Disk {
            directory: not_a_directory.clone(),
            allocations: Default::default(),
            refuse: false,
        };
        let error = match Run::start(&module(&source), &Options::default(), &mut host) {
            Err(error) => error,
            Ok(_) => panic!("reservation must fail"),
        };
        assert!(error.to_string().contains("cannot reserve"), "{error}");
    }
    assert_eq!(
        std::fs::read_to_string(not_a_directory).unwrap(),
        "preserved"
    );
}

#[test]
fn custom_storage_reservation_and_writes_stay_with_the_host() {
    struct Virtual {
        allocations: usize,
        files: Vec<(String, Vec<u8>)>,
    }
    impl Host for Virtual {
        fn print(&mut self, _: &str) {}
        fn operator_directory(&mut self) -> Result<PathValue, Error> {
            self.allocations += 1;
            Ok(PathValue::parse(
                &format!("virtual://scratch/{}", self.allocations),
                "/",
            ))
        }
        fn write(&mut self, path: &PathValue, bytes: &[u8]) -> Result<(), Error> {
            self.files.push((path.uri(), bytes.to_vec()));
            Ok(())
        }
    }
    let source = "workflow {\n channel.of('collect').collectFile(sort: false).view()\n channel.of('split\\n').splitText(file: true).view()\n channel.of('named\\n').splitText(file: 'chosen.txt').view()\n}\n";
    let mut host = Virtual {
        allocations: 0,
        files: Vec::new(),
    };
    let (mut run, _) = Run::start(&module(source), &Options::default(), &mut host).unwrap();
    run.finish(&mut host).unwrap();
    assert_eq!(host.allocations, 1);
    assert_eq!(host.files.len(), 3);
    assert!(
        host.files
            .iter()
            .any(|(path, _)| path.ends_with("/chosen.1.txt"))
    );
    assert!(
        host.files
            .iter()
            .all(|(path, _)| path.starts_with("virtual://scratch/1/"))
    );
    let mut contents: Vec<_> = host.files.into_iter().map(|(_, bytes)| bytes).collect();
    contents.sort();
    assert_eq!(
        contents,
        [
            b"collect".to_vec(),
            b"named\n".to_vec(),
            b"split\n".to_vec()
        ]
    );
}

#[test]
fn driver_forwards_custom_reservation_and_keeps_staging_inputs() {
    let directory = tempfile::tempdir().unwrap();
    let source = "process P {\n input:\n path x\n script:\n 'true'\n}\nworkflow {\n P(channel.of('driver').collectFile(sort: false))\n}\n";
    std::fs::write(directory.path().join("main.nf"), source).unwrap();
    let allocations = Rc::new(RefCell::new(Vec::new()));
    let host = Disk {
        directory: directory.path().to_owned(),
        allocations: allocations.clone(),
        refuse: false,
    };
    let mut driver = Nextflow::new(
        &Launch {
            target: directory.path().to_owned(),
            ..Launch::default()
        },
        Settings {
            host: Box::new(host),
            ..Settings::default()
        },
    )
    .unwrap();
    let tasks = driver.start().unwrap();
    assert_eq!(allocations.borrow().len(), 1);
    let source = tasks[0]
        .inputs
        .iter()
        .find(|input| input.content.is_none())
        .unwrap()
        .source
        .clone();
    drop(driver);
    assert_eq!(std::fs::read_to_string(source).unwrap(), "driver");
}
