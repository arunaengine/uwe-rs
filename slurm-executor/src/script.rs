//! The batch job of an attempt: the options of `sbatch`, and the job script that runs the
//! wrapper of the task in its container on a node.
// Copyright (c) 2026 The iunctus Contributors
// SPDX-License-Identifier: MIT or Apache-2.0

use std::path::{Path, PathBuf};

use executor::containers::recording_its_end;
use executor::wrapper::quote;
use executor::{CONTROL, ContainerRun, EngineCommand, Job};

/// The job script in the control folder of an attempt.
pub const SCRIPT: &str = "job.sh";
/// The file in the control folder that gets the output of the job, like the errors of the
/// container runtime.
pub const LOG: &str = "slurm.log";
/// The file in the control folder where the job records the exit code of the container once
/// it ended, so that the files of the task are complete.
pub const CONTAINER_EXIT: &str = "container-exit";

/// How the nodes run the containers of tasks.
#[derive(Clone, Debug)]
pub enum Container {
    /// Apptainer or Singularity, by its command. It runs images of registries as the user,
    /// inside the job, so that SLURM limits and stops it.
    Apptainer(String),
    /// Podman or Docker. The container may run outside the job, so it gets the limits of the
    /// job and stops with it.
    Engine(EngineCommand),
}

/// The options of `sbatch` for an attempt whose scratch directory is `root`: its name, its
/// folder and log, its CPUs, memory, time, GPUs and queue, then the `extra` options of the
/// user, which come last and so win.
pub fn sbatch_options(job: Job<'_>, root: &Path, extra: &[String]) -> Vec<String> {
    let placement = job.placement;
    let mut options = head(job.name, root);
    options.push(format!("--cpus-per-task={}", placement.cpus.max(1)));
    if let Some(bytes) = placement.memory_bytes {
        options.push(format!("--mem={}M", bytes.div_ceil(1 << 20)));
    }
    if let Some(seconds) = placement.time_seconds {
        options.push(format!("--time={}", time(seconds)));
    }
    let gpus: u32 = placement.accelerators.iter().map(|gpu| gpu.count).sum();
    if gpus > 0 {
        options.push(format!("--gpus={gpus}"));
    }
    if let Some(queue) = &placement.queue {
        options.push(format!("--partition={queue}"));
    }
    options.extend(extra.iter().cloned());
    options
}

/// What the job that pulls an image into a SIF file asks for. Very large images may need more
/// than the default of 2 CPUs, 4 GB and one hour.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct PullResources {
    pub cpus: u32,
    pub memory_mb: u64,
    pub time_seconds: u64,
}

impl Default for PullResources {
    fn default() -> PullResources {
        PullResources {
            cpus: 2,
            memory_mb: 4096,
            time_seconds: 3600,
        }
    }
}

/// The options of `sbatch` for the job that pulls an image into a SIF file in `root`: the
/// resources of `pull`, then the `extra` options of the user.
pub fn pull_options(
    name: &str,
    root: &Path,
    pull: &PullResources,
    extra: &[String],
) -> Vec<String> {
    let mut options = head(name, root);
    options.extend([
        format!("--cpus-per-task={}", pull.cpus),
        format!("--mem={}M", pull.memory_mb),
        format!("--time={}", time(pull.time_seconds)),
    ]);
    options.extend(extra.iter().cloned());
    options
}

/// The options that every job of iunctus gets: its name, its folder and its log.
fn head(name: &str, root: &Path) -> Vec<String> {
    vec![
        "--parsable".to_owned(),
        format!("--job-name=iunctus-{name}"),
        format!("--chdir={}", root.display()),
        format!("--output={}", root.join(CONTROL).join(LOG).display()),
        "--nodes=1".into(),
        "--ntasks=1".into(),
    ]
}

/// A time in the form that SLURM reads, `days-hours:minutes:seconds`.
fn time(seconds: u64) -> String {
    let (days, rest) = (seconds / 86_400, seconds % 86_400);
    let (hours, rest) = (rest / 3600, rest % 3600);
    format!("{days}-{hours:02}:{:02}:{:02}", rest / 60, rest % 60)
}

