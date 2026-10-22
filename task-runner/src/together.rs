//! Groups short tasks with matching images and resources into shared backend jobs.
// Copyright (c) 2026 The uwe Contributors
// SPDX-License-Identifier: MIT or Apache-2.0

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, PoisonError};
use std::time::Duration;

use bytes::Bytes;
use executor::wrapper::{self, Folders};
use executor::{Backend, CONTROL, ExecutorError, Finished, Job, Staged, Staging, Usage, WORK};
use task_model::{OutputRule, Placement, TaskSpec};
use tokio::sync::oneshot;
use tokio_util::sync::CancellationToken;

use crate::Runner;
use crate::inputs::Snapshots;

/// How long the first short task of a group waits for others that can join it.
const WINDOW: Duration = Duration::from_millis(250);

/// The script in the work directory of a group that runs its tasks.
const SCRIPT: &str = "together.sh";

/// The variable with the container options of a task, like its shared memory or GPUs, which
/// the backend reads.
const FLAGS: &str = "UWE_CONTAINER_FLAGS";

/// The line that ends the wrapper of a task in the script of its group.
const END_OF_WRAPPER: &str = "UWE_END_OF_WRAPPER";

/// What the wrapper of a task in a group starts its folders with: the folder of the group,
/// which its own path tells, since the wrapper lies in `<group>/m<number>/`.
const GROUP: &str = "@GROUP@/";

/// The short tasks that wait for their group to start, by what they need to share it.
pub(crate) type Gathering = Arc<Mutex<HashMap<String, Vec<Member>>>>;

/// A task in a group.
pub(crate) struct Member {
    name: String,
    spec: TaskSpec,
    staging: Staging,
    _snapshots: Snapshots,
    placement: Placement,
    /// How many tasks like this one may share a container.
    per_container: usize,
    cancel: CancellationToken,
    reply: oneshot::Sender<Result<Finished, ExecutorError>>,
}

impl Member {
    fn job(&self) -> Job<'_> {
        Job {
            name: &self.name,
            spec: &self.spec,
            staging: &self.staging,
            placement: &self.placement,
        }
    }

    /// Hands the result to the task. A task that stopped waiting leaves its folder behind,
    /// which goes.
    fn answer(self, result: Result<Finished, ExecutorError>) {
        if let Err(Ok(finished)) = self.reply.send(result) {
            let _ = std::fs::remove_dir_all(&finished.root);
        }
    }
}

/// Whether a task can run in a group: it names an image and a command, its work directory and
/// all its files are relative to the work directory, and it changes no input where it is.
pub(crate) fn joinable(job: &Job<'_>) -> bool {
    let relative = |path: &str| !path.starts_with('/') && path.trim_end_matches('/') != ".";
    let spec = job.spec;
    spec.image.is_some()
        && spec.containers.is_empty()
        && spec.output_manifest.is_none()
        && spec.replaces_entrypoint
        && job.placement.time_seconds.is_none()
        // A task that runs as its image says may run as another user, in another folder.
        && spec.run_as.is_none()
        && !spec.arguments.is_empty()
        && spec.workdir.is_none()
        && (spec.outputs.iter()).all(|rule| relative(&rule.pattern))
        && (job.staging.inputs.iter()).all(|input| relative(&input.path) && !input.in_place)
        && job.staging.directories.iter().all(|path| relative(path))
        // The script of the group ends the wrapper of each task with this line.
        && !(spec.arguments.iter()).any(|argument| argument.contains(END_OF_WRAPPER))
}

