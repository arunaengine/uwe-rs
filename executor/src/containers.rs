//! Groups task containers by image and coordinates their wrappers and dependencies.
// Copyright (c) 2026 The iunctus Contributors
// SPDX-License-Identifier: MIT or Apache-2.0

use std::path::{Path, PathBuf};

use task_model::{EnvironmentValue, Image, TaskSpec};

use crate::braces::expand_braces;
use crate::wrapper::{
    BASH, COLLECT, END, ENDED, EXIT, Folders, MANIFEST, NOW, START, STDERR, STDIN, STDOUT,
    USAGE_SCRIPT, hand_back, invalid_specification, quote, start_in,
};

/// The folder in the control folder that holds a folder for each container of a task that
/// runs several, with its wrapper and, besides the main container, its control files.
pub const CONTAINERS: &str = "containers";

/// The wrapper of each container, in its folder.
pub const WRAPPER: &str = "wrapper.sh";

/// The exit code of a container that did not run because a container it comes after failed,
/// as Argo's executor ends it with an error.
pub const DEPENDENCY_FAILED: i32 = 1;

/// Containers of a task that share an image, which run in one container of the engine.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Group {
    pub image: Image,
    /// Their positions in the containers of the task.
    pub members: Vec<usize>,
}

/// The containers of a task by their image, in the order of their first container: the
/// group of the main container comes first. `None` when a container has no image.
pub fn groups(spec: &TaskSpec) -> Option<Vec<Group>> {
    let mut groups: Vec<Group> = Vec::new();
    for (position, container) in spec.containers.iter().enumerate() {
        let image = container.image.clone()?;
        match groups.iter_mut().find(|group| group.image == image) {
            Some(group) => group.members.push(position),
            None => groups.push(Group {
                image,
                members: vec![position],
            }),
        }
    }
    Some(groups)
}

/// The folder of a container's control files, relative to the control folder: the control
/// folder itself for the main container.
pub fn own_folder(spec: &TaskSpec, position: usize) -> String {
    match position {
        0 => String::new(),
        _ => format!("{CONTAINERS}/{}", spec.containers[position].name),
    }
}

/// The wrapper of a container, relative to the control folder.
pub fn wrapper_path(spec: &TaskSpec, position: usize) -> String {
    format!("{CONTAINERS}/{}/{WRAPPER}", spec.containers[position].name)
}

/// The wrapper of a group of containers that share an image, relative to the control folder.
pub fn group_path(index: usize) -> String {
    format!("{CONTAINERS}/group-{index}.sh")
}

/// Whether the container at `position` comes after the one at `before`, directly or through
/// others.
fn comes_after(spec: &TaskSpec, position: usize, before: usize) -> bool {
    let containers = &spec.containers;
    let mut seen = vec![false; containers.len()];
    let mut open = vec![position];
    while let Some(next) = open.pop() {
        for name in &containers[next].after {
            if let Some(found) = containers.iter().position(|other| other.name == *name)
                && !seen[found]
            {
                if found == before {
                    return true;
                }
                seen[found] = true;
                open.push(found);
            }
        }
    }
    false
}

/// Orders sequential containers by dependencies, placing the main container as early as valid.
pub fn sequence(spec: &TaskSpec) -> Option<Vec<usize>> {
    let order = spec.container_order()?;
    let (after_main, before): (Vec<usize>, Vec<usize>) = (order.into_iter())
        .filter(|&position| position != 0)
        .partition(|&position| comes_after(spec, position, 0));
    Some(before.into_iter().chain([0]).chain(after_main).collect())
}