/// The exit code that the job script records for a container that ran longer than the time of
/// its task, as `timeout` gives it.
pub const TIMED_OUT: i32 = 124;

/// How long a container that ran out of time may take to end after its stop signal.
const STOP_GRACE: u64 = 10;

/// The job script: it waits until the node sees the whole wrapper, runs the wrapper in the
/// task's container, makes what the task wrote durable, then records the exit code of the
/// container. Podman and Docker get an init process, which hands the signal that stops the job
/// to the task, so that the container ends with the job. A task with a `time` runs under
/// `timeout`, as SLURM counts the time of a job in whole minutes. `size` is the length of the
/// wrapper in bytes.
pub fn job_script(
    container: &Container,
    run: &ContainerRun<'_>,
    gpus: bool,
    time: Option<u64>,
    size: u64,
) -> String {
    let words: Vec<String> = (command(container, run, gpus).iter())
        .map(|word| quote(word))
        .collect();
    let control = run.root.join(CONTROL);
    let exit = quote(&control.join(CONTAINER_EXIT).to_string_lossy());
    let record = quote(&control.join(executor::wrapper::EXIT).to_string_lossy());
    format!(
        "#!/bin/bash\n# The iunctus job of one attempt of a task.\n{}{}{}\nstatus=$?\n{}{}\
         [ \"$status\" -ne 0 ] || [ -s {record} ] || echo \"iunctus: the container ended, but \
         the wrapper of the task recorded no exit code on $(hostname)\" >&2\n\
         echo $status > {exit}\n",
        wait_for(&[(PathBuf::from(run.script()), size)], &exit),
        timeout(time),
        words.join(" "),
        timed_out(time),
        durable(run.root)
    )
}

