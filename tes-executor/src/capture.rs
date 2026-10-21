//! Makes shell task outputs transferable as FILE objects with a layout manifest.
// Copyright (c) 2026 The uwe Contributors
// SPDX-License-Identifier: MIT or Apache-2.0

use executor::{ExecutorError, Job, wrapper};

use crate::backend::{CONTROL, HOME, OUTBOX};
use crate::staging::{container_path, unsupported};

pub(crate) const TRANSFER: &str = "/uwe/transfer";

pub(crate) fn script(
    job: Job<'_>,
    work: &str,
    directories: &[String],
    programs: &[String],
) -> Result<String, ExecutorError> {
    for rule in &job.spec.outputs {
        container_path(&rule.pattern, work)?;
    }
    let folders = wrapper::Folders {
        work,
        control: CONTROL,
        home: HOME,
        outbox: OUTBOX,
        owner: "",
    };
    let mut script = wrapper::wrapper(job.spec, &folders);
    let mut prepare = String::new();
    for directory in directories {
        let path = container_path(directory, work)?;
        if super::staging::wildcard(&path)
            || path == super::backend::WRAPPER
            || path == CONTROL
            || path.starts_with(&format!("{CONTROL}/"))
            || path == TRANSFER
            || path.starts_with(&format!("{TRANSFER}/"))
        {
            return Err(unsupported(
                "TES input directories overlap the output capture folders",
            ));
        }
        prepare.push_str(&format!("mkdir -p {} || exit 1\n", wrapper::quote(&path)));
    }
    for program in programs {
        prepare.push_str(&format!("chmod +x {} || exit 1\n", wrapper::quote(program)));
    }
    script = script.replacen("#!/bin/sh\n", &format!("#!/bin/sh\n{prepare}"), 1);
    executor::capture::capture(&script, TRANSFER)
}

#[cfg(test)]
mod tests {
    use super::*;
    use executor::Staging;
    use task_model::{FORMAT, Placement, TaskSpec};

    #[test]
    fn preserves_command_status() {
        let folder = tempfile::tempdir().unwrap();
        for code in [0, 17] {
            let spec = TaskSpec {
                format: FORMAT,
                arguments: vec!["sh".into(), "-c".into(), format!("exit {code}")],
                namespace: String::new(),
                image: None,
                workdir: None,
                environment: Default::default(),
                inputs: Vec::new(),
                outputs: Vec::new(),
                stdin: None,
                containers: Vec::new(),
                replaces_entrypoint: true,
                run_as: None,
            };
            let staging = Staging::default();
            let placement = Placement::default();
            let job = Job {
                name: "capture",
                spec: &spec,
                staging: &staging,
                placement: &placement,
            };
            let script = script(job, "/uwe/work", &[], &[])
                .unwrap()
                .replace("/uwe", folder.path().to_str().unwrap());
            let path = folder.path().join("wrapper.sh");
            std::fs::write(&path, script).unwrap();
            let output = std::process::Command::new("sh")
                .arg(&path)
                .output()
                .unwrap();
            assert_eq!(output.status.code(), Some(code), "{output:?}");
            assert_eq!(
                std::fs::read_to_string(folder.path().join("control/exit")).unwrap(),
                format!("{code}\n")
            );
        }
    }
}