/// The wrapper of the container at `position` of a task that runs several, in the folders
/// that its container sees. Its arguments must be what the container runs, the entrypoint of
/// its image included. An invalid specification produces only a failure wrapper.
pub fn container_wrapper(spec: &TaskSpec, position: usize, folders: &Folders<'_>) -> String {
    if spec.validate().is_err() {
        return invalid_specification();
    }
    let container = &spec.containers[position];
    let main = position == 0;
    let own = own_folder(spec, position);
    let mut script = format!(
        "#!/bin/sh\n# The iunctus wrapper of container {} of a task.\nset -u\n",
        container.name
    );
    script.push_str(BASH);
    script.push_str(&format!(
        "work={}\ncontrol={}\noutbox={}\nown=\"$control\"{}\n",
        quote(folders.work),
        quote(folders.control),
        quote(folders.outbox),
        match own.is_empty() {
            true => String::new(),
            false => format!("/{}", quote(&own)),
        }
    ));
    script.push_str("[ -d \"$work\" ] && [ -d \"$own\" ] || mkdir -p \"$work\" \"$own\"\n");
    // Containers that run as Kubernetes runs them start where their image or their `workdir`
    // says, with the `HOME` of their user.
    let as_image = spec.run_as.is_some();
    if !as_image {
        script.push_str("cd \"$work\" || exit 1\n");
        script.push_str(&format!(
            "export HOME={}\n[ -d \"$HOME\" ] || mkdir -p \"$HOME\"\n",
            quote(folders.home)
        ));
    }
    if let Some(workdir) = &container.workdir {
        script.push_str(&start_in(workdir));
    }
    for (name, value) in &container.environment {
        let value = match value {
            EnvironmentValue::Literal(text) => quote(text),
            EnvironmentValue::WorkDirectory => "\"$work\"".to_owned(),
        };
        script.push_str(&format!("export {name}={value}\n"));
    }
    script.push_str(WAITED);
    script.push_str("status=0\n");
    for name in &container.after {
        let Some(found) = spec.containers.iter().position(|other| other.name == *name) else {
            continue;
        };
        let exit = exit_path(spec, found);
        script.push_str(&format!(
            "if [ \"$status\" -eq 0 ] && ! waited {exit}; then\n    status={DEPENDENCY_FAILED}\n    echo {} > \"$own/{STDERR}\"\nfi\n",
            quote(&format!(
                "iunctus: container {} did not run, since container {name} failed",
                container.name
            ))
        ));
    }
    let arguments: Vec<String> = container.arguments.iter().map(|word| quote(word)).collect();
    let stdin = match (main, spec.stdin.is_some()) {
        (true, true) => format!(" < \"$control/{STDIN}\""),
        _ => String::new(),
    };
    script.push_str(&format!("{NOW} > \"$own/{START}\"\n"));
    if container.service {
        script.push_str(&service_lines(spec, &arguments.join(" ")));
    } else {
        script.push_str(&format!(
        "if [ \"$status\" -eq 0 ]; then\n    attempt=1\n    : > \"$own/{STDOUT}\"\n    : > \"$own/{STDERR}\"\n    while :; do\n        {} >> \"$own/{STDOUT}\" 2>> \"$own/{STDERR}\"{stdin}\n        status=$?\n        {{ [ \"$status\" -eq 0 ] || [ \"$status\" -eq 127 ] || [ \"$attempt\" -ge {} ]; }} && break\n        attempt=$((attempt + 1))\n    done\nfi\n",
        arguments.join(" "),
        container.attempts.max(1)
    ));
    }
    script.push_str(&format!(
        "{NOW} > \"$own/{END}\"\necho \"$status\" > \"$own/{EXIT}\"\n"
    ));
    if main {
        // The outputs are complete when every container that does not come after the main
        // one has ended, whether it failed or not.
        for other in 1..spec.containers.len() {
            if !comes_after(spec, other, 0) && !spec.containers[other].service {
                script.push_str(&format!("waited {} || :\n", exit_path(spec, other)));
            }
        }
        script.push_str(USAGE_SCRIPT);
        script.push_str(COLLECT);
        script.push_str(&format!(": > \"$control/{MANIFEST}\"\n"));
        if as_image {
            script.push_str("cd \"$work\" || exit 1\n");
        }
        if !spec.outputs.is_empty() {
            script.push_str("if [ \"$status\" -eq 0 ]; then\n");
            for rule in &spec.outputs {
                for pattern in expand_braces(rule.pattern.trim_end_matches('/')) {
                    script.push_str(&format!("    collect {}\n", quote(&pattern)));
                }
            }
            script.push_str("fi\n");
        }
        if !folders.outbox.is_empty() {
            script.push_str("chmod -R a+rwX \"$work\" \"$outbox\" \"$control\" 2> /dev/null\n");
        }
    }
    script.push_str(&format!("echo {}\nexit 0\n", quote(ENDED)));
    script
}