impl<B: Backend> Runner<B> {
    /// Runs a short task in a group with the other short tasks that come within
    /// [`WINDOW`] and can share its container, at most `per_container` in one group.
    pub(crate) async fn together(
        &self,
        job: Job<'_>,
        per_container: usize,
        cancel: &CancellationToken,
        snapshots: &Snapshots,
    ) -> Result<Finished, ExecutorError> {
        // Group output capture follows input mounts beneath each member directory.
        if job
            .staging
            .inputs
            .iter()
            .any(|input| !input.copy && self.inputs().references.contains(&input.source))
        {
            return self.backend.run(job, cancel).await;
        }
        let (reply, mut answer) = oneshot::channel();
        let key = key(&job);
        let member = Member {
            name: job.name.to_owned(),
            spec: job.spec.clone(),
            staging: job.staging.clone(),
            _snapshots: snapshots.clone(),
            placement: job.placement.clone(),
            per_container,
            cancel: cancel.clone(),
            reply,
        };
        let first = {
            let mut gathering = self
                .gathering
                .lock()
                .unwrap_or_else(PoisonError::into_inner);
            let members = gathering.entry(key.clone()).or_default();
            members.push(member);
            members.len() == 1
        };
        if first {
            let runner = self.clone();
            tokio::spawn(async move {
                tokio::time::sleep(WINDOW).await;
                let members = {
                    let mut gathering = runner
                        .gathering
                        .lock()
                        .unwrap_or_else(PoisonError::into_inner);
                    gathering.remove(&key).unwrap_or_default()
                };
                runner.start_groups(members);
            });
        }
        tokio::select! {
            answer = &mut answer => answer.unwrap_or_else(|_| Err(lost())),
            () = cancel.cancelled() => {
                if let Ok(Ok(finished)) = answer.await {
                    let _ = tokio::fs::remove_dir_all(&finished.root).await;
                }
                Err(ExecutorError::Cancelled)
            },
        }
    }

    /// Spreads the tasks that came together over groups and starts each group. A group holds as
    /// many tasks as fit into one start-up time; when there are more groups than the machine has
    /// room for at once, the groups get larger, so that they all run side by side.
    fn start_groups(&self, members: Vec<Member>) {
        let mut waiting = Vec::new();
        for member in members {
            match member.cancel.is_cancelled() {
                true => member.answer(Err(ExecutorError::Cancelled)),
                false => waiting.push(member),
            }
        }
        let Some(first) = waiting.first() else {
            return;
        };
        let room = (self.slots / first.placement.cpus.max(1) as usize).max(1);
        let fit = waiting
            .iter()
            .map(|member| member.per_container)
            .min()
            .unwrap_or(1);
        let size = waiting.len().div_ceil(room).max(fit);
        let mut groups = Vec::new();
        while !waiting.is_empty() {
            let rest = waiting.split_off(size.min(waiting.len()));
            groups.push(std::mem::replace(&mut waiting, rest));
        }
        for group in groups {
            let runner = self.clone();
            tokio::spawn(async move { runner.run_group(group).await });
        }
    }

    /// Runs a group, alone when it has one task, and hands each task its result.
    async fn run_group(&self, mut members: Vec<Member>) {
        if members.len() == 1 {
            let member = members.remove(0);
            let result = self.backend.run(member.job(), &member.cancel).await;
            member.answer(result);
            return;
        }
        if self.verbose_groups {
            println!(
                "[together] {} short tasks run one after another in one container",
                members.len()
            );
        }
        match self.group(&members).await {
            Ok(results) => {
                for (member, result) in members.into_iter().zip(results) {
                    member.answer(result);
                }
            }
            Err(error) => {
                for member in members {
                    member.answer(Err(copied(&error)));
                }
            }
        }
    }

    /// Runs the tasks of a group in one job of the backend, and gives each its folders back.
    async fn group(
        &self,
        members: &[Member],
    ) -> Result<Vec<Result<Finished, ExecutorError>>, ExecutorError> {
        let script = script(members);
        let stored = |error: crate::RunnerError| ExecutorError::Io(std::io::Error::other(error));
        let digest = (self.store.put_content(Bytes::from(script)).await)
            .map_err(|error| stored(error.into()))?;
        let script = self.local_content(&digest).await.map_err(stored)?;
        let (spec, staging, placement) = combined(members, script);
        let name = format!("{}-together", members[0].name);
        let job = Job {
            name: &name,
            spec: &spec,
            staging: &staging,
            placement: &placement,
        };
        // The group stops when every task in it stopped waiting.
        let cancel = CancellationToken::new();
        let all_cancelled = async {
            for member in members {
                member.cancel.cancelled().await;
            }
        };
        let run = self.backend.run(job, &cancel);
        tokio::pin!(run);
        let finished = tokio::select! {
            finished = &mut run => finished,
            () = all_cancelled => {
                cancel.cancel();
                run.await
            }
        }?;
        let scratch = (finished.root.parent()).map_or_else(PathBuf::new, Path::to_owned);
        let mut before = Usage::default();
        let mut results = Vec::new();
        for (number, member) in members.iter().enumerate() {
            let folder = finished.root.join(WORK).join(format!("m{number}"));
            let root = scratch.join(&member.name);
            let result = match tokio::fs::rename(&folder, &root).await {
                Ok(()) => own(root, &mut before).await,
                Err(error) => Err(ExecutorError::Io(error)),
            };
            results.push(result);
        }
        let _ = tokio::fs::remove_dir_all(&finished.root).await;
        Ok(results)
    }
}

