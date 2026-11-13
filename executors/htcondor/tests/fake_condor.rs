//! Checks the HTCondor backend with local tools that simulate an HTCondor access point.
// Copyright (c) 2026 The uwe Contributors
// SPDX-License-Identifier: MIT or Apache-2.0

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::time::Duration;

use executor::registry::{ImageConfig, ImageConfigs};
use executor::{Backend, ExecutorError, Job, Staged, Staging};
use htcondor_executor::{Condor, HtcondorBackend};
use slurm_executor::Container;
use task_model::{FORMAT, Image, OutputRule, Placement, TaskSpec};
use tokio_util::sync::CancellationToken;

/// Keeps the submit file and runs its executable in the background in `initialdir`, with both
/// streams to `output`. With a file `hold` beside it, the job stays held for that reason.
const CONDOR_SUBMIT: &str = r#"#!/bin/bash
state="$(dirname "$0")/state"
mkdir -p "$state"
file=${@: -1}
exec 9> "$state/ids.lock"
flock -x 9
id=$(( $(cat "$state/last" 2>/dev/null || echo 1000) + 1 ))
echo "$id" > "$state/last"
flock -u 9
cp "$file" "$state/$id.condor"
value() { sed -n "s/^$1 = //p" "$file"; }
if [ -e "$(dirname "$0")/hold" ]; then
    cp "$(dirname "$0")/hold" "$state/$id.held"
else
    (
        cd "$(value initialdir)" && setsid bash -c 'echo $$ > "$1"; exec bash "$2"' job \
            "$state/$id.pid" "$(value executable)" > "$(value output)" 2>&1
        echo $? > "$state/$id.exit"
    ) > /dev/null 2>&1 &
fi
echo "$id.0 - $id.0"
"#;

/// Lists the jobs that have not ended: running, or held with their reason.
const CONDOR_Q: &str = r#"#!/bin/bash
state="$(dirname "$0")/state"
for id in "$@"; do
    case $id in -*) break ;; esac
    cluster=${id%.0}
    [ -e "$state/$cluster.removed" ] && continue
    if [ -e "$state/$cluster.held" ]; then
        echo "$cluster 0 5 $(cat "$state/$cluster.held")"
    elif [ ! -e "$state/$cluster.exit" ]; then
        echo "$cluster 0 2 undefined"
    fi
done
"#;

const CONDOR_HISTORY: &str = r#"#!/bin/bash
state="$(dirname "$0")/state"
cluster=${1%.0}
if [ -e "$state/$cluster.removed" ]; then echo "3 undefined via condor_rm"
elif [ -e "$state/$cluster.exit" ]; then echo "4 $(cat "$state/$cluster.exit") undefined"; fi
"#;

const CONDOR_RM: &str = r#"#!/bin/bash
state="$(dirname "$0")/state"
cluster=${1%.0}
echo "$1" >> "$state/removed"
touch "$state/$cluster.removed"
[ ! -s "$state/$cluster.pid" ] || kill -TERM -- "-$(cat "$state/$cluster.pid")" 2> /dev/null
exit 0
"#;

/// Runs the command after the image in the folder of `--pwd`, without a container.
const APPTAINER: &str = r#"#!/bin/bash
while [ $# -gt 0 ]; do
    case $1 in
        --pwd) cd "$2" || exit 255; shift 2 ;;
        --bind) shift 2 ;;
        exec|--*) shift ;;
        *) shift; break ;;
    esac
done
exec "$@"
"#;

/// Writes the fake tools into `root/bin`.
fn access_point(root: &Path) -> PathBuf {
    let bin = root.join("bin");
    std::fs::create_dir_all(&bin).expect("makes the folder");
    for (name, text) in [
        ("condor_submit", CONDOR_SUBMIT),
        ("condor_q", CONDOR_Q),
        ("condor_history", CONDOR_HISTORY),
        ("condor_rm", CONDOR_RM),
        ("apptainer", APPTAINER),
    ] {
        // A child process writes the command. A process that another test starts meanwhile
        // gets a copy of what this one has open, and a command open for writing cannot run.
        let script = "printf '%s' \"$1\" > \"$2\" && chmod 755 \"$2\"";
        let status = std::process::Command::new("sh")
            .args(["-c", script, "sh", text])
            .arg(bin.join(name))
            .status();
        assert!(status.expect("writes the command").success());
    }
    bin
}

