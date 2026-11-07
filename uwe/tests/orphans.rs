//! Reports and stops the Kubernetes Jobs and SLURM jobs of a run whose process is gone, through
//! the settings of the run, against a fake API server and fake tools of SLURM.
// Copyright (c) 2026 The uwe Contributors
// SPDX-License-Identifier: MIT or Apache-2.0

use std::collections::BTreeMap;
use std::io::{BufRead, BufReader, Read, Write};
use std::net::{TcpListener, TcpStream};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use executor::registry::{ImageConfig, ImageConfigs};
use executor::{Backend, CancellationToken, Job, Staging};
use kubernetes_executor::{Cluster, KubernetesBackend};
use serde_json::{Value, json};
use slurm_executor::{Container, SlurmBackend};
use task_model::{FORMAT, Image, Placement, TaskSpec};
use uwe::RunRequest;
use uwe::run::Settings;
use uwe::run::orphans::{Orphan, Outcome};

/// The child process that runs a test, with the fake tools first on its PATH.
const CHILD: &str = "UWE_ORPHANS_CHILD";

/// The run whose process is gone.
const RUN: &str = "left-run";

/// Runs its script in the background in a session of its own, and keeps its name and comment.
const SBATCH: &str = r#"#!/bin/bash
state="$(dirname "$0")/state"
mkdir -p "$state"
comment='(null)'
for argument in "$@"; do
    case $argument in
        --chdir=*) chdir=${argument#--chdir=} ;;
        --job-name=*) name=${argument#--job-name=} ;;
        --comment=*) comment=${argument#--comment=} ;;
    esac
    script=$argument
done
id=$(( $(cat "$state/last" 2>/dev/null || echo 100) + 1 ))
echo "$id" > "$state/last"
echo "$name" > "$state/$id.name"
echo "$comment" > "$state/$id.comment"
echo "$script" > "$state/$id.command"
(cd "$chdir" && setsid bash -c 'echo $$ > "$1"; exec bash "$2"' job "$state/$id.pid" "$script" \
    > /dev/null 2>&1; echo $? > "$state/$id.exit") > /dev/null 2>&1 &
echo "$id"
"#;

/// Answers for the jobs of `--jobs` or `--name` in the columns of `--format`.
const SQUEUE: &str = r#"#!/bin/bash
state="$(dirname "$0")/state"
for argument in "$@"; do
    case $argument in
        --jobs=*) jobs=${argument#--jobs=} ;;
        --name=*) name=${argument#--name=} ;;
        --format=*) format=${argument#--format=} ;;
        --json) echo "squeue: unrecognized option '--json'" >&2; exit 1 ;;
    esac