/// The job script of a task of several containers: it starts a container for each of `runs`,
/// one for each image, all at once, as the local executor does, then records the exit code of
/// the first container that failed, else 0. `exits` holds for each run the files of the exit
/// codes of its containers: a container of the engine that fails without its wrappers, as when
/// it is killed for lack of memory, records its exit code there, so that the containers that
/// wait for one of them go on. `sizes` holds the length in bytes of the wrapper of each run,
/// which the job waits to see whole, as [`job_script`] does.
pub fn job_script_of_containers(
    container: &Container,
    runs: &[ContainerRun<'_>],
    exits: &[Vec<PathBuf>],
    gpus: bool,
    time: Option<u64>,
    sizes: &[u64],
) -> String {
    let mut script = String::from("#!/bin/bash\n# The iunctus job of one attempt of a task.\n");
    let exit = runs[0].root.join(CONTROL).join(CONTAINER_EXIT);
    let wrappers: Vec<(PathBuf, u64)> = (runs.iter().zip(sizes))
        .map(|(run, size)| (PathBuf::from(run.script()), *size))
        .collect();
    script.push_str(&wait_for(&wrappers, &quote(&exit.to_string_lossy())));
    script.push_str("pids=\n");
    for (index, run) in runs.iter().enumerate() {
        let words: Vec<String> = (command(container, run, gpus).iter())
            .map(|word| quote(word))
            .collect();
        let exits: Vec<String> = (exits.get(index).into_iter().flatten())
            .map(|exit| exit.to_string_lossy().into_owned())
            .collect();
        let command = format!("{}{}", timeout(time), words.join(" "));
        script.push_str(&recording_its_end(&command, &exits));
        script.push_str("pids=\"$pids $!\"\n");
    }
    script.push_str(&format!(
        "status=0\nfor pid in $pids; do\n    wait \"$pid\"\n    code=$?\n    [ \"$status\" -ne 0 ] || status=$code\ndone\n{}{}echo \"$status\" > {}\n",
        timed_out(time),
        durable(runs[0].root),
        quote(&exit.to_string_lossy())
    ));
    script
}

/// The exit code that a job records when the node does not see the whole wrapper of its task
/// in time, as Docker gives 125 when it cannot run a container.
pub const NOT_SEEN: i32 = 125;

/// How long a job waits for the node to see the whole wrapper of its task, in seconds.
const SEEN_GRACE: u64 = 60;

/// The lines that wait until the node sees each file at its length in bytes. A shared file
/// system like CephFS may show a file that another machine has just written as empty or short
/// for a while, and a shell runs an empty wrapper as a task that did nothing and ended well.
/// A file that the node does not see whole in time ends the job with [`NOT_SEEN`] in `exit`.
fn wait_for(files: &[(PathBuf, u64)], exit: &str) -> String {
    let checks: Vec<String> = (files.iter())
        .map(|(path, size)| format!("seen {} {size}", quote(&path.to_string_lossy())))
        .collect();
    format!(
        "seen() {{ [ \"$(stat -c %s \"$1\" 2> /dev/null)\" = \"$2\" ]; }}\n\
         waited=0\n\
         until {}; do\n    \
         if [ $waited -ge {SEEN_GRACE} ]; then\n        \
         echo \"iunctus: after {SEEN_GRACE} s, the node $(hostname) still does not see the whole \
         wrapper of the task\" >&2\n        \
         echo {NOT_SEEN} > {exit}\n        \
         exit 0\n    \
         fi\n    \
         sleep 1\n    \
         waited=$((waited + 1))\n\
         done\n",
        checks.join(" && ")
    )
}

/// The line that makes every file in the folder of an attempt durable before the job records
/// its end, so that the machine of iunctus, which reads the records and outputs next, sees
/// them whole on a shared file system. `sync` with files syncs only those files.
fn durable(root: &Path) -> String {
    format!(
        "find {} -type f -exec sync -- {{}} + 2> /dev/null\n",
        quote(&root.to_string_lossy())
    )
}

/// The start of a command that stops it at the time of its task, then kills it after a grace.
fn timeout(time: Option<u64>) -> String {
    match time {
        Some(seconds) => format!("timeout --kill-after={STOP_GRACE} {seconds} "),
        None => String::new(),
    }
}

/// The line that records a container that needed the kill after its time as out of time too.
fn timed_out(time: Option<u64>) -> String {
    match time {
        Some(seconds) => {
            format!("[ $status -eq 137 ] && [ $SECONDS -ge {seconds} ] && status={TIMED_OUT}\n")
        }
        None => String::new(),
    }
}

/// The command that runs the wrapper of `run` in its container.
fn command(container: &Container, run: &ContainerRun<'_>, gpus: bool) -> Vec<String> {
    match container {
        Container::Apptainer(program) => apptainer(program, run, gpus),
        Container::Engine(engine) => {
            let mut command = vec![engine.program.clone()];
            let mut arguments = run.arguments(engine);
            arguments.insert(1, "--init".into());
            command.extend(arguments);
            command
        }
    }
}

/// The command of Apptainer: a container with its own `/tmp`, home and environment, which sees
/// the scratch directory at the same path, the work directory, and the other mounts. It runs
/// as the user who runs iunctus, without setuid or fakeroot. A task that runs as Kubernetes
/// runs it starts in its `workdir`, or in `/`, the working directory of most images, which a
/// job cannot ask the registry for; its file system is writable in memory
/// (`--writable-tmpfs`), so that it may write outside its folders where its user may write.
/// A task without network requests an isolated network. Sites or runtimes that cannot
/// provide it must fail the container launch.
fn apptainer(program: &str, run: &ContainerRun<'_>, gpus: bool) -> Vec<String> {
    let start = match run.access.run_as {
        Some(_) => run.start().unwrap_or_else(|| "/".to_owned()),
        None => run.inside(),
    };
    let mut command = vec![
        program.to_owned(),
        "exec".into(),
        "--containall".into(),
        "--pwd".into(),
        start,
    ];
    if run.access.run_as.is_some() {
        command.push("--writable-tmpfs".into());
    }
    if run.offline {
        command.extend(["--net".into(), "--network".into(), "none".into()]);
    }
    for bind in run.binds() {
        command.extend(["--bind".into(), executor::local::volume(&bind)]);
    }
    if gpus {
        command.push("--nv".into());
    }
    command.push(image(&run.image.reference));
    command.extend(["sh".into(), run.script()]);
    command
}

/// The image as Apptainer names it: images of registries get `docker://`, while a path or an
/// address with its own scheme stays as it is.
fn image(reference: &str) -> String {
    let local = reference.starts_with('/') || reference.ends_with(".sif");
    match local || reference.contains("://") {
        true => reference.to_owned(),
        false => format!("docker://{reference}"),
    }
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeMap;
    use std::path::PathBuf;

    use executor::process::Mount;
    use executor::{Access, EngineKind, Staging};
    use task_model::{Accelerator, FORMAT, Image, Placement, TaskSpec};

    use super::*;

    fn spec(workdir: Option<&str>) -> TaskSpec {
        TaskSpec {
            run_as: None,
            format: FORMAT,
            namespace: String::new(),
            image: Some(Image {
                reference: "debian@sha256:ab".into(),
                platform: None,
            }),
            arguments: vec!["bash".into(), "-c".into(), "true".into()],
            workdir: workdir.map(str::to_owned),
            environment: BTreeMap::new(),
            inputs: Vec::new(),
            outputs: Vec::new(),
            stdin: None,
            containers: Vec::new(),
            replaces_entrypoint: false,
        }
    }

    fn run<'a>(
        image: &'a Image,
        mounts: &'a [Mount],
        workdir: Option<&'a str>,
    ) -> ContainerRun<'a> {
        ContainerRun {
            name: "iunctus-run-P-1-1",
            image,
            root: Path::new("/shared/scratch/run-P-1-1"),
            workdir,
            mounts,
            owner: (1000, 100),
            cpus: 2,
            memory: Some(3 << 30),
            offline: false,
            wrapper_path: None,
            access: &CALLER,
        }
    }

    /// How a container of a task runs that runs as the user who runs iunctus.
    static CALLER: Access = Access {
        run_as: None,
        user: None,
        capabilities: Vec::new(),
        hand_back: None,
    };

    #[test]
    fn requests_task_resources() {
        let spec = spec(None);
        let staging = Staging::default();
        let placement = Placement {
            cpus: 2,
            memory_bytes: Some((3 << 30) + 1),
            time_seconds: Some(90_061),
            accelerators: vec![Accelerator {
                count: 1,
                kind: None,
            }],
            queue: Some("short".into()),
            ..Placement::default()
        };
        let job = Job {
            name: "run-P-1-1",
            spec: &spec,
            staging: &staging,
            placement: &placement,
        };
        let root = Path::new("/shared/scratch/run-P-1-1");
        let options = sbatch_options(job, root, &["--account=lab".into()]);
        let expected = [
            "--parsable",
            "--job-name=iunctus-run-P-1-1",
            "--chdir=/shared/scratch/run-P-1-1",
            "--output=/shared/scratch/run-P-1-1/control/slurm.log",
            "--nodes=1",
            "--ntasks=1",
            "--cpus-per-task=2",
            "--mem=3073M",
            "--time=1-01:01:01",
            "--gpus=1",
            "--partition=short",
            "--account=lab",
        ];
        assert_eq!(options, expected);
    }

    #[test]
    fn apptainer_mounts_folders() {
        let image = spec(None).image.expect("an image");
        let mounts = [Mount {
            source: PathBuf::from("/shared/store/cas"),
            target: "/shared/store/cas".into(),
            read_only: true,
        }];
        let script = job_script(
            &Container::Apptainer("apptainer".into()),
            &run(&image, &mounts, Some("/data")),
            true,
            None,
            100,
        );
        let expected = "\ndone\n\
            'apptainer' 'exec' '--containall' '--pwd' '/data' \
            '--bind' '/shared/scratch/run-P-1-1:/shared/scratch/run-P-1-1' \
            '--bind' '/shared/scratch/run-P-1-1/work:/data' \
            '--bind' '/shared/store/cas:/shared/store/cas:ro' '--nv' \
            'docker://debian@sha256:ab' 'sh' \
            '/shared/scratch/run-P-1-1/control/wrapper.sh'\n\
            status=$?\n\
            find '/shared/scratch/run-P-1-1' -type f -exec sync -- {} + 2> /dev/null\n";
        assert!(script.contains(expected), "{script}");
        // The job waits for the node to see the whole wrapper, and records the end.
        assert!(
            script.contains("until seen '/shared/scratch/run-P-1-1/control/wrapper.sh' 100; do\n"),
            "{script}"
        );
        assert!(
            script.ends_with("echo $status > '/shared/scratch/run-P-1-1/control/container-exit'\n"),
            "{script}"
        );
        // A task that runs as its image says starts in its `workdir`, which is no mount, with a
        // file system that it may write.
        let access = Access {
            run_as: Some(task_model::RunAs::default()),
            ..Access::default()
        };
        let as_image = ContainerRun {
            access: &access,
            ..run(&image, &[], Some("/data"))
        };
        let script = job_script(
            &Container::Apptainer("apptainer".into()),
            &as_image,
            false,
            None,
            100,
        );
        let expected = "'apptainer' 'exec' '--containall' '--pwd' '/data' '--writable-tmpfs' \
            '--bind' '/shared/scratch/run-P-1-1:/shared/scratch/run-P-1-1' 'docker://";
        assert!(script.contains(expected), "{script}");
        // A task without network gets only loopback.
        let offline = ContainerRun {
            offline: true,
            ..run(&image, &[], None)
        };
        let isolated = command(&Container::Apptainer("singularity".into()), &offline, false);
        assert!(
            isolated
                .windows(3)
                .any(|words| words == ["--net", "--network", "none"])
        );
        let script = job_script(
            &Container::Apptainer("apptainer".into()),
            &offline,
            false,
            None,
            100,
        );
        assert!(
            script.contains(
                "'exec' '--containall' '--pwd' '/shared/scratch/run-P-1-1/work' \
                             '--net' '--network' 'none' "
            ),
            "{script}"
        );
        assert_eq!(super::image("/images/tool.sif"), "/images/tool.sif");
        assert_eq!(super::image("oras://host/tool:1"), "oras://host/tool:1");
    }

    #[test]
    fn podman_enforces_limits() {
        let image = spec(None).image.expect("an image");
        let engine = EngineCommand {
            program: "podman".into(),
            kind: EngineKind::Podman,
            rootless: true,
        };
        let script = job_script(
            &Container::Engine(engine),
            &run(&image, &[], None),
            false,
            None,
            100,
        );
        let command = (script.lines())
            .find(|line| line.starts_with("'podman'"))
            .expect("the command");
        let expected = "'podman' 'run' '--init' '--rm' '--name' 'iunctus-run-P-1-1' \
            '--entrypoint' 'sh' '--userns=keep-id' '--user' '1000:100' \
            '--cap-drop' 'ALL' '--security-opt' 'no-new-privileges' \
            '--volume' '/shared/scratch/run-P-1-1:/shared/scratch/run-P-1-1' \
            '--workdir' '/shared/scratch/run-P-1-1/work' '--cpus' '2' '--memory' '3221225472' \
            'debian@sha256:ab' '/shared/scratch/run-P-1-1/control/wrapper.sh'";
        assert_eq!(command, expected);
        // A task without network gets none.
        let offline = ContainerRun {
            offline: true,
            ..run(&image, &[], None)
        };
        let engine = EngineCommand::docker("docker");
        let script = job_script(&Container::Engine(engine), &offline, false, Some(90), 100);
        assert!(script.contains("'--network' 'none'"), "{script}");
        // A task with a time runs under `timeout`.
        assert!(
            script.contains("\ntimeout --kill-after=10 90 'docker' 'run' '--init' "),
            "{script}"
        );
        assert!(
            script.contains(
                "\nstatus=$?\n[ $status -eq 137 ] && [ $SECONDS -ge 90 ] && status=124\nfind "
            ),
            "{script}"
        );
    }
}