/// What tasks must share to run in one container: the image, whether their commands replace
/// its entrypoint (the script of the group then runs without it, as each command would), the
/// resources they ask for, and the options of the container.
fn key(job: &Job<'_>) -> String {
    let placement = Placement {
        time_seconds: None,
        priority: 0,
        ..job.placement.clone()
    };
    let image = job.spec.image.as_ref().map(|image| &image.reference);
    let placement = serde_json::to_string(&placement).unwrap_or_default();
    let entrypoint = job.spec.replaces_entrypoint;
    let flags = job.spec.environment.get(FLAGS);
    format!("{image:?}\n{entrypoint}\n{placement}\n{flags:?}")
}

/// The script of a group: it writes the wrapper of each task into the folder of the task and
/// runs it, one after another, whatever the ones before gave. A stop ends the group.
fn script(members: &[Member]) -> String {
    let mut script = String::from(
        "#!/bin/sh\n# Short tasks of uwe, one after another in one container.\ngroup=$(pwd)\n",
    );
    for (number, member) in members.iter().enumerate() {
        let folder = format!("m{number}");
        let folders = |name: &str| format!("{GROUP}{folder}/{name}");
        let (work, control, home) = (folders(WORK), folders(CONTROL), folders(executor::HOME));
        let own = Folders {
            owner: "",
            work: &work,
            control: &control,
            home: &home,
            outbox: "",
        };
        // The folder of the group is where the wrapper lies, two levels up.
        let text =
            wrapper::wrapper(&member.spec, &own).replace(&format!("'{GROUP}"), "\"${0%/*/*}\"'/");
        script.push_str(&format!(
            "mkdir -p \"$group/{folder}\"\ncat > \"$group/{folder}/wrapper.sh\" <<'{END_OF_WRAPPER}'\n\
             {text}{END_OF_WRAPPER}\n{}[ -z \"${{stopped:-}}\" ] || exit 143\n",
            wrapper::stoppable(&format!("sh \"$group/{folder}/wrapper.sh\""))
        ));
    }
    script
}

/// The job of a group: the script as its command, the inputs of each task below the folder of
/// the task, and the folder of each task as an output. It asks for the most CPUs and memory
/// that one task asks for, and for the time of all tasks.
fn combined(members: &[Member], script: PathBuf) -> (TaskSpec, Staging, Placement) {
    let first = &members[0];
    let mut staging = Staging {
        inputs: vec![Staged {
            path: SCRIPT.into(),
            source: script,
            copy: true,
            ..Staged::default()
        }],
        ..Staging::default()
    };
    let mut outputs = Vec::new();
    let mut placement = first.placement.clone();
    for (number, member) in members.iter().enumerate() {
        let folder = format!("m{number}");
        for input in &member.staging.inputs {
            staging.inputs.push(Staged {
                path: format!("{folder}/{WORK}/{}", input.path),
                ..input.clone()
            });
        }
        (staging.directories).extend(
            (member.staging.directories.iter()).map(|path| format!("{folder}/{WORK}/{path}")),
        );
        if let Some(stdin) = &member.staging.stdin {
            staging.inputs.push(Staged {
                path: format!("{folder}/{CONTROL}/{}", wrapper::STDIN),
                source: stdin.clone(),
                ..Staged::default()
            });
        }
        outputs.push(OutputRule { pattern: folder });
        placement.priority = placement.priority.max(member.placement.priority);
    }
    // Each wrapper sets the environment of its task; the group keeps the container options.
    let flags = first.spec.environment.get_key_value(FLAGS);
    let spec = TaskSpec {
        arguments: vec!["sh".into(), SCRIPT.into()],
        workdir: None,
        environment: flags
            .map(|(name, value)| (name.clone(), value.clone()))
            .into_iter()
            .collect(),
        inputs: Vec::new(),
        outputs,
        stdin: None,
        ..first.spec.clone()
    };
    (spec, staging, placement)
}

