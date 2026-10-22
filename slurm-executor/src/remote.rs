//! The job script of an attempt on a node that shares no folders with this machine, which stages
//! the files of the task with the uwe helper.
// Copyright (c) 2026 The uwe Contributors
// SPDX-License-Identifier: MIT or Apache-2.0

use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};

use executor::ContainerRun;
use executor::binary::Binary;
use executor::helper::Request;
use executor::remote::RemoteStage;
use executor::wrapper::quote;

use crate::script::{
    CONTAINER_EXIT, Container, ENGINE_EXIT, LOG, NOT_SEEN, clock, command, engine_finish,
    engine_guard, engine_launch, timed_out, timeout,
};

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

/// Where a job finds the static uwe binary that stages the files.
#[derive(Clone, Debug)]
pub enum Helper {
    /// A path that exists on every node.
    Path(String),
    /// An address that the job downloads with `curl` or `wget`, and the SHA-256 of the file.
    Download { url: String, sha256: String },
    /// A binary of this machine, which uwe uploads to the staging root, and which the job
    /// downloads from there like [`Helper::Download`].
    Upload(Binary),
}

/// The records that an attempt sends back: those of the wrapper, then the log of the job, and
/// last the exit code of the container, which tells this machine that the rest is there.
pub fn records() -> Vec<&'static str> {
    let mut records = executor::capture::RECORDS.to_vec();
    records.extend([LOG, ENGINE_EXIT, CONTAINER_EXIT]);
    records
}

/// Where a job reaches the staging root without the helper.
#[derive(Clone, Copy, Debug)]
pub struct Links<'a> {
    /// The plan of the helper.
    pub plan: &'a str,
    /// Where the job downloads the binary of [`Helper::Upload`].
    pub binary: Option<&'a str>,
    /// The upload of the log of the job, when the helper did not send it.
    pub log: Option<&'a Request>,
    pub engine: Option<&'a Request>,
}

/// The job script that runs `run` with its folder on the node instead of at its scratch
/// directory, between stage-in and stage-out. On every exit it removes its folder, after it
/// sent its log back, or else kept it beside the folder.
pub fn job_script(
    container: &Container,
    run: &ContainerRun<'_>,
    staging: &NodeStaging,
    gpus: bool,
    time: Option<u64>,
    links: Links<'_>,
) -> String {
    let base = match &staging.folder {
        Some(folder) => quote(folder),
        None => "\"${TMPDIR:-/tmp}\"".to_owned(),
    };
    let name = run.root.file_name().unwrap_or_default().to_string_lossy();
    // The folder of the job is new and private: it keeps the helper, the plan and the log, which
    // run or count after the task. Only its `data` folder is the folder of the task.
    let keep = format!("mv -f \"$node/{LOG}\" \"$node.log\"");
    let (send, early) = match links.log {
        Some(request) => (
            format!("{} || {keep}", send_log(request, false)),
            format!("    {}\n", send_log(request, true)),
        ),
        None => (keep, String::new()),
    };
    let cleanup = match container {
        Container::Engine(_) => {
            "if declare -F engine_clean > /dev/null; then engine_clean || return 1; fi\n    "
        }
        _ => "",
    };
    // Without its folder the job has no log yet, as SLURM writes its output nowhere, so it
    // sends the reason alone. A signal ends the script through its exit trap, which ignores
    // more of them and removes the data of the task first, as a SIGKILL may cut it short.
    let mut script = format!(
        "#!/bin/bash\n# The uwe job of one attempt of a task, on a node without the shared \
         folders.\numask 077\nif ! node=$(mkdir -p {base} 2>&1 && mktemp -d {base}/{} 2>&1); \
         then\n    early=\"uwe: cannot make a private folder in \"{base}\" on $(hostname): \
         $node\"\n{early}    exit 1\nfi\n\
         exec >> \"$node/{LOG}\" 2>&1\nsent=\nfinish() {{\n    trap '' TERM INT HUP\n    \
         {cleanup}\
         rm -rf \"$node/data\"\n    [ -n \"$sent\" ] || {send}\n    rm -rf \"$node\"\n}}\n\
         trap finish EXIT\ntrap 'exit 143' TERM INT HUP\n",
        quote(&format!("uwe-{name}.XXXXXXXX"))
    );
    let download = match &staging.helper {
        Helper::Path(path) => {
            script.push_str(&format!("helper={}\n", quote(path)));
            None
        }
        Helper::Download { url, sha256 } => Some((url.as_str(), sha256.as_str(), url.as_str())),
        Helper::Upload(uploaded) => {
            // The nodes may differ from this machine, and the binary runs only on its own.
            let machine = uploaded.machine.uname();
            script.push_str(&format!(
                "[ \"$(uname -m)\" = {machine} ] || {{ echo \"uwe: the helper runs on \
                 {machine}, but $(hostname) is $(uname -m); name a binary for it with \
                 helper_file\" >&2; exit 1; }}\n"
            ));
            let url = links.binary.expect("the address of the uploaded binary");
            Some((url, uploaded.digest.hex(), "the staging root"))
        }
    };
    if let Some((url, sha256, address)) = download {
        script.push_str(&format!(
            "helper=\"$node/uwe\"\n\
             {{ curl -fsSL -o \"$helper\" {url} || wget -q -O \"$helper\" {url}; }} && \
             echo {sha256}'  '\"$helper\" | sha256sum -c --status - && chmod +x \"$helper\" || \
             {{ echo \"uwe: cannot get the helper from {address} on $(hostname)\" >&2; \
             exit 1; }}\n",
            url = quote(url),
            sha256 = quote(sha256),
            address = address.replace(['"', '$', '`', '\\'], ""),
        ));
    }
    // Other users may not change the files of the task, unless its container runs as one.
    if run.access.hand_back.is_some() {
        script.push_str("umask 000\n");
    }
    let words = node_words(&command(container, run, gpus), &run.root.to_string_lossy());
    let publish = links
        .engine
        .map(|request| {
            send_log(request, false).replace(
                &format!("\"$node/{LOG}\""),
                &format!("\"$node/{ENGINE_EXIT}\""),
            )
        })
        .unwrap_or_default();
    let guard = engine_guard(
        container,
        &[run.name],
        &format!("\"$node/{ENGINE_EXIT}\""),
        &publish,
    )
    .replace("trap engine_clean EXIT\n", "");
    script.push_str(&format!(
        "status={NOT_SEEN}\nif \"$helper\" __stage in \"$node/data\" {} \"$node\"; then\n    \
         {}{}{}{}\n    status=$?\n    {}{}fi\necho \"$status\" > \"$node/{CONTAINER_EXIT}\"\n\
         \"$helper\" __stage out \"$node/data\" \"$node\" || exit 1\nsent=1\n",
        quote(links.plan),
        guard,
        clock(time).replace('\n', "\n    "),
        timeout(time),
        engine_launch(container, words.join(" "), 0),
        timed_out(time),
        engine_finish(container),
    ));
    script
}

