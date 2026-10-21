//! Checks ShmSize aliases, units, expression evaluation and required remote refusal.
// Copyright (c) 2026 The uwe Contributors
// SPDX-License-Identifier: MIT or Apache-2.0

use cwl_runtime::{CwlDriver, Options};
use workflow_graph::{Driver, Launch, Task};

fn task(class: &str, field: &str, options: Options) -> Result<Task, String> {
    let root = tempfile::tempdir().unwrap();
    std::fs::write(
        root.path().join("main.cwl"),
        format!(
            "cwlVersion: v1.2\nclass: CommandLineTool\nbaseCommand: echo\n\
         $namespaces: {{cwltool: 'http://commonwl.org/cwltool#'}}\n\
         requirements:\n  {class}: {{{field}}}\n\
         inputs: {{size: {{type: string, default: '128m'}}}}\noutputs: []\n"
        ),
    )
    .unwrap();
    let mut driver = CwlDriver::new(
        &Launch {
            target: root.path().join("main.cwl"),
            ..Launch::default()
        },
        options,
    )
    .map_err(|error| error.message)?;
    Ok(driver.start().map_err(|error| error.message)?.remove(0))
}

#[test]
fn sets_shm_size() {
    for class in ["ShmSize", "cwltool:ShmSize"] {
        for (field, bytes) in [
            ("", 64 << 20),
            ("shmSize: '1g'", 1 << 30),
            ("shmSize: '2K'", 2048),
            ("shmSize: '1024'", 1024),
            ("shmSize: '1.5m'", 1572864),
            ("shmSize: $(inputs.size)", 128 << 20),
        ] {
            let task = task(class, field, Options::default()).unwrap();
            let flags: serde_json::Value =
                serde_json::from_str(&task.environment["IUNCTUS_CONTAINER_FLAGS"]).unwrap();
            assert_eq!(flags["shm"], bytes);
        }
    }
}

#[test]
fn rejects_bad_sizes() {
    for value in [
        "'0'",
        "'-1g'",
        "'1t'",
        "'NaNg'",
        "'1e309g'",
        "'99999999999g'",
        "1024",
    ] {
        let error = task("ShmSize", &format!("shmSize: {value}"), Options::default()).unwrap_err();
        assert!(error.contains("ShmSize"), "{value}: {error}");
    }
}

#[test]
fn refuses_remote_shm() {
    let error = task(
        "ShmSize",
        "shmSize: '64m'",
        Options {
            simulated: false,
            ..Options::default()
        },
    )
    .unwrap_err();
    assert!(error.contains("local executor"), "{error}");
}
