//! The job script of an attempt on a node that shares no folders with this machine, which stages
//! the files of the task with the iunctus helper.
// Copyright (c) 2026 The iunctus Contributors
// SPDX-License-Identifier: MIT or Apache-2.0

use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};

use executor::remote::RemoteStage;
use executor::wrapper::quote;
use executor::{CONTROL, ContainerRun};

use crate::script::{CONTAINER_EXIT, Container, LOG, NOT_SEEN, command, timed_out, timeout};

/// How the jobs of a backend get their files when the nodes share no folders with this machine.
#[derive(Clone, Debug)]
pub struct NodeStaging {
    pub stage: RemoteStage,
    pub helper: Helper,
    /// The folder of the nodes where each job makes the folder of its attempt; `None` for
    /// `$TMPDIR`, else `/tmp`.
    pub folder: Option<String>,
    /// For nodes with credentials: the file of the storage providers of the staging root, which
    /// only its owner may read. Each job gets its text in the environment, never in its script.
    pub secret_file: Option<PathBuf>,
}

/// The text of the file of the storage providers, which only its owner may read.
pub fn secret_text(file: &Path) -> Result<String, String> {
    let unreadable = |error: std::io::Error| format!("cannot read {}: {error}", file.display());
    let metadata = std::fs::metadata(file).map_err(unreadable)?;
    if metadata.permissions().mode() & 0o077 != 0 {
        return Err(format!(
            "other users may read {}, which holds secrets; make it private with chmod 600",
            file.display()
        ));
    }
    std::fs::read_to_string(file).map_err(unreadable)
}

/// Where a job finds the static iunctus binary that stages the files.
#[derive(Clone, Debug)]
pub enum Helper {
    /// A path that exists on every node.
    Path(String),
    /// An address that the job downloads with `curl` or `wget`, and the SHA-256 of the file.
    Download { url: String, sha256: String },
}

/// The records that an attempt sends back: those of the wrapper, then the log of the job, and
/// last the exit code of the container, which tells this machine that the rest is there.
pub fn records() -> Vec<&'static str> {
    let mut records = executor::capture::RECORDS.to_vec();
    records.extend([LOG, CONTAINER_EXIT]);
    records
}

/// The job script that runs `run` with its folder on the node instead of at its scratch
/// directory, between stage-in from the plan at `plan` and stage-out.
pub fn job_script(
    container: &Container,
    run: &ContainerRun<'_>,
    staging: &NodeStaging,
    gpus: bool,
    time: Option<u64>,
    plan: &str,
) -> String {
    let base = match &staging.folder {
        Some(folder) => quote(folder),
        None => "\"${TMPDIR:-/tmp}\"".to_owned(),
    };
    let name = run.root.file_name().unwrap_or_default().to_string_lossy();
    // Other users may not read the files of the task, unless its container runs as one.
    let umask = match run.access.hand_back {
        Some(_) => "000",
        None => "077",
    };
    let mut script = format!(
        "#!/bin/bash\n# The iunctus job of one attempt of a task, on a node without the shared \
         folders.\numask {umask}\nnode={base}/{}\nmkdir -p \"$node/{CONTROL}\" || exit 1\n\
         exec >> \"$node/{CONTROL}/{LOG}\" 2>&1\n",
        quote(&format!("iunctus-{name}"))
    );
    match &staging.helper {
        Helper::Path(path) => script.push_str(&format!("helper={}\n", quote(path))),
        Helper::Download { url, sha256 } => script.push_str(&format!(
            "helper=\"$node/iunctus\"\n\
             {{ curl -fsSL -o \"$helper\" {url} || wget -q -O \"$helper\" {url}; }} && \
             echo {sha256}'  '\"$helper\" | sha256sum -c --status - && chmod +x \"$helper\" || \
             {{ echo \"iunctus: cannot get the helper from {address} on $(hostname)\" >&2; \
             exit 1; }}\n",
            url = quote(url),
            sha256 = quote(sha256),
            address = url.replace(['"', '$', '`', '\\'], ""),
        )),
    }
    let words = node_words(&command(container, run, gpus), &run.root.to_string_lossy());
    script.push_str(&format!(
        "status={NOT_SEEN}\nif \"$helper\" __stage in \"$node\" {}; then\n    {}{}\n    \
         status=$?\n    {}fi\necho \"$status\" > \"$node/{CONTROL}/{CONTAINER_EXIT}\"\n\
         \"$helper\" __stage out \"$node\" || exit 1\nrm -rf \"$node\"\n",
        quote(plan),
        timeout(time),
        words.join(" "),
        timed_out(time),
    ));
    script
}