fn backend(root: &Path, bin: &Path) -> HtcondorBackend {
    let condor = Condor {
        commands: Some(bin.to_owned()),
        lines: vec!["+ProjectName = \"lab\"".into()],
    };
    let apptainer = Container::Apptainer(bin.join("apptainer").to_string_lossy().into_owned());
    let mut backend = HtcondorBackend::new(condor, root.join("scratch"), apptainer);
    backend.poll = Duration::from_millis(100);
    // The image of the tests has a known config, so that no test asks a registry.
    let known = [("debian:stable-slim".to_owned(), ImageConfig::default())];
    backend.configs = ImageConfigs::known(known);
    backend
}

fn spec(script: &str) -> TaskSpec {
    TaskSpec {
        run_as: None,
        format: FORMAT,
        namespace: String::new(),
        image: Some(Image {
            reference: "debian:stable-slim".into(),
            platform: None,
        }),
        arguments: vec!["/bin/bash".into(), "-c".into(), script.into()],
        workdir: None,
        environment: BTreeMap::new(),
        inputs: Vec::new(),
        outputs: vec![OutputRule {
            pattern: "out.txt".into(),
        }],
        output_manifest: None,
        stdin: None,
        containers: Vec::new(),
        replaces_entrypoint: false,
    }
}

/// An input `in.txt` with this text.
fn staging(root: &Path, text: &str) -> Staging {
    let source = root.join("source").join("in.txt");
    std::fs::create_dir_all(source.parent().expect("a folder")).expect("makes the folder");
    std::fs::write(&source, text).expect("writes the input");
    Staging {
        inputs: vec![Staged {
            path: "in.txt".into(),
            source,
            ..Default::default()
        }],
        ..Staging::default()
    }
}

#[tokio::test]
async fn runs_shared_jobs() {
    let root = tempfile::tempdir().expect("a folder");
    let bin = access_point(root.path());
    let backend = backend(root.path(), &bin);
    let spec = spec("tr a-z A-Z < in.txt > out.txt");
    let staging = staging(root.path(), "hello\n");
    let mut placement = Placement {
        cpus: 2,
        memory_bytes: Some(4 << 30),
        time_seconds: Some(3600),
        ..Placement::default()
    };
    let options = serde_json::json!("HasCHTCStaging=true; +ProjectName = \"probe\"");
    placement.options.insert("clusterOptions".into(), options);
    let job = Job {
        name: "run-BIG-1-1",
        spec: &spec,
        staging: &staging,
        placement: &placement,
    };
    let finished = (backend.run(job, &CancellationToken::new()).await).expect("the job ends");
    assert_eq!(finished.exit_code, Some(0));
    let output = std::fs::read_to_string(finished.local_path("out.txt")).expect("an output");
    assert_eq!(output, "HELLO\n");
    let submitted = std::fs::read_to_string(bin.join("state/1001.condor")).expect("the file");
    for line in [
        "request_cpus = 2\n",
        "request_memory = 4096\n",
        "should_transfer_files = IF_NEEDED\n",
        "periodic_remove = (RemoteWallClockTime - CumulativeSuspensionTime) > 3900\n",
        "HasCHTCStaging=true\n+ProjectName = \"probe\"\n+ProjectName = \"lab\"\nqueue\n",
    ] {
        assert!(submitted.contains(line), "{line} in {submitted}");
    }
}

#[tokio::test]
async fn removes_held_jobs() {
    let root = tempfile::tempdir().expect("a folder");
    let bin = access_point(root.path());
    std::fs::write(bin.join("hold"), "Error from slot1@node: no disk").expect("writes");
    let backend = backend(root.path(), &bin);
    let spec = spec("true");
    let staging = Staging::default();
    let placement = Placement::default();
    let job = Job {
        name: "run-HELD-1-1",
        spec: &spec,
        staging: &staging,
        placement: &placement,
    };
    let result = backend.run(job, &CancellationToken::new()).await;
    let Err(ExecutorError::Engine { message, .. }) = result else {
        panic!("a held job fails its attempt: {result:?}");
    };
    assert!(message.contains("HTCondor held the job (Error from slot1@node: no disk)"));
    let removed = std::fs::read_to_string(bin.join("state/removed")).expect("a removal");
    assert_eq!(removed, "1001.0\n");
    assert!(!backend.scratch.join("run-HELD-1-1").exists());
}

#[test]
fn reports_missing_tools() {
    let root = tempfile::tempdir().expect("a folder");
    let condor = Condor {
        commands: Some(root.path().to_owned()),
        lines: Vec::new(),
    };
    assert_eq!(condor.missing_tools(), htcondor_executor::TOOLS);
}
