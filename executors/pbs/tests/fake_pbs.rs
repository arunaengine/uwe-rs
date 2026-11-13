//! Checks the PBS backend with local tools that simulate a PBS server with Gadi's job summary.
// Copyright (c) 2026 The uwe Contributors
// SPDX-License-Identifier: MIT or Apache-2.0

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::time::Duration;

use executor::registry::{ImageConfig, ImageConfigs};
use executor::{Backend, ExecutorError, Job, Staged, Staging};
use pbs_executor::{Pbs, PbsBackend};
use slurm_executor::Container;
use task_model::{FORMAT, Image, OutputRule, Placement, TaskSpec};
use tokio_util::sync::CancellationToken;

/// Keeps the script and its options, runs it in the background with its output to its `#PBS -o`
/// file, appends a resource summary as Gadi does, and prints a Gadi job id. With a file `hold`
/// beside it, the job stays held for that reason.
const QSUB: &str = r#"#!/bin/bash
state="$(dirname "$0")/state"
mkdir -p "$state"
script=${@: -1}
exec 9> "$state/ids.lock"
flock -x 9
id=$(( $(cat "$state/last" 2>/dev/null || echo 140000000) + 1 ))
echo "$id" > "$state/last"
flock -u 9
cp "$script" "$state/$id.sh"
printf '%s\n' "$@" > "$state/$id.options"
if [ -e "$(dirname "$0")/hold" ]; then
    cp "$(dirname "$0")/hold" "$state/$id.held"
else
    log=$(sed -n 's/^#PBS -o //p' "$script")
    (
        setsid bash -c 'echo $$ > "$1"; exec bash "$2"' job "$state/$id.pid" "$script" > "$log" 2>&1
        code=$?
        cat >> "$log" <<EOT
======================================================================================
                  Resource Usage on 2026-11-06 14:10:01:
   Job Id:             $id.gadi-pbs
   Exit Status:        $code
   Service Units:      0.01
======================================================================================
EOT
        echo $code > "$state/$id.exit"
    ) > /dev/null 2>&1 &
fi
echo "$id.gadi-pbs"
"#;

/// Answers in the JSON of `qstat -f -F json`; jobs that ended stay as finished, as with job
/// history.
const QSTAT: &str = r#"#!/bin/bash
state="$(dirname "$0")/state"
printf '{"pbs_version": "2024.1.0", "Jobs": {'
separator=
for id in "$@"; do
    case $id in -*|json) continue ;; esac
    number=${id%%.*}
    [ -e "$state/$number.sh" ] || continue
    if [ -e "$state/$number.removed" ]; then job='"job_state": "F", "Exit_status": 271'
    elif [ -e "$state/$number.held" ]; then job="\"job_state\": \"H\", \"comment\": \"$(cat "$state/$number.held")\""
    elif [ -e "$state/$number.exit" ]; then job="\"job_state\": \"F\", \"Exit_status\": $(cat "$state/$number.exit")"
    else job='"job_state": "R"'; fi
    printf '%s"%s": {%s}' "$separator" "$id" "$job"
    separator=,
done
printf '}}\n'
"#;

const QDEL: &str = r#"#!/bin/bash
state="$(dirname "$0")/state"
number=${1%%.*}
echo "$1" >> "$state/removed"
touch "$state/$number.removed"
[ ! -s "$state/$number.pid" ] || kill -TERM -- "-$(cat "$state/$number.pid")" 2> /dev/null
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
fn login_node(root: &Path) -> PathBuf {
    let bin = root.join("bin");
    std::fs::create_dir_all(&bin).expect("makes the folder");
    for (name, text) in [
        ("qsub", QSUB),
        ("qstat", QSTAT),
        ("qdel", QDEL),
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

fn backend(root: &Path, bin: &Path) -> PbsBackend {
    let pbs = Pbs {
        commands: Some(bin.to_owned()),
        options: vec!["-A".into(), "lab".into()],
        project: Some("ab12".into()),
        disk_resource: Some("jobfs".into()),
        ..Pbs::default()
    };
    let apptainer = Container::Apptainer(bin.join("apptainer").to_string_lossy().into_owned());
    let mut backend = PbsBackend::new(pbs, root.join("scratch"), apptainer);
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

#[tokio::test]
async fn runs_gadi_jobs() {
    let root = tempfile::tempdir().expect("a folder");
    let bin = login_node(root.path());
    let backend = backend(root.path(), &bin);
    let spec = spec("tr a-z A-Z < in.txt > out.txt; echo warned >&2");
    let source = root.path().join("in.txt");
    std::fs::write(&source, "hello\n").expect("writes the input");
    let staging = Staging {
        inputs: vec![Staged {
            path: "in.txt".into(),
            source,
            ..Default::default()
        }],
        ..Staging::default()
    };
    let placement = Placement {
        cpus: 2,
        memory_bytes: Some(1 << 30),
        disk_bytes: Some(100 << 20),
        time_seconds: Some(3600),
        queue: Some("normal".into()),
        ..Placement::default()
    };
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
    let script = std::fs::read_to_string(bin.join("state/140000001.sh")).expect("the script");
    for line in [
        "#!/bin/bash\n#PBS -N uwe-run-BIG-1-1\n",
        "#PBS -q normal\n#PBS -P ab12\n#PBS -l ncpus=2\n#PBS -l mem=1024mb\n",
        "#PBS -l walltime=01:05:00\n#PBS -l jobfs=100mb\n",
    ] {
        assert!(script.contains(line), "{line} in {script}");
    }
    let options = std::fs::read_to_string(bin.join("state/140000001.options")).expect("options");
    assert!(options.starts_with("-A\nlab\n"), "{options}");
    // The task's standard error ends with the summary of the job, for nf-gadi.
    let stderr = std::fs::read_to_string(finished.root.join("control/stderr")).expect("stderr");
    assert!(stderr.starts_with("warned\n====="), "{stderr}");
    assert!(
        stderr.contains("Job Id:             140000001.gadi-pbs\n"),
        "{stderr}"
    );
}

#[tokio::test]
async fn removes_held_jobs() {
    let root = tempfile::tempdir().expect("a folder");
    let bin = login_node(root.path());
    std::fs::write(bin.join("hold"), "job held, too many failed attempts").expect("writes");
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
    assert!(message.contains("PBS held the job (job held, too many failed attempts)"));
    let removed = std::fs::read_to_string(bin.join("state/removed")).expect("a removal");
    assert_eq!(removed, "140000001.gadi-pbs\n");
}
