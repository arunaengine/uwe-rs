//! Runs small Nextflow workflows through fake HTCondor, PBS and SLURM tools, which run each job
//! here: on shared folders, and through a staging root with the uwe binary as the helper.
// Copyright (c) 2026 The uwe Contributors
// SPDX-License-Identifier: MIT or Apache-2.0

mod common;

use std::path::{Path, PathBuf};

/// Keeps the submit file and runs its executable in `initialdir` before it answers, with both
/// streams to `output`.
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
(cd "$(value initialdir)" && bash "$(value executable)") > "$(value output)" 2>&1
echo $? > "$state/$id.exit"
echo "$id.0 - $id.0"
"#;

/// The jobs have left the queue by the time anyone asks.
const CONDOR_Q: &str = "#!/bin/bash\nexit 0\n";

const CONDOR_HISTORY: &str = r#"#!/bin/bash
echo "4 $(cat "$(dirname "$0")/state/${1%.0}.exit") undefined"
"#;

const CONDOR_RM: &str = "#!/bin/bash\necho \"$1\" >> \"$(dirname \"$0\")/state/removed\"\n";

/// Runs the command after the image without a container, but sees the folders of the binds at
/// their targets: it rewrites the paths of the wrapper, as a node without the scratch folder.
const APPTAINER: &str = r#"#!/bin/bash
binds=()
while [ $# -gt 0 ]; do
    case $1 in
        --pwd) start=$2; shift 2 ;;
        --bind) rest=${2#*:}; binds+=("${rest%%:*}" "${2%%:*}"); shift 2 ;;
        exec|--*) shift ;;
        *) image=$1; shift; break ;;
    esac