/// The command that uploads the log of the job with `request`, with `curl` or `wget`, or copies
/// it for a staging root in a folder. An `early` log is the text of `$early` instead.
fn send_log(request: &Request, early: bool) -> String {
    let log = format!("\"$node/{LOG}\"");
    if let Some(path) = executor::helper::local_file(&request.url) {
        let folder = path
            .parent()
            .unwrap_or(&path)
            .to_string_lossy()
            .into_owned();
        let (folder, path) = (quote(&folder), quote(&path.to_string_lossy()));
        return match early {
            true => format!("{{ mkdir -p {folder} && echo \"$early\" > {path}; }}"),
            false => format!("{{ mkdir -p {folder} && cp {log} {path}; }}"),
        };
    }
    let headers = |option: &str| -> String {
        (request.headers.iter())
            .map(|(name, value)| format!(" {option}{}", quote(&format!("{name}: {value}"))))
            .collect()
    };
    let (file, body) = match early {
        true => ("-".to_owned(), "--body-data=\"$early\"".to_owned()),
        false => (log.clone(), format!("--body-file={log}")),
    };
    let pipe = if early { "echo \"$early\" | " } else { "" };
    format!(
        "{{ {pipe}curl -fsS --max-time 60 -X PUT -T {file}{} {url} || wget -q -O /dev/null \
         --timeout=30 --tries=1 --method=PUT {body}{} {url}; }}",
        headers("-H "),
        headers("--header="),
        url = quote(&request.url),
    )
}