/// The lines that run a service in the background until every other container of the task
/// ended, then stop it, as Argo stops sidecars; a service counts as ended well.
fn service_lines(spec: &TaskSpec, command: &str) -> String {
    let mut lines = format!(
        "if [ \"$status\" -eq 0 ]; then\n    : > \"$own/{STDOUT}\"\n    : > \"$own/{STDERR}\"\n    {command} >> \"$own/{STDOUT}\" 2>> \"$own/{STDERR}\" &\n    service=$!\nfi\n"
    );
    for (position, other) in spec.containers.iter().enumerate() {
        if !other.service {
            lines.push_str(&format!("waited {} || :\n", exit_path(spec, position)));
        }
    }
    // A service that ignores the stop is killed after five seconds.
    lines.push_str(
        "if [ -n \"${service:-}\" ]; then\n    kill \"$service\" 2> /dev/null\n    tries=0\n    while kill -0 \"$service\" 2> /dev/null && [ \"$tries\" -lt 50 ]; do\n        sleep 0.1\n        tries=$((tries + 1))\n    done\n    kill -9 \"$service\" 2> /dev/null\nfi\nstatus=0\n",
    );
    lines
}

/// Waits until a file of an exit code has one, then tells whether it is 0.
const WAITED: &str = r#"waited() {
    while [ ! -s "$1" ]; do
        sleep 0.1
    done
    read -r code < "$1"
    [ "$code" = 0 ]
}
"#;

/// The file of a container's exit code, as the wrapper names it.
fn exit_path(spec: &TaskSpec, position: usize) -> String {
    match own_folder(spec, position) {
        own if own.is_empty() => format!("\"$control/{EXIT}\""),
        own => format!("\"$control\"/{}", quote(&format!("{own}/{EXIT}"))),
    }
}

/// The file of a container's exit code, relative to the control folder.
pub fn exit_file(spec: &TaskSpec, position: usize) -> String {
    match own_folder(spec, position) {
        own if own.is_empty() => EXIT.to_owned(),
        own => format!("{own}/{EXIT}"),
    }
}

/// The lines that run `command` in the background and, when it fails, record its exit code in
/// each file of `exits` that has none yet.
pub fn recording_its_end(command: &str, exits: &[String]) -> String {
    let mut lines = format!(
        "{{
    {command}
    code=$?
"
    );
    for exit in exits {
        let exit = quote(exit);
        lines.push_str(&format!(
            "    [ \"$code\" -eq 0 ] || [ -s {exit} ] || echo \"$code\" > {exit}\n"
        ));
    }
    lines.push_str("    exit \"$code\"\n} &\n");
    lines
}

/// The wrapper of one container of the engine that runs the wrappers of a group of containers
/// at once, and ends when all of them ended.
pub fn group_wrapper(members: &[(String, String)], owner: &str, root: &str) -> String {
    // The script is the first process of its container, which ignores a stop that it has no
    // trap for; when it ends, the container ends with every process in it.
    let mut script = String::from(
        "#!/bin/sh\n# Containers of an iunctus task that share an image.\ntrap 'exit 143' TERM INT HUP\n",
    );
    for (wrapper, exit) in members {
        let command = format!("sh {} > /dev/null", quote(wrapper));
        script.push_str(&recording_its_end(&command, std::slice::from_ref(exit)));
    }
    script.push_str("wait\n");
    script.push_str(&hand_back(owner, &quote(root)));
    script.push_str(&format!("echo {}\nexit 0\n", quote(ENDED)));
    script
}

/// Writes the wrapper of every container and of every group of a task that runs several into
/// the control folder `control` of this machine.
pub fn write_wrappers(
    spec: &TaskSpec,
    control: &Path,
    folders: &Folders<'_>,
) -> std::io::Result<Vec<(Group, String)>> {
    let groups = groups(spec).ok_or_else(|| std::io::Error::other("a container has no image"))?;
    let inside = |relative: &str| format!("{}/{relative}", folders.control.trim_end_matches('/'));
    for position in 0..spec.containers.len() {
        let path = control.join(wrapper_path(spec, position));
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        std::fs::write(&path, container_wrapper(spec, position, folders))?;
    }
    let mut written = Vec::new();
    for (index, group) in groups.into_iter().enumerate() {
        let members: Vec<(String, String)> = (group.members.iter())
            .map(|&position| {
                let wrapper = inside(&wrapper_path(spec, position));
                (wrapper, inside(&exit_file(spec, position)))
            })
            .collect();
        let root = Path::new(folders.control)
            .parent()
            .unwrap_or(Path::new("/"));
        let script = group_wrapper(&members, folders.owner, &root.to_string_lossy());
        std::fs::write(control.join(group_path(index)), script)?;
        written.push((group, inside(&group_path(index))));
    }
    Ok(written)
}