/// The words of a command as shell words, with the folders of binds that are in the scratch
/// directory `root` taken from the folder of the node.
fn node_words(command: &[String], root: &str) -> Vec<String> {
    let mut words = Vec::new();
    for (index, word) in command.iter().enumerate() {
        let bind = index > 0 && matches!(command[index - 1].as_str(), "--bind" | "--volume");
        match word.strip_prefix(root) {
            Some(rest) if bind && (rest.starts_with('/') || rest.starts_with(':')) => {
                words.push(format!("\"$node\"{}", quote(rest)))
            }
            _ => words.push(quote(word)),
        }
    }
    words
}

#[cfg(test)]
mod tests {
    use std::path::Path;

    use executor::process::Mount;
    use executor::{Access, EngineCommand};
    use task_model::Image;

    use super::*;

    #[test]
    fn stages_through_node() {
        let image = Image {
            reference: "debian@sha256:ab".into(),
            platform: None,
        };
        let mounts = [Mount {
            source: "/shared/scratch/run-P-1-1/mounts/opt/data".into(),
            target: "/opt/data".into(),
            read_only: false,
        }];
        let access = Access::default();
        let run = ContainerRun {
            name: "iunctus-run-P-1-1",
            image: &image,
            root: Path::new("/shared/scratch/run-P-1-1"),
            workdir: Some("/data"),
            mounts: &mounts,
            owner: (1000, 100),
            cpus: 1,
            memory: None,
            offline: false,
            wrapper_path: None,
            access: &access,
        };
        let stage = RemoteStage {
            objects: content_store::Store::memory().unwrap(),
            expiry: std::time::Duration::from_secs(60),
            max_outputs: 1,
            credentials: false,
        };
        let mut staging = NodeStaging {
            stage,
            helper: Helper::Path("/opt/iunctus".into()),
            folder: None,
            secret_file: None,
        };
        let apptainer = Container::Apptainer("apptainer".into());
        let script = job_script(&apptainer, &run, &staging, false, Some(60), "https://plan");
        let expected = "node=\"${TMPDIR:-/tmp}\"/'iunctus-run-P-1-1'\n\
            mkdir -p \"$node/control\" || exit 1\n\
            exec >> \"$node/control/slurm.log\" 2>&1\nhelper='/opt/iunctus'\nstatus=125\n\
            if \"$helper\" __stage in \"$node\" 'https://plan'; then\n    \
            timeout --kill-after=10 60 'apptainer' 'exec' '--containall' '--pwd' '/data' \
            '--bind' \"$node\"':/shared/scratch/run-P-1-1' \
            '--bind' \"$node\"'/work:/data' \
            '--bind' \"$node\"'/mounts/opt/data:/opt/data' 'docker://debian@sha256:ab' 'sh' \
            '/shared/scratch/run-P-1-1/control/wrapper.sh'\n    status=$?\n    \
            [ $status -eq 137 ] && [ $SECONDS -ge 60 ] && status=124\nfi\n\
            echo \"$status\" > \"$node/control/container-exit\"\n\
            \"$helper\" __stage out \"$node\" || exit 1\nrm -rf \"$node\"\n";
        assert!(script.ends_with(expected), "{script}");
        staging.helper = Helper::Download {
            url: "https://example.org/iunctus".into(),
            sha256: "ab".repeat(32),
        };
        staging.folder = Some("/local".into());
        let docker = Container::Engine(EngineCommand::docker("docker"));
        let script = job_script(&docker, &run, &staging, false, None, "https://plan");
        assert!(
            script.contains("umask 077\nnode='/local'/'iunctus-run-P-1-1'\n"),
            "{script}"
        );
        assert!(
            script.contains("curl -fsSL -o \"$helper\" 'https://example.org/iunctus' || wget"),
            "{script}"
        );
        assert!(script.contains("sha256sum -c --status -"), "{script}");
        assert!(
            script.contains("'--volume' \"$node\"'/work:/data'"),
            "{script}"
        );
    }
}
