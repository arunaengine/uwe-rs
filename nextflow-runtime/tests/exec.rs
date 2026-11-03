//! Checks interpreter execution, files, and dependencies against the Nextflow 26.04.6 exec oracle.
// Copyright (c) 2026 The uwe Contributors
// SPDX-License-Identifier: MIT or Apache-2.0

use std::path::{Path, PathBuf};

use workflow_graph::{DeliveryMode, Launch, Simulator, execute};

#[test]
fn skips_exec_effects() {
    for stored in [false, true] {
        let folder = tempfile::tempdir().unwrap();
        let sentinel = folder.path().join("sentinel");
        let store = folder.path().join("store");
        if stored {
            std::fs::create_dir(&store).unwrap();
            std::fs::write(store.join("result.txt"), "stored").unwrap();
        }
        let source = format!(
            "process P {{\n storeDir '{}'\n output:\n path 'result.txt'\n when:\n {stored}\n exec:\n file('{}').text = 'ran'\n task.workDir.resolve('result.txt').text = 'new'\n}}\nworkflow {{ P() }}",
            store.display(),
            sentinel.display()
        );
        std::fs::write(folder.path().join("main.nf"), source).unwrap();
        let mut driver = nextflow_runtime::driver(&Launch {
            target: folder.path().to_owned(),
            scratch: folder.path().join("scratch"),
            ..Launch::default()
        })
        .unwrap();
        assert!(driver.start().unwrap().is_empty());
        driver.finish().unwrap();
        assert!(!sentinel.exists());
        assert!(!folder.path().join("scratch/nextflow").exists());
        if stored {
            assert_eq!(
                std::fs::read_to_string(store.join("result.txt")).unwrap(),
                "stored"
            );
        }
    }
}

#[test]
fn probes_pure_outputs() {
    for stored in [false, true] {
        let folder = tempfile::tempdir().unwrap();
        let sentinel = folder.path().join("sentinel");
        let store = folder.path().join("store");
        if stored {
            std::fs::create_dir(&store).unwrap();
            std::fs::write(store.join("2.txt"), "stored").unwrap();
        }
        let pattern = if stored { "${x}.txt" } else { "${x++}.txt" };
        let source = format!(
            "process P {{\n storeDir '{}'\n input:\n val x\n output:\n path \"{pattern}\"\n when:\n x++ > 0\n exec:\n file('{}').text = \"$x\"\n task.workDir.resolve(\"${{x}}.txt\").text = 'new'\n}}\nworkflow {{ P(1) }}",
            store.display(),
            sentinel.display()
        );
        std::fs::write(folder.path().join("main.nf"), source).unwrap();
        let mut driver = nextflow_runtime::driver(&Launch {
            target: folder.path().to_owned(),
            scratch: folder.path().join("scratch"),
            ..Launch::default()
        })
        .unwrap();
        let tasks = driver.start().unwrap();
        assert_eq!(tasks.len(), usize::from(!stored));
        for task in tasks {
            assert!(
                driver
                    .complete(workflow_graph::Outcome {
                        id: task.id,
                        ..Default::default()
                    })
                    .unwrap()
                    .is_empty()
            );
        }
        driver.finish().unwrap();
        if stored {
            assert!(!sentinel.exists());
        } else {
            assert_eq!(std::fs::read_to_string(sentinel).unwrap(), "2");
        }
        assert_eq!(
            std::fs::read_to_string(store.join("2.txt")).unwrap(),
            if stored { "stored" } else { "new" }
        );
    }
}

/// A file of the run's folder is published as a copy, since the folder goes with the driver.
#[test]
fn copies_run_files() {
    let folder = tempfile::tempdir().unwrap();
    let source = "process P {\n publishDir 'results'\n output:\n path 'made.txt'\n exec:\n task.workDir.resolve('made.txt').text = 'made'\n}\nworkflow { P() }";
    std::fs::write(folder.path().join("main.nf"), source).unwrap();
    let scratch = folder.path().join("scratch");
    let mut driver = nextflow_runtime::driver(&Launch {
        target: folder.path().to_owned(),
        scratch: scratch.clone(),
        ..Launch::default()
    })
    .unwrap();
    for task in driver.start().unwrap() {
        let outcome = workflow_graph::Outcome {
            id: task.id,
            ..Default::default()
        };
        assert!(driver.complete(outcome).unwrap().is_empty());
    }
    driver.finish().unwrap();
    let deliveries = driver.deliveries();
    assert_eq!(deliveries.len(), 1);
    let made = Path::new(&deliveries[0].location);
    assert!(made.starts_with(scratch.join("nextflow")), "{made:?}");
    assert!(matches!(deliveries[0].mode, DeliveryMode::Copy));
    assert_eq!(std::fs::read_to_string(made).unwrap(), "made");
    drop(driver);
    assert!(!made.exists());
}

/// The files below a folder, by their paths there.
fn files(folder: &Path) -> Vec<PathBuf> {
    let mut found = Vec::new();
    let mut pending = vec![folder.to_path_buf()];
    while let Some(current) = pending.pop() {
        for entry in std::fs::read_dir(&current).into_iter().flatten().flatten() {
            let path = entry.path();
            if path.is_dir() {
                pending.push(path);
            } else {
                found.push(path);
            }
        }
    }
    found
}

#[test]
fn interprets_exec_sections() {
    let fixture = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/exec");
    let directory = tempfile::tempdir().expect("a temporary directory");
    std::fs::copy(fixture.join("main.nf"), directory.path().join("main.nf")).expect("copies");
    let scratch = directory.path().join("scratch");
    let launch = Launch {
        target: directory.path().to_owned(),
        scratch: scratch.clone(),
        simulated: true,
        ..Launch::default()
    };
    let mut driver = nextflow_runtime::driver(&launch).expect("a driver");
    let mut executor = Simulator::new(directory.path().join("simulator"));
    let graph = execute("main.nf", driver.as_mut(), &mut executor)
        .into_result()
        .expect("the run succeeds");
    graph.check().expect("a valid graph");
    let tasks: Vec<_> = graph.nodes.iter().map(|node| &node.task).collect();
    let steps = |step: &'static str| tasks.iter().filter(move |task| task.step == step);
    assert_eq!(steps("MAKE_SHEET").count(), 2);
    for task in steps("MAKE_SHEET").chain(steps("CHECK")) {
        assert!(task.image.is_none() && task.command.is_empty(), "{task:?}");
    }
    // A task that reads what an `exec:` section made waits for it.
    for task in steps("COUNT").chain(steps("CHECK")) {
        assert_eq!(task.after.len(), 1, "{task:?}");
        let maker = tasks.iter().find(|other| other.id == task.after[0]);
        assert_eq!(maker.map(|maker| maker.step.as_str()), Some("MAKE_SHEET"));
    }
    // The sections made the files that Nextflow publishes.
    let made = files(&scratch.join("nextflow"));
    for published in [
        "sheets/A.csv",
        "sheets/BB.csv",
        "checks/A.ok",
        "checks/BB.ok",
    ] {
        let recorded = fixture.join("nextflow-26.04.6/results").join(published);
        let name = recorded.file_name().expect("a name");
        let file = (made.iter())
            .find(|path| path.file_name() == Some(name))
            .unwrap_or_else(|| panic!("no {published} made"));
        assert_eq!(
            std::fs::read_to_string(file).expect("reads"),
            std::fs::read_to_string(&recorded).expect("reads the record"),
            "{published}"
        );
    }
}
