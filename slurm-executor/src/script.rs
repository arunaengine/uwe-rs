//! The batch job of an attempt: the options of `sbatch`, and the job script that runs the
//! wrapper of the task in its container on a node.
// Copyright (c) 2026 The iunctus Contributors
// SPDX-License-Identifier: MIT or Apache-2.0

use std::path::Path;

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
    let mut options = vec![
        "--parsable".to_owned(),
        format!("--job-name=iunctus-{}", job.name),
        format!("--chdir={}", root.display()),
        format!("--output={}", root.join(CONTROL).join(LOG).display()),
        "--nodes=1".into(),
        "--ntasks=1".into(),
        format!("--cpus-per-task={}", placement.cpus.max(1)),
    ];
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

/// A time in the form that SLURM reads, `days-hours:minutes:seconds`.
fn time(seconds: u64) -> String {
    let (days, rest) = (seconds / 86_400, seconds % 86_400);
    let (hours, rest) = (rest / 3600, rest % 3600);
    format!("{days}-{hours:02}:{:02}:{:02}", rest / 60, rest % 60)
}

/// The job script: it runs the wrapper in the task's container, then records the exit code of
/// the container. Podman and Docker get an init process, which hands the signal that stops the
/// job to the task, so that the container ends with the job.
pub fn job_script(container: &Container, run: &ContainerRun<'_>, gpus: bool) -> String {
    let command = match container {
        Container::Apptainer(program) => apptainer(program, run, gpus),
        Container::Engine(engine) => {
            let mut command = vec![engine.program.clone()];
            let mut arguments = run.arguments(engine);
            arguments.insert(1, "--init".into());
            command.extend(arguments);
            command
        }
    };
    let words: Vec<String> = command.iter().map(|word| quote(word)).collect();
    let exit = run.root.join(CONTROL).join(CONTAINER_EXIT);
    format!(
        "#!/bin/bash\n# The iunctus job of one attempt of a task.\n{}\necho $? > {}\n",
        words.join(" "),
        quote(&exit.to_string_lossy())
    )
}

/// The command of Apptainer: a container with its own `/tmp`, home and environment, which sees
/// the scratch directory at the same path, the work directory, and the other mounts.
fn apptainer(program: &str, run: &ContainerRun<'_>, gpus: bool) -> Vec<String> {
    let mut command = vec![
        program.to_owned(),
        "exec".into(),
        "--containall".into(),
        "--pwd".into(),
        run.inside(),
    ];
    for bind in run.binds() {
        command.extend(["--bind".into(), executor::local::volume(&bind)]);
    }
    if gpus {
        command.push("--nv".into());
    }
    command.push(image(&run.image.reference));
    command.extend(["/bin/bash".into(), run.script()]);
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
    use executor::{EngineKind, Staging};
    use task_model::{Accelerator, FORMAT, Image, Placement, TaskSpec};

    use super::*;

    fn spec(workdir: Option<&str>) -> TaskSpec {
        TaskSpec {
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
        }
    }

    #[test]
    fn jobs_ask_for_what_their_tasks_need() {
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
    fn apptainer_runs_the_wrapper_with_the_folders_of_the_task() {
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
        );
        let expected = "#!/bin/bash\n# The iunctus job of one attempt of a task.\n\
            'apptainer' 'exec' '--containall' '--pwd' '/data' \
            '--bind' '/shared/scratch/run-P-1-1:/shared/scratch/run-P-1-1' \
            '--bind' '/shared/scratch/run-P-1-1/work:/data' \
            '--bind' '/shared/store/cas:/shared/store/cas:ro' '--nv' \
            'docker://debian@sha256:ab' '/bin/bash' \
            '/shared/scratch/run-P-1-1/control/wrapper.sh'\n\
            echo $? > '/shared/scratch/run-P-1-1/control/container-exit'\n";
        assert_eq!(script, expected);
        assert_eq!(super::image("/images/tool.sif"), "/images/tool.sif");
        assert_eq!(super::image("oras://host/tool:1"), "oras://host/tool:1");
    }

    #[test]
    fn podman_runs_the_wrapper_with_an_init_process_and_the_limits() {
        let image = spec(None).image.expect("an image");
        let engine = EngineCommand {
            program: "podman".into(),
            kind: EngineKind::Podman,
            rootless: true,
        };
        let script = job_script(&Container::Engine(engine), &run(&image, &[], None), false);
        let command = script.lines().nth(2).expect("the command");
        let expected = "'podman' 'run' '--init' '--rm' '--name' 'iunctus-run-P-1-1' \
            '--entrypoint' '/bin/bash' '--userns=keep-id' '--user' '1000:100' \
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
        let script = job_script(&Container::Engine(engine), &offline, false);
        assert!(script.contains("'--network' 'none'"), "{script}");
    }
}