/// How a container of a task that runs several ended, from its control files.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Ended {
    pub name: String,
    pub exit_code: Option<i32>,
    /// Its captured streams, when it got that far.
    pub stdout: Option<PathBuf>,
    pub stderr: Option<PathBuf>,
}

/// How the containers of a task that runs several ended, in the order of the task, the main
/// one first, from its control folder on this machine.
pub fn ended(spec: &TaskSpec, control: &Path) -> Vec<Ended> {
    (0..spec.containers.len())
        .map(|position| {
            let own = control.join(own_folder(spec, position));
            let exists = |name: &str| Some(own.join(name)).filter(|path| path.exists());
            Ended {
                name: spec.containers[position].name.clone(),
                exit_code: read_code(&own.join(EXIT)),
                stdout: exists(STDOUT),
                stderr: exists(STDERR),
            }
        })
        .collect()
}

/// The exit code that the wrappers of a task recorded in its control folder on this machine:
/// that of the main container when it failed, else that of the first other container that
/// failed, else 0. `None` when a container recorded none, so that the task did not end.
pub fn recorded(spec: &TaskSpec, control: &Path) -> Option<i32> {
    let main = read_code(&control.join(EXIT))?;
    if spec.containers.len() < 2 {
        return Some(main);
    }
    let others: Option<Vec<i32>> = (ended(spec, control).into_iter().skip(1))
        .map(|ended| ended.exit_code)
        .collect();
    let others = others?;
    let failed = others.into_iter().find(|code| *code != 0);
    Some(if main != 0 { main } else { failed.unwrap_or(0) })
}