/// The words of a command as shell words, with the folders of binds and the image archive that
/// are in the scratch directory `root` taken from the folder of the node.
fn node_words(command: &[String], root: &str) -> Vec<String> {
    let mut words = Vec::new();
    let archive = format!("docker-archive:{root}/");
    for (index, word) in command.iter().enumerate() {
        let bind = index > 0 && matches!(command[index - 1].as_str(), "--bind" | "--volume");
        if let Some(rest) = word.strip_prefix(&archive) {
            words.push(format!(
                "docker-archive:\"$node/data\"{}",
                quote(&format!("/{rest}"))
            ));
            continue;
        }
        match word.strip_prefix(root) {
            Some(rest) if bind && (rest.starts_with('/') || rest.starts_with(':')) => {
                words.push(format!("\"$node/data\"{}", quote(rest)))
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
            name: "uwe-run-P-1-1",
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
            cleanup: false,
        };
        let mut staging = NodeStaging {
            stage,
            helper: Helper::Path("/opt/uwe".into()),
            folder: None,
            secret_file: None,
        };
        let apptainer = Container::Apptainer("apptainer".into());
        let links = Links {
            plan: "https://plan",
            binary: None,
            log: None,
            engine: None,
        };
        let script = job_script(&apptainer, &run, &staging, false, Some(60), links);
        let expected = "umask 077\nif ! node=$(mkdir -p \"${TMPDIR:-/tmp}\" 2>&1 && \
            mktemp -d \"${TMPDIR:-/tmp}\"/'uwe-run-P-1-1.XXXXXXXX' 2>&1); then\n    \
            early=\"uwe: cannot make a private folder in \"\"${TMPDIR:-/tmp}\"\" on \
            $(hostname): $node\"\n    exit 1\nfi\n\
            exec >> \"$node/slurm.log\" 2>&1\nsent=\nfinish() {\n    trap '' TERM INT HUP\n    \
            rm -rf \"$node/data\"\n    \
            [ -n \"$sent\" ] || mv -f \"$node/slurm.log\" \"$node.log\"\n    rm -rf \"$node\"\n}\n\
            trap finish EXIT\ntrap 'exit 143' TERM INT HUP\nhelper='/opt/uwe'\nstatus=125\n\
            if \"$helper\" __stage in \"$node/data\" 'https://plan' \"$node\"; then\n    \
            SECONDS=0\n    timeout --kill-after=10 60 'apptainer' 'exec' '--containall' '--pwd' '/data' \
            '--bind' \"$node/data\"':/shared/scratch/run-P-1-1' \
            '--bind' \"$node/data\"'/work:/data' \
            '--bind' \"$node/data\"'/mounts/opt/data:/opt/data' 'docker://debian@sha256:ab' \
            'sh' '/shared/scratch/run-P-1-1/control/wrapper.sh'\n    status=$?\n    \
            [ $status -eq 137 ] && [ $SECONDS -ge 60 ] && status=124\nfi\n\
            echo \"$status\" > \"$node/container-exit\"\n\
            \"$helper\" __stage out \"$node/data\" \"$node\" || exit 1\nsent=1\n";
        assert!(script.ends_with(expected), "{script}");
        staging.helper = Helper::Download {
            url: "https://example.org/uwe".into(),
            sha256: "ab".repeat(32),
        };
        staging.folder = Some("/local".into());
        let docker = Container::Engine(EngineCommand::docker("docker"));
        let log = Request {
            method: "PUT".into(),
            url: "https://staging/log?signature".into(),
            headers: vec![("x-ms-blob-type".into(), "BlockBlob".into())],
        };
        let links = Links {
            log: Some(&log),
            ..links
        };
        let script = job_script(&docker, &run, &staging, false, None, links);
        let expected = "[ -n \"$sent\" ] || { curl -fsS --max-time 60 -X PUT -T \
            \"$node/slurm.log\" -H 'x-ms-blob-type: BlockBlob' 'https://staging/log?signature' || \
            wget -q -O /dev/null --timeout=30 --tries=1 --method=PUT --body-file=\"$node/slurm.log\" \
            --header='x-ms-blob-type: BlockBlob' 'https://staging/log?signature'; } || \
            mv -f \"$node/slurm.log\" \"$node.log\"\n";
        assert!(script.contains(expected), "{script}");
        // A job without its folder sends the reason alone.
        let expected = "    { echo \"$early\" | curl -fsS --max-time 60 -X PUT -T - \
            -H 'x-ms-blob-type: BlockBlob' 'https://staging/log?signature' || wget -q -O /dev/null \
            --timeout=30 --tries=1 --method=PUT --body-data=\"$early\" --header='x-ms-blob-type: BlockBlob' \
            'https://staging/log?signature'; }\n    exit 1\n";
        assert!(script.contains(expected), "{script}");
        assert!(
            script.contains("mktemp -d '/local'/'uwe-run-P-1-1.XXXXXXXX' 2>&1); then"),
            "{script}"
        );
        assert!(
            script.contains("curl -fsSL -o \"$helper\" 'https://example.org/uwe' || wget"),
            "{script}"
        );
        assert!(script.contains("sha256sum -c --status -"), "{script}");
        assert!(
            script.contains("'--volume' \"$node/data\"'/work:/data'"),
            "{script}"
        );
        staging.helper = Helper::Upload(Binary {
            file: "/usr/bin/uwe".into(),
            digest: task_model::Digest::sha256(b"uwe"),
            machine: executor::binary::Machine::Arm64,
            uploaded: Default::default(),
        });
        let links = Links {
            binary: Some("https://staging/cas/x?signature"),
            ..links
        };
        let script = job_script(&docker, &run, &staging, false, None, links);
        let expected = "[ \"$(uname -m)\" = aarch64 ] || { echo \"uwe: the helper runs on \
            aarch64, but $(hostname) is $(uname -m); name a binary for it with helper_file\" \
            >&2; exit 1; }\nhelper=\"$node/uwe\"\n{ curl -fsSL -o \"$helper\" \
            'https://staging/cas/x?signature' || wget";
        assert!(script.contains(expected), "{script}");
        let digest = task_model::Digest::sha256(b"uwe");
        assert!(
            script.contains(&format!("echo '{}'", digest.hex())),
            "{script}"
        );
        assert!(script.contains("from the staging root on"), "{script}");
        // A task of another user may change its data, but never the helper.
        let access = Access {
            hand_back: Some((0, 0)),
            ..Access::default()
        };
        let run = ContainerRun {
            access: &access,
            ..run
        };
        let script = job_script(&docker, &run, &staging, false, None, links);
        let open = script.find("umask 000\n").expect("an open umask");
        assert!(
            open > script.find("chmod +x").expect("the helper"),
            "{script}"
        );
    }
}