/// What a task of a group left in its folders, now its own scratch directory. The counters of
/// the container add up over the tasks, so the task gets what grew since the task before it;
/// the peak of memory is the container's, not the task's, so it is left out.
async fn own(root: PathBuf, before: &mut Usage) -> Result<Finished, ExecutorError> {
    let finished = Finished::from_control(root.clone());
    let mut finished = match finished {
        Ok(finished) if finished.exit_code.is_some() => finished,
        Ok(_) => {
            let _ = tokio::fs::remove_dir_all(&root).await;
            return Err(ExecutorError::Engine {
                engine: "the container of the group".into(),
                message: "it stopped before this task ended".into(),
            });
        }
        Err(error) => {
            let _ = tokio::fs::remove_dir_all(&root).await;
            return Err(error);
        }
    };
    if let Some(now) = finished.usage {
        let grown = |now: Option<u64>, before: Option<u64>| {
            now.map(|now| now.saturating_sub(before.unwrap_or_default()))
        };
        finished.usage = Some(Usage {
            cpu_usec: grown(now.cpu_usec, before.cpu_usec),
            memory_peak: None,
            read_bytes: grown(now.read_bytes, before.read_bytes),
            write_bytes: grown(now.write_bytes, before.write_bytes),
            oom_kills: grown(now.oom_kills, before.oom_kills),
        });
        *before = now;
    }
    Ok(finished)
}

/// The error of a group, for each of its tasks.
fn copied(error: &ExecutorError) -> ExecutorError {
    match error {
        ExecutorError::Cancelled => ExecutorError::Cancelled,
        ExecutorError::Engine { engine, message } => ExecutorError::Engine {
            engine: engine.clone(),
            message: message.clone(),
        },
        ExecutorError::MissingImage { image, reason } => ExecutorError::MissingImage {
            image: image.clone(),
            reason: reason.clone(),
        },
        ExecutorError::NoShell(image) => ExecutorError::NoShell(image.clone()),
        other => ExecutorError::Io(std::io::Error::other(other.to_string())),
    }
}

