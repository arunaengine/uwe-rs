//! Makes shell task outputs transferable as FILE objects with a layout manifest.
// Copyright (c) 2026 The uwe Contributors
// SPDX-License-Identifier: MIT or Apache-2.0

use executor::{ExecutorError, Job, wrapper};

use crate::backend::{CONTROL, HELPER, HOME, OUTBOX};
use crate::staging::{container_path, unsupported};

pub(crate) const TRANSFER: &str = "/uwe/transfer";

pub(crate) fn discover(
    script: String,
    job: Job<'_>,
    work: &str,
    settings: &crate::Settings,
) -> Result<String, ExecutorError> {
    let Some(mut discovery) = executor::manifest::Discovery::of(job.spec, job.staging) else {
        return Ok(script);
    };
    if [CONTROL, OUTBOX, TRANSFER].iter().any(|path| {
        std::path::Path::new(work).starts_with(path) || std::path::Path::new(path).starts_with(work)
    }) {
        return Err(unsupported("TES workdir overlaps output capture folders"));
    }
    let helper = settings.helper_file.as_ref().ok_or_else(|| {
        unsupported("TES output manifests require a static uwe binary configured with helper_file")
    })?;
    discovery.work = Some(work.into());
    let config = serde_json::to_string(&discovery).map_err(unsupported)?;
    let guard = format!(
        "[ \"$(uname -m)\" = {} ] && echo {}'  '{} | sha256sum -c --status - && chmod +x {} || \
         {{ echo 'uwe: TES helper architecture or digest mismatch' >&2; exit 1; }}\n",
        wrapper::quote(helper.machine.uname()),
        wrapper::quote(helper.digest.hex()),
        wrapper::quote(HELPER),
        wrapper::quote(HELPER),
    );
    let script = script.replacen("#!/bin/sh\n", &format!("#!/bin/sh\n{guard}"), 1);
    let ending = if script.ends_with("exit \"$status\"\n") {
        "exit \"$status\"\n"
    } else {
        "exit 0\n"
    };
    let script = script
        .strip_suffix(ending)
        .ok_or_else(|| unsupported("invalid TES wrapper"))?;
    Ok(format!(
        "{script}if [ \"$status\" -eq 0 ]; then\n    {} __stage discover /uwe {} || \
         {{ echo 1 > \"$control/exit\"; exit 1; }}\nfi\n{ending}",
        wrapper::quote(HELPER),
        wrapper::quote(&config),
    ))
}

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
            || job.spec.output_manifest.is_some() && std::path::Path::new(&path).starts_with(HELPER)
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
                output_manifest: None,
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