done
place() {
    local path=$1 index
    for ((index = 0; index < ${#binds[@]}; index += 2)); do
        case $path in
            "${binds[index]}"|"${binds[index]}"/*)
                printf '%s' "${binds[index + 1]}${path#"${binds[index]}"}"; return ;;
        esac
    done
    printf '%s' "$path"
}
text=$(cat "$(place "$2")")
for ((index = 0; index < ${#binds[@]}; index += 2)); do
    text=${text//"${binds[index]}"/"${binds[index + 1]}"}
done
wrapper=$(mktemp)
printf '%s\n' "$text" > "$wrapper"
cd "$(place "$start")" || exit 255
exec bash "$wrapper"
"#;

const SCRIPT: &str = "process BIG {
    container 'docker://debian:stable-slim'
    cpus 2
    memory '4 GB'
    disk '10 GB'
    time '1h'
    output: path 'out.txt'
    script: \"echo big > out.txt\"
}
process SMALL {
    container 'docker://debian:stable-slim'
    input: path x
    output: stdout
    script: \"cat $x\"
}
workflow { SMALL(BIG()) | view }
";

/// Runs the script with its output to its `#PBS -o` file before it answers, appends a resource
/// summary as Gadi does, and prints a Gadi job id.
const QSUB: &str = r#"#!/bin/bash
state="$(dirname "$0")/state"
mkdir -p "$state"
script=${@: -1}
exec 9> "$state/ids.lock"
flock -x 9
n=$(( $(cat "$state/last" 2>/dev/null || echo 0) + 1 ))
echo "$n" > "$state/last"
flock -u 9
cp "$script" "$state/$n.sh"
log=$(sed -n 's/^#PBS -o //p' "$script")
bash "$script" > "$log" 2>&1
code=$?
cat >> "$log" <<EOT
======================================================================================
                  Resource Usage on 2026-11-06 14:10:0$n:
   Job Id:             $((140000000+n)).gadi-pbs
   Project:            ab12
   Exit Status:        $code
   Service Units:      0.0$n
   NCPUs Requested:    $((n*2))                      NCPUs Used: $((n*2))
                                           CPU Time Used: 00:00:0$n
   Memory Requested:   $n.0GB                 Memory Used: 1$n.5MB
   GPU Memory Used:    99.0GB
   Walltime Requested: 01:00:00            Walltime Used: 00:00:0$((n+1))
   JobFS Requested:    100.0MB                JobFS Used: 0B
======================================================================================
EOT
echo "$((140000000+n)).gadi-pbs"
"#;

/// The jobs have finished by the time anyone asks.
const QSTAT: &str = r#"#!/bin/bash
printf '{"Jobs": {'
separator=
for id in "$@"; do
    case $id in *.gadi-pbs) printf '%s"%s": {"job_state": "F", "Exit_status": 0}' "$separator" "$id"; separator=, ;; esac
done
printf '}}\n'
"#;

const QDEL: &str = "#!/bin/bash\necho \"$1\" >> \"$(dirname \"$0\")/state/removed\"\n";

/// Runs the job script in its `--chdir` folder with both streams to `--output` before it
/// answers, and prints the job id.
const SBATCH: &str = r#"#!/bin/bash
state="$(dirname "$0")/state"
mkdir -p "$state"
for argument in "$@"; do
    case $argument in
        --chdir=*) chdir=${argument#--chdir=} ;;
        --output=*) output=${argument#--output=} ;;
        --job-name=*) name=${argument#--job-name=} ;;
    esac
    script=$argument
done
exec 9> "$state/ids.lock"
flock -x 9
id=$(( $(cat "$state/last" 2>/dev/null || echo 100) + 1 ))
echo "$id" > "$state/last"
flock -u 9
echo "$name" > "$state/$id.name"
echo "$script" > "$state/$id.command"
(cd "$chdir" && bash "$script") > "$output" 2>&1
echo $? > "$state/$id.exit"
echo "$id"
"#;

/// Answers for the jobs of `--jobs` in the columns of `--format`; it refuses `--json`, as
/// SLURM before 21.08 does.
const SQUEUE: &str = r#"#!/bin/bash
state="$(dirname "$0")/state"
for argument in "$@"; do
    case $argument in
        --jobs=*) jobs=${argument#--jobs=} ;;
        --format=*) format=${argument#--format=} ;;
        --json) echo "squeue: unrecognized option '--json'" >&2; exit 1 ;;
    esac
done
for id in ${jobs//,/ }; do
    [ "$(cat "$state/$id.exit" 2>/dev/null)" = 0 ] && job=COMPLETED || job=FAILED
    case $format in
        '%i %T') echo "$id $job" ;;
        '%i %T %r') echo "$id $job None" ;;
        '%i|%T|%j|%V|%o') echo "$id|$job|$(cat "$state/$id.name")|2026-11-06T00:00:00|$(cat "$state/$id.command")" ;;
    esac
done
"#;

/// A cluster without accounting.
const SACCT: &str = "#!/bin/bash\necho 'sacct: accounting storage is disabled' >&2\nexit 1\n";

const SCANCEL: &str = "#!/bin/bash\necho \"$1\" >> \"$(dirname \"$0\")/state/removed\"\n";

/// Writes the fake tools into `folder/tools`. Not `bin`: every task gets the pipeline's `bin`
/// folder as an input, and the job state there would change it, so `-resume` would miss.
fn tools(folder: &Path) -> PathBuf {
    let bin = folder.join("tools");
    std::fs::create_dir_all(&bin).expect("makes the folder");
    for (name, text) in [
        ("condor_submit", CONDOR_SUBMIT),
        ("condor_q", CONDOR_Q),
        ("condor_history", CONDOR_HISTORY),
        ("condor_rm", CONDOR_RM),
        ("qsub", QSUB),
        ("qstat", QSTAT),
        ("qdel", QDEL),
        ("sbatch", SBATCH),
        ("squeue", SQUEUE),
        ("sacct", SACCT),
        ("scancel", SCANCEL),
        ("apptainer", APPTAINER),
    ] {
        // A child process writes the command, so that no test holds it open for writing.
        let script = "printf '%s' \"$1\" > \"$2\" && chmod 755 \"$2\"";
        let status = std::process::Command::new("sh")
            .args(["-c", script, "sh", text])
            .arg(bin.join(name))
            .status();
        assert!(status.expect("writes the command").success());
    }
    bin
}

/// Runs `uwe run main.nf` with `options` in `folder`, with the fake tools first on the PATH,
/// and gives the exit code and the output.
fn run(folder: &Path, config: &str, settings: &str, options: &[&str]) -> (i32, String) {
    std::fs::write(folder.join("main.nf"), SCRIPT).expect("writes the script");
    std::fs::write(folder.join("nextflow.config"), config).expect("writes the config");
    uwe(folder, "main.nf", settings, options)
}

/// Runs `uwe run <target>` with `options` in `folder`, with the fake tools first on the PATH.
fn uwe(folder: &Path, target: &str, settings: &str, options: &[&str]) -> (i32, String) {
    let bin = tools(folder);
    let file = folder.join("uwe.toml");
    std::fs::write(&file, settings).expect("writes the settings");
    let path = format!(
        "{}:{}",
        bin.display(),
        std::env::var("PATH").unwrap_or_default()
    );
    let output = common::command()
        .args(["run", target, "-uwe-config"])
        .arg(&file)
        .args(options)
        .current_dir(folder)
        .env("PATH", path)
        .env("XDG_CACHE_HOME", folder.join("cache"))
        .output()
        .expect("runs uwe");
    let text = format!(
        "{}{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    (output.status.code().unwrap_or(-1), text)
}

/// nf-ospool's `process.executor = 'ospool'` picks the HTCondor executor; the jobs get the
/// resources and cluster options of their tasks, then the lines of `[executor.htcondor]`.
#[test]
fn runs_ospool_pipelines() {
    let folder = tempfile::tempdir().expect("a folder");
    let config = "process {\n executor = 'ospool'\n \
                  clusterOptions = 'HasCHTCStaging=true; +ProjectName = \"probe\"'\n}\n\
                  executor {\n $ospool {\n  submitFileDir = 'submit'\n }\n}\n";
    let settings = "[executor.htcondor]\noptions = ['+Lab = \"genomics\"']\n";
    let (code, output) = run(folder.path(), config, settings, &[]);
    assert_eq!(code, 0, "{output}");
    assert!(output.contains("big"), "{output}");
    let state = folder.path().join("tools/state");
    let submitted = std::fs::read_to_string(state.join("1001.condor")).expect("a submit file");
    for line in [
        "request_cpus = 2\nmachine_count = 1\nrequest_memory = 4096\nrequest_disk = 10485760\n",
        "should_transfer_files = IF_NEEDED\n",
        "HasCHTCStaging=true\n+ProjectName = \"probe\"\n+Lab = \"genomics\"\nqueue\n",
    ] {
        assert!(submitted.contains(line), "{line} in {submitted}");
    }
    assert!(state.join("1002.condor").is_file());
    assert!(!state.join("removed").exists());
}

/// `-executor htcondor` runs a pipeline without an executor of its own through a staging root,
/// with HTCondor's file transfer for the job script and the uwe binary as the helper.
#[test]
fn runs_staged_jobs() {
    let folder = tempfile::tempdir().expect("a folder");
    let settings = format!(
        "[executor.htcondor]\nstaging = 'object-store'\nstaging_root = '{}'\n\
         node_folder = '{}'\nhelper_path = '{}'\n",
        folder.path().join("staging").display(),
        folder.path().join("node").display(),
        common::BINARY,
    );
    let (code, output) = run(folder.path(), "", &settings, &["-executor", "htcondor"]);
    assert_eq!(code, 0, "{output}");
    assert!(output.contains("big"), "{output}");
    let state = folder.path().join("tools/state");
    let submitted = std::fs::read_to_string(state.join("1001.condor")).expect("a submit file");
    assert!(
        submitted.contains("should_transfer_files = YES\n"),
        "{submitted}"
    );
    let node = std::fs::read_dir(folder.path().join("node")).expect("the node folder");
    assert_eq!(node.count(), 0, "the jobs remove their folders");
}

/// nf-gadi's pipeline with Nextflow's `pbspro` executor: the jobs get Gadi's project and storage
/// from the cluster options, and nf-gadi writes the recorded report.
#[test]
fn runs_gadi_pipelines() {
    let folder = tempfile::tempdir().expect("a folder");
    let config = "plugins { id 'nf-gadi@1.4.0' }\nprocess {\n executor = 'pbspro'\n \
                  queue = 'normal'\n clusterOptions = '-P ab12 -l storage=gdata/ab12'\n}\n\
                  gadi {\n format = 'csv'\n output = 'report.csv'\n}\n";
    let settings = "[executor.pbs]\ndisk_resource = 'jobfs'\n";
    let (code, output) = run(folder.path(), config, settings, &[]);
    assert_eq!(code, 0, "{output}");
    assert!(output.contains("big"), "{output}");
    assert!(!output.contains("`clusterOptions` is ignored"), "{output}");
    let script = std::fs::read_to_string(folder.path().join("tools/state/1.sh")).expect("a job");
    for line in [
        "#!/bin/bash\n#PBS -P ab12 -l storage=gdata/ab12\n#PBS -N uwe-",
        "#PBS -j oe\n#PBS -q normal\n#PBS -l ncpus=2\n#PBS -l mem=4096mb\n",
        "#PBS -l walltime=01:05:00\n#PBS -l jobfs=10240mb\n",
    ] {
        assert!(script.contains(line), "{line} in {script}");
    }
    let recorded = "Name,Process,Queue,Service Units,CPUs,CPU time,Used Walltime,\
                    Requested Walltime,Used Memory,Requested Memory,Used JobFS,Requested JobFS,\
                    Efficiency,Exit Code\n\
                    BIG,BIG,normal,0.01,2,00:00:01,00:00:02,01:00:00,11.5MB,1.0GB,0B,100.0MB,\
                    0.25,0\n\
                    SMALL,SMALL,normal,0.02,4,00:00:02,00:00:03,01:00:00,12.5MB,2.0GB,0B,\
                    100.0MB,0.17,0\n";
    let report = std::fs::read_to_string(folder.path().join("report.csv")).expect("a report");
    assert_eq!(report, recorded, "{output}");
}

/// A Snakemake rule runs as a PBS job with `-executor pbs`, as tasks of every language do.
#[test]
fn runs_snakemake_rules() {
    let folder = tempfile::tempdir().expect("a folder");
    let rule = "rule count:\n    input: \"sample.txt\"\n    output: \"count.txt\"\n    \
                container: \"docker://debian:stable-slim\"\n    \
                shell: \"wc -w < {input} > {output}\"\n";
    std::fs::write(folder.path().join("Snakefile"), rule).expect("writes the rule");
    std::fs::write(folder.path().join("sample.txt"), "one two three\n").expect("writes");
    let options = ["-executor", "pbs"];
    let (code, output) = uwe(folder.path(), "Snakefile", "[executor.pbs]\n", &options);
    assert_eq!(code, 0, "{output}");
    let count = std::fs::read_to_string(folder.path().join("count.txt")).expect("a count");
    assert_eq!(count.trim(), "3");
    assert!(folder.path().join("tools/state/1.sh").is_file());
}

/// `-resume` takes every task of an earlier run of a digest-pinned pipeline, through HTCondor,
/// PBS and SLURM, and submits no job again.
#[test]
fn resumes_pinned_pipelines() {
    let pinned = SCRIPT.replace(
        "debian:stable-slim",
        "debian@sha256:0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef",
    );
    for (executor, last) in [("htcondor", "1002"), ("pbs", "2"), ("slurm", "102")] {
        let folder = tempfile::tempdir().expect("a folder");
        std::fs::write(folder.path().join("main.nf"), &pinned).expect("writes the script");
        let settings = format!("[executor.{executor}]\n");
        let options = ["-executor", executor, "-resume"];
        let (code, output) = uwe(folder.path(), "main.nf", &settings, &options);
        assert_eq!(code, 0, "{output}");
        let (code, output) = uwe(folder.path(), "main.nf", &settings, &options);
        assert_eq!(code, 0, "{output}");
        assert!(output.contains("big"), "{output}");
        let submitted = std::fs::read_to_string(folder.path().join("tools/state/last"));
        assert_eq!(submitted.expect("a job count").trim(), last, "{output}");
    }
}