done
if [ -n "$name" ]; then
    for file in "$state"/*.name; do
        [ "$(cat "$file")" = "$name" ] && jobs="$jobs,$(basename "$file" .name)"
    done
fi
for id in ${jobs//,/ }; do
    [ -e "$state/$id.name" ] || continue
    if [ -e "$state/$id.cancelled" ]; then job=CANCELLED
    elif [ ! -e "$state/$id.exit" ]; then job=RUNNING
    else job=COMPLETED; fi
    case $format in
        '%i %T') echo "$id $job" ;;
        '%i %T %r') echo "$id $job None" ;;
        '%i|%o') echo "$id|$(cat "$state/$id.command")" ;;
        '%i|%T|%U|%k') echo "$id|$job|$(id -u)|$(cat "$state/$id.comment")" ;;
        '%i|%T|%j|%V|%o') echo "$id|$job|$(cat "$state/$id.name")|2026-11-07T00:00:00|$(cat "$state/$id.command")" ;;
    esac
done
"#;

/// Ends the job of its first argument when it has the name of `--name`, and notes the call.
const SCANCEL: &str = r#"#!/bin/bash
state="$(dirname "$0")/state"
for option in "$@"; do
    case $option in --name=*) [ "${option#--name=}" = "$(cat "$state/$1.name")" ] || exit 0 ;; esac
done
echo "$*" >> "$state/cancels"
touch "$state/$1.cancelled"
until [ -s "$state/$1.pid" ]; do sleep 0.1; done
kill -TERM -- "-$(cat "$state/$1.pid")"
"#;

/// A cluster without accounting.
const SACCT: &str = "#!/bin/bash\necho 'sacct: accounting storage is disabled' >&2\nexit 1\n";

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

#[test]
fn stops_left_jobs() {
    if std::env::var_os(CHILD).is_none() {
        let root = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(root.path().join("tools")).unwrap();
        let tools = [
            ("sbatch", SBATCH),
            ("squeue", SQUEUE),
            ("scancel", SCANCEL),
            ("sacct", SACCT),
            ("apptainer", APPTAINER),
        ];
        for (name, text) in tools {
            let file = root.path().join("tools").join(name);
            std::fs::write(&file, text).unwrap();
            let mode = std::os::unix::fs::PermissionsExt::from_mode(0o755);
            std::fs::set_permissions(&file, mode).unwrap();
        }
        let path = format!(
            "{}:{}",
            root.path().join("tools").display(),
            std::env::var("PATH").unwrap_or_default()
        );
        let output = std::process::Command::new(std::env::current_exe().unwrap())
            .args(["--exact", "stops_left_jobs", "--nocapture"])
            .env(CHILD, root.path())
            .env("PATH", path)
            .env("UWE_SITE_CONFIG", "")
            .env("XDG_CONFIG_HOME", root.path().join("config"))
            .env_remove("SLURM_CONF")
            .env_remove("SLURM_CLUSTERS")
            .output()
            .unwrap();
        stop_jobs(&root.path().join("tools/state"));
        let text = String::from_utf8_lossy(&[output.stdout, output.stderr].concat()).into_owned();
        assert!(output.status.success(), "the child failed:\n{text}");
        assert!(text.contains("1 passed"), "the child ran no test:\n{text}");
        return;
    }
    let root = PathBuf::from(std::env::var_os(CHILD).unwrap());
    let runtime = tokio::runtime::Runtime::new().unwrap();
    let (kubeconfig, api) = serve(&root);
    let cache = root.join("cache");
    let cluster = runtime
        .block_on(Cluster::connect(&kubeconfig, None))
        .unwrap();
    let mut jobs = KubernetesBackend::new(cluster, cache.join("scratch/kubernetes"));
    jobs.run = RUN.into();
    jobs.configs = ImageConfigs::known([("debian:stable-slim".into(), ImageConfig::default())]);
    let mut batch = SlurmBackend::new(
        cache.join("scratch/slurm"),
        Container::Apptainer("apptainer".into()),
    );
    batch.run = RUN.into();
    batch.poll = Duration::from_millis(100);
    // Both attempts start, and their process ends once the cluster took their jobs.
    let pod = spec("debian:stable-slim");
    let node = spec("/images/tool.sif");
    let (staging, placement, cancel) = (Staging::default(), placement(), CancellationToken::new());
    let job = |name, spec| Job {
        name,
        spec,
        staging: &staging,
        placement: &placement,
    };
    let attempt = format!("{RUN}-sleep-1-1");
    let journals = ["kubernetes", "slurm"]
        .map(|kind| cache.join(format!("scratch/{kind}/{RUN}-sleep-1-1/remote.json")));
    runtime.block_on(async {
        tokio::select! {
            results = async { tokio::join!(
                jobs.run(job(&attempt, &pod), &cancel),
                batch.run(job(&attempt, &node), &cancel),
            ) } => panic!("{results:?}"),
            () = until(|| journals.iter().all(|journal| taken(journal))) => {}
        }
    });

    let config = root.join("uwe.toml");
    let text = format!(
        "[executor.kubernetes]\nkubeconfig = \"{}\"\n",
        kubeconfig.display()
    );
    std::fs::write(&config, text).unwrap();
    let request = || {
        let mut settings = Settings::new("");
        settings.run_id = Some(RUN.into());
        settings.config_file = Some(config.clone());
        settings.cache_folder = Some(cache.clone());
        RunRequest::new(settings)
    };
    let outcomes = |found: Vec<Orphan>| -> Vec<(Option<String>, Outcome)> {
        (found.into_iter())
            .map(|orphan| (orphan.state, orphan.outcome))
            .collect()
    };
    let report = runtime.block_on(request().remote_tasks());
    let report = report.unwrap_or_else(|failure| panic!("{}", failure.message));
    assert_eq!(
        outcomes(report),
        [
            (Some("active".into()), Outcome::Running),
            (Some("RUNNING".into()), Outcome::Running)
        ]
    );
    assert!(journals.iter().all(|journal| journal.is_file()));
    assert!(
        !api.lock()
            .unwrap()
            .calls
            .iter()
            .any(|call| call.starts_with("DELETE"))
    );

    let stopped = runtime.block_on(request().stop_tasks());
    let stopped = stopped.unwrap_or_else(|failure| panic!("{}", failure.message));
    let canceled: Vec<Outcome> = stopped.into_iter().map(|orphan| orphan.outcome).collect();
    assert_eq!(canceled, [Outcome::Canceled, Outcome::Canceled]);
    assert!(journals.iter().all(|journal| !journal.exists()));
    let deleted = format!("DELETE /apis/batch/v1/namespaces/lab/jobs/uwe-{RUN}-sleep-1-1");
    assert!(api.lock().unwrap().calls.contains(&deleted));
    let cancels = std::fs::read_to_string(root.join("tools/state/cancels")).unwrap();
    assert!(
        cancels.contains(&format!("--name=uwe-{RUN}-sleep-1-1")),
        "{cancels}"
    );
    // Once the journals are settled, a second call asks no cluster.
    let calls = api.lock().unwrap().calls.len();
    let again = runtime.block_on(request().stop_tasks());
    assert!(
        again
            .unwrap_or_else(|failure| panic!("{}", failure.message))
            .is_empty()
    );
    assert_eq!(api.lock().unwrap().calls.len(), calls);
}

/// What the fake API server knows: the Jobs by name, whether each is deleted, and each request
/// as `METHOD path`.
#[derive(Default)]
struct Api {
    jobs: BTreeMap<String, (Value, bool)>,
    calls: Vec<String>,
}

/// An API server that keeps the Jobs that it gets as active, with no pods, until they are
/// deleted; gives a kubeconfig for it in `root`.
fn serve(root: &Path) -> (PathBuf, Arc<Mutex<Api>>) {
    let api = Arc::new(Mutex::new(Api::default()));
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let address = listener.local_addr().unwrap();
    let shared = api.clone();
    std::thread::spawn(move || {
        for stream in listener.incoming().flatten() {
            answer(&shared, stream);
        }
    });
    let kubeconfig = root.join("kubeconfig");
    let text = format!(
        "apiVersion: v1\nkind: Config\ncurrent-context: test\nclusters:\n- name: test\n  \
         cluster:\n    server: http://{address}\ncontexts:\n- name: test\n  context:\n    \
         cluster: test\n    user: test\n    namespace: lab\nusers:\n- name: test\n  user:\n    \
         token: test-token\n"
    );
    std::fs::write(&kubeconfig, text).unwrap();
    (kubeconfig, api)
}

fn answer(api: &Mutex<Api>, mut stream: TcpStream) {
    let mut reader = BufReader::new(&mut stream);
    let mut first = String::new();
    reader.read_line(&mut first).unwrap();
    let mut length = 0;
    loop {
        let mut line = String::new();
        reader.read_line(&mut line).unwrap();
        if line.trim().is_empty() {
            break;
        }
        if let Some((name, value)) = line.split_once(':')
            && name.eq_ignore_ascii_case("content-length")
        {
            length = value.trim().parse().unwrap();
        }
    }
    let mut body = vec![0; length];
    reader.read_exact(&mut body).unwrap();
    let mut words = first.split_whitespace();
    let (method, target) = (words.next().unwrap(), words.next().unwrap());
    let path = target.split('?').next().unwrap();
    let mut api = api.lock().unwrap();
    api.calls.push(format!("{method} {path}"));
    let jobs = "/apis/batch/v1/namespaces/lab/jobs";
    let name = path.strip_prefix(&format!("{jobs}/")).unwrap_or_default();
    let missing = json!({"kind": "Status", "apiVersion": "v1", "status": "Failure",
        "code": 404, "reason": "NotFound"});
    let (status, value) = match method {
        "POST" if path == jobs => {
            let mut job: Value = serde_json::from_slice(&body).unwrap();
            job["metadata"]["uid"] = json!("job-uid");
            if !target.contains("dryRun") {
                let name = job["metadata"]["name"].as_str().unwrap().to_owned();
                job["status"] = json!({"active": 1});
                api.jobs.insert(name, (job.clone(), false));
            }
            ("201 Created", job)
        }
        "GET" => match api.jobs.get(name) {
            Some((job, false)) => ("200 OK", job.clone()),
            Some(_) => ("404 Not Found", missing),
            None if name.is_empty() => (
                "200 OK",
                json!({"apiVersion": "v1", "metadata": {}, "items": []}),
            ),
            None => ("404 Not Found", missing),
        },
        "DELETE" => {
            if let Some(job) = api.jobs.get_mut(name) {
                job.1 = true;
            }
            (
                "200 OK",
                json!({"kind": "Status", "apiVersion": "v1", "status": "Success"}),
            )
        }
        _ => ("404 Not Found", missing),
    };
    let body = value.to_string();
    let reply = format!(
        "HTTP/1.1 {status}\r\nContent-Type: application/json\r\nContent-Length: {}\r\n\
         Connection: close\r\n\r\n{body}",
        body.len()
    );
    let _ = stream.write_all(reply.as_bytes());
}

fn spec(image: &str) -> TaskSpec {
    TaskSpec {
        output_manifest: None,
        run_as: None,
        format: FORMAT,
        namespace: String::new(),
        image: Some(Image {
            reference: image.into(),
            platform: None,
        }),
        arguments: vec!["sleep".into(), "300".into()],
        workdir: None,
        environment: BTreeMap::new(),
        inputs: Vec::new(),
        outputs: Vec::new(),
        stdin: None,
        containers: Vec::new(),
        replaces_entrypoint: true,
    }
}

fn placement() -> Placement {
    Placement {
        cpus: 1,
        memory_bytes: Some(256 << 20),
        ..Placement::default()
    }
}

/// Whether the journal of an attempt names the job that the cluster took.
fn taken(journal: &Path) -> bool {
    let record = std::fs::read(journal).ok();
    let record: Option<Value> = record.and_then(|bytes| serde_json::from_slice(&bytes).ok());
    record.is_some_and(|record| record["uid"].is_string() || record["identity"].is_object())
}

/// Waits until `ready` holds, for at most 30 seconds.
async fn until(ready: impl Fn() -> bool) {
    for _ in 0..3000 {
        if ready() {
            return;
        }
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
    panic!("the test waited too long");
}

/// Ends the fake jobs that still run, by the process groups that the fake `sbatch` noted.
fn stop_jobs(state: &Path) {
    for entry in std::fs::read_dir(state).into_iter().flatten().flatten() {
        let path = entry.path();
        if path.extension().is_some_and(|extension| extension == "pid")
            && !path.with_extension("exit").exists()
            && let Ok(group) = std::fs::read_to_string(&path)
        {
            let group = format!("-{}", group.trim());
            let _ = std::process::Command::new("kill")
                .args(["-TERM", "--", &group])
                .status();
        }
    }
}