fn read_code(path: &Path) -> Option<i32> {
    std::fs::read_to_string(path).ok()?.trim().parse().ok()
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeMap;

    use task_model::{ContainerSpec, FORMAT};

    use super::*;

    fn spec(containers: &[(&str, &str, &[&str])]) -> TaskSpec {
        let image = |reference: &str| Image {
            reference: reference.to_owned(),
            platform: None,
        };
        TaskSpec {
            format: FORMAT,
            namespace: String::new(),
            image: Some(image(containers[0].1)),
            arguments: vec!["true".into()],
            workdir: None,
            environment: BTreeMap::new(),
            inputs: Vec::new(),
            outputs: Vec::new(),
            stdin: None,
            containers: (containers.iter())
                .map(|(name, reference, after)| ContainerSpec {
                    name: (*name).to_owned(),
                    image: Some(image(reference)),
                    arguments: vec!["echo".into(), (*name).to_owned()],
                    workdir: None,
                    environment: BTreeMap::new(),
                    after: after.iter().map(|name| (*name).to_owned()).collect(),
                    attempts: 2,
                    replaces_entrypoint: false,
                    service: false,
                })
                .collect(),
            replaces_entrypoint: false,
            run_as: None,
        }
    }

    #[test]
    fn groups_same_images() {
        let spec = spec(&[
            ("main", "a", &["b"]),
            ("b", "b", &[]),
            ("c", "a", &["main"]),
        ]);
        let groups = groups(&spec).expect("groups");
        let members: Vec<&[usize]> = groups.iter().map(|group| &group.members[..]).collect();
        assert_eq!(members, [&[0, 2][..], &[1][..]]);
        assert_eq!(groups[0].image.reference, "a");
    }

    #[test]
    fn orders_main_container() {
        let spec = spec(&[
            ("main", "a", &["b"]),
            ("b", "a", &[]),
            ("c", "a", &["main"]),
            ("d", "a", &[]),
        ]);
        assert_eq!(sequence(&spec), Some(vec![1, 3, 0, 2]));
    }

    #[test]
    fn stops_service_containers() {
        let mut spec = spec(&[("main", "a", &[]), ("web", "b", &[])]);
        spec.containers[1].service = true;
        let folders = Folders {
            work: "/w",
            control: "/c",
            home: "/h",
            outbox: "",
            owner: "",
        };
        // The main container collects without waiting for the service, which never ends.
        let main = container_wrapper(&spec, 0, &folders);
        assert!(!main.contains("containers/web/exit"), "{main}");
        // The service runs in the background until the main container ended.
        let web = container_wrapper(&spec, 1, &folders);
        assert!(
            web.contains("'echo' 'web' >> \"$own/stdout\" 2>> \"$own/stderr\" &"),
            "{web}"
        );
        assert!(web.contains("waited \"$control/exit\" || :"), "{web}");
        assert!(web.contains("kill \"$service\""), "{web}");
        assert!(web.contains("status=0\n"), "{web}");
    }

    #[test]
    fn isolates_container_wrappers() {
        let spec = spec(&[
            ("main", "a", &["b"]),
            ("b", "a", &[]),
            ("c", "a", &["main"]),
        ]);
        let folders = Folders {
            work: "/w",
            control: "/c",
            home: "/h",
            outbox: "",
            owner: "",
        };
        let other = container_wrapper(&spec, 1, &folders);
        assert!(
            other.contains("own=\"$control\"/'containers/b'\n"),
            "{other}"
        );
        assert!(other.contains("[ \"$attempt\" -ge 2 ]"), "{other}");
        assert!(!other.contains("manifest"), "{other}");
        let main = container_wrapper(&spec, 0, &folders);
        assert!(main.contains("own=\"$control\"\n"), "{main}");
        assert!(
            main.contains("! waited \"$control\"/'containers/b/exit'"),
            "{main}"
        );
        // The main container waits for b before it collects, not for c, which comes after it.
        assert!(main.contains("waited \"$control\"/'containers/b/exit' || :"));
        assert!(!main.contains("containers/c/exit"), "{main}");
        assert!(main.contains(": > \"$control/manifest\""), "{main}");
        let members = [(
            "/c/containers/main/wrapper.sh".to_owned(),
            "/c/exit".to_owned(),
        )];
        let group = group_wrapper(&members, "", "/");
        let expected = "{\n    sh '/c/containers/main/wrapper.sh' > /dev/null\n    code=$?\n    \
            [ \"$code\" -eq 0 ] || [ -s '/c/exit' ] || echo \"$code\" > '/c/exit'\n    exit \
            \"$code\"\n} &\nwait\n";
        assert!(group.contains(expected), "{group}");
    }

    #[test]
    fn rejects_invalid_environment() {
        let mut spec = spec(&[("main", "a", &[]), ("other", "a", &[])]);
        let command = "BAD=value\nprintf injected";
        spec.containers[1]
            .environment
            .insert(command.into(), EnvironmentValue::Literal("value".into()));
        let folders = Folders {
            work: "/w",
            control: "/c",
            home: "/h",
            outbox: "",
            owner: "",
        };

        let text = container_wrapper(&spec, 1, &folders);
        assert_eq!(text, invalid_specification());
        assert!(!text.contains(command));
    }

    #[test]
    fn selects_container_failure() {
        let spec = spec(&[("main", "a", &[]), ("b", "a", &[]), ("c", "a", &[])]);
        let folder = tempfile::tempdir().expect("a folder");
        let control = folder.path();
        let write = |relative: &str, code: &str| {
            let path = control.join(relative);
            std::fs::create_dir_all(path.parent().expect("a parent")).expect("makes");
            std::fs::write(path, code).expect("writes");
        };
        write("exit", "0\n");
        write("containers/b/exit", "0\n");
        assert_eq!(recorded(&spec, control), None);
        write("containers/c/exit", "3\n");
        assert_eq!(recorded(&spec, control), Some(3));
        write("exit", "2\n");
        assert_eq!(recorded(&spec, control), Some(2));
        let ends = ended(&spec, control);
        assert_eq!(ends[0].exit_code, Some(2));
        assert_eq!(ends[2].name, "c");
        assert_eq!(ends[2].exit_code, Some(3));
        assert_eq!(ends[2].stdout, None);
    }
}