/// The error of a task whose group ended without an answer for it.
fn lost() -> ExecutorError {
    ExecutorError::Engine {
        engine: "the group of short tasks".into(),
        message: "it ended without an answer for this task".into(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use task_model::{FORMAT, Image};

    fn member(
        name: &str,
        pattern: &str,
    ) -> (Member, oneshot::Receiver<Result<Finished, ExecutorError>>) {
        let (reply, answer) = oneshot::channel();
        let spec = TaskSpec {
            run_as: None,
            output_manifest: None,
            format: FORMAT,
            namespace: String::new(),
            image: Some(Image {
                reference: "debian:stable-slim".into(),
                platform: None,
            }),
            arguments: vec!["sh".into(), "-c".into(), "echo it's me > out.txt".into()],
            workdir: None,
            environment: Default::default(),
            inputs: Vec::new(),
            outputs: vec![OutputRule {
                pattern: pattern.into(),
            }],
            stdin: None,
            containers: Vec::new(),
            replaces_entrypoint: false,
        };
        let member = Member {
            name: name.into(),
            spec,
            staging: Staging {
                inputs: vec![Staged {
                    path: "in.txt".into(),
                    source: "/data/in.txt".into(),
                    ..Staged::default()
                }],
                directories: Vec::new(),
                stdin: Some("/data/stdin".into()),
            },
            placement: Placement {
                cpus: 1,
                ..Placement::default()
            },
            _snapshots: Snapshots::new(
                &crate::Settings {
                    downloads: "/unused".into(),
                    import_inputs: false,
                    storage: storage::Storage::default(),
                    digests: None,
                },
                "test",
            ),
            per_container: 4,
            cancel: CancellationToken::new(),
            reply,
        };
        (member, answer)
    }

    #[test]
    fn isolates_group_folders() {
        let (first, _) = member("r-A-1-1", "out.txt");
        let (second, _) = member("r-A-2-1", "out.txt");
        let members = [first, second];
        let (spec, staging, placement) = combined(&members, "/store/script".into());
        assert_eq!(spec.arguments, ["sh", SCRIPT]);
        let patterns: Vec<&str> = spec
            .outputs
            .iter()
            .map(|rule| rule.pattern.as_str())
            .collect();
        assert_eq!(patterns, ["m0", "m1"]);
        let paths: Vec<&str> = staging
            .inputs
            .iter()
            .map(|input| input.path.as_str())
            .collect();
        assert_eq!(
            paths,
            [
                SCRIPT,
                "m0/work/in.txt",
                "m0/control/stdin",
                "m1/work/in.txt",
                "m1/control/stdin"
            ]
        );
        assert_eq!(placement.time_seconds, None);
        let text = script(&members);
        assert!(text.contains("work=\"${0%/*/*}\"'/m1/work'"), "{text}");
        assert!(text.contains("sh \"$group/m1/wrapper.sh\""), "{text}");
        assert!(!text.contains(GROUP), "{text}");
    }

    #[test]
    fn isolates_absolute_tasks() {
        let (mut task, _) = member("r-A-1-1", "out.txt");
        task.spec.replaces_entrypoint = true;
        task.placement.time_seconds = None;
        assert!(joinable(&task.job()));
        task.spec.outputs[0].pattern = "/results/out.txt".into();
        assert!(!joinable(&task.job()));
        let (mut task, _) = member("r-A-1-1", "out.txt");
        task.spec.replaces_entrypoint = true;
        task.placement.time_seconds = None;
        task.spec.workdir = Some("/work".into());
        assert!(!joinable(&task.job()));
        let (mut task, _) = member("r-A-1-1", "out.txt");
        task.spec.replaces_entrypoint = true;
        task.placement.time_seconds = None;
        task.spec.run_as = Some(task_model::RunAs::default());
        assert!(!joinable(&task.job()));
    }

    #[test]
    fn separates_replaced_entrypoints() {
        let (first, _) = member("r-A-1-1", "out.txt");
        let (mut second, _) = member("r-A-2-1", "out.txt");
        assert_eq!(key(&first.job()), key(&second.job()));
        second.spec.replaces_entrypoint = true;
        assert_ne!(key(&first.job()), key(&second.job()));
    }

    #[test]
    fn keeps_container_flags() {
        let flags = |text: &str| task_model::EnvironmentValue::Literal(text.into());
        let (mut first, _) = member("r-A-1-1", "out.txt");
        let (mut second, _) = member("r-A-2-1", "out.txt");
        first
            .spec
            .environment
            .insert(FLAGS.into(), flags(r#"{"shm":2147483648}"#));
        assert_ne!(
            key(&first.job()),
            key(&second.job()),
            "other flags, other group"
        );
        second.spec.environment = first.spec.environment.clone();
        second.spec.environment.insert("OWN".into(), flags("value"));
        assert_eq!(key(&first.job()), key(&second.job()));
        let (spec, _, _) = combined(&[first, second], "/store/script".into());
        let names: Vec<&String> = spec.environment.keys().collect();
        assert_eq!(
            names,
            [FLAGS],
            "the group keeps the flags, the wrappers the rest"
        );
        assert_eq!(spec.environment[FLAGS], flags(r#"{"shm":2147483648}"#));
    }

    #[tokio::test]
    async fn references_bypass_groups() {
        use std::os::unix::fs::PermissionsExt;

        struct Direct;
        impl Backend for Direct {
            async fn pin(&self, _: &str) -> Result<Option<String>, ExecutorError> {
                Ok(None)
            }
            async fn run(
                &self,
                job: Job<'_>,
                _: &CancellationToken,
            ) -> Result<Finished, ExecutorError> {
                assert!(matches!(job.name, "first" | "second"));
                assert_eq!(
                    job.spec.outputs,
                    [OutputRule {
                        pattern: "out.txt".into()
                    }]
                );
                assert_eq!(job.staging.inputs.len(), 1);
                assert_eq!(job.staging.inputs[0].path, "database");
                assert!(!job.staging.inputs[0].copy);
                assert!(job.staging.inputs[0].directory);
                Ok(Finished {
                    root: job.name.into(),
                    exit_code: Some(0),
                    files: Vec::new(),
                    usage: None,
                    address: None,
                })
            }
        }
        let root = tempfile::tempdir().unwrap();
        let reference = root.path().join("reference");
        std::fs::create_dir(&reference).unwrap();
        let file = reference.join("genome");
        std::fs::write(&file, b"ACGT").unwrap();
        std::fs::set_permissions(&file, std::fs::Permissions::from_mode(0o444)).unwrap();
        std::fs::set_permissions(&reference, std::fs::Permissions::from_mode(0o555)).unwrap();
        let mut references = crate::References::default();
        references
            .register(
                &reference,
                crate::reference_digest(&reference).await.unwrap(),
            )
            .await
            .unwrap();
        let settings = crate::Settings {
            downloads: root.path().join("downloads"),
            import_inputs: true,
            storage: storage::Storage::default(),
            digests: None,
        };
        let mut runner = Runner::new(
            content_store::Store::local(&root.path().join("store")).unwrap(),
            Direct,
            "test",
            settings,
        );
        runner.references(references);
        let (mut first, _) = member("first", "out.txt");
        let (mut second, _) = member("second", "out.txt");
        for member in [&mut first, &mut second] {
            member.spec.replaces_entrypoint = true;
            member.staging.stdin = None;
            member.staging.inputs[0] = Staged {
                path: "database".into(),
                source: reference.clone(),
                directory: true,
                ..Staged::default()
            };
            assert!(joinable(&member.job()));
        }
        let (first_result, second_result) = tokio::join!(
            runner.together(first.job(), 4, &first.cancel, &first._snapshots),
            runner.together(second.job(), 4, &second.cancel, &second._snapshots)
        );
        assert_eq!(first_result.unwrap().root, Path::new("first"));
        assert_eq!(second_result.unwrap().root, Path::new("second"));
        assert!(runner.gathering.lock().unwrap().is_empty());
        std::fs::set_permissions(reference, std::fs::Permissions::from_mode(0o755)).unwrap();
    }

    #[tokio::test]
    async fn cancellation_retains_snapshots() {
        struct Waiting {
            root: PathBuf,
            started: tokio::sync::Notify,
            release: tokio::sync::Notify,
        }
        impl Backend for Waiting {
            async fn pin(&self, _: &str) -> Result<Option<String>, ExecutorError> {
                Ok(None)
            }
            async fn run(
                &self,
                job: Job<'_>,
                _: &CancellationToken,
            ) -> Result<Finished, ExecutorError> {
                self.started.notify_one();
                self.release.notified().await;
                for input in &job.staging.inputs {
                    std::fs::read(&input.source)?;
                }
                let root = self.root.join(job.name);
                for index in 0..2 {
                    let member = root.join(WORK).join(format!("m{index}"));
                    std::fs::create_dir_all(member.join(CONTROL))?;
                    std::fs::create_dir_all(member.join(WORK))?;
                    std::fs::write(member.join(CONTROL).join(wrapper::EXIT), "0")?;
                }
                Ok(Finished {
                    root,
                    exit_code: Some(0),
                    files: Vec::new(),
                    usage: None,
                    address: None,
                })
            }
        }
        let root = tempfile::tempdir().unwrap();
        let settings = crate::Settings {
            downloads: root.path().join("downloads"),
            import_inputs: false,
            storage: storage::Storage::default(),
            digests: None,
        };
        let runner = Runner::new(
            content_store::Store::local(&root.path().join("store")).unwrap(),
            Waiting {
                root: root.path().join("scratch"),
                started: Default::default(),
                release: Default::default(),
            },
            "test",
            settings.clone(),
        );
        let (mut first, first_result) = member("first", "out");
        let (mut second, second_result) = member("second", "out");
        let mut paths = Vec::new();
        for member in [&mut first, &mut second] {
            let snapshots = Snapshots::new(&settings, &member.name);
            let path = snapshots.folder().await.unwrap().join("input");
            std::fs::write(&path, b"input").unwrap();
            member.staging.stdin = None;
            member.staging.inputs[0].source = path.clone();
            member._snapshots = snapshots;
            paths.push(path);
        }
        let cancelled = second.cancel.clone();
        runner.start_groups(vec![first, second]);
        runner.backend().started.notified().await;
        cancelled.cancel();
        drop(second_result);
        assert!(paths.iter().all(|path| path.exists()));
        runner.backend().release.notify_one();
        assert!(first_result.await.unwrap().is_ok());
    }
}
