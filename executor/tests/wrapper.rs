//! Runs the wrapper with the host's Bash and checks the control files and the manifest.
// Copyright (c) 2026 The uwe Contributors
// SPDX-License-Identifier: MIT or Apache-2.0

use std::collections::BTreeMap;
use std::path::Path;
use std::process::Command;

use executor::manifest::{Collected, parse};
use executor::wrapper::{ENDED, Folders, wrapper};
use task_model::{Digest, EnvironmentValue, FORMAT, OutputRule, TaskSpec};

fn spec(outputs: &[&str]) -> TaskSpec {
    let mut environment = BTreeMap::new();
    environment.insert(
        "GREETING".to_owned(),
        EnvironmentValue::Literal("it's me".to_owned()),
    );
    environment.insert("TASK_DIR".to_owned(), EnvironmentValue::WorkDirectory);
    TaskSpec {
        run_as: None,
        format: FORMAT,
        namespace: String::new(),
        image: None,
        arguments: vec!["/bin/bash".into(), "-ue".into(), "task script.sh".into()],
        workdir: None,
        environment,
        inputs: Vec::new(),
        outputs: outputs
            .iter()
            .map(|pattern| OutputRule {
                pattern: (*pattern).to_owned(),
            })
            .collect(),
        stdin: None,
        containers: Vec::new(),
        replaces_entrypoint: false,
    }
}

/// Writes the task script into the work directory and runs the wrapper; the result has the
/// wrapper's exit code. The wrapper's own output is only the line that says that it ended.
fn run(root: &Path, spec: &TaskSpec, script: &str) -> i32 {
    let work = root.join("work");
    let control = root.join("control");
    std::fs::create_dir_all(&work).expect("creates the work directory");
    std::fs::write(work.join("task script.sh"), script).expect("writes the script");
    let home = root.join("home");
    let folders = Folders {
        owner: "",
        work: &work.to_string_lossy(),
        control: &control.to_string_lossy(),
        home: &home.to_string_lossy(),
        outbox: "",
    };
    let text = wrapper(spec, &folders);
    let output = Command::new("bash")
        .arg("-c")
        .arg(text)
        .output()
        .expect("runs bash");
    let said = String::from_utf8_lossy(&output.stdout);
    assert_eq!(said, format!("{ENDED}\n"));
    output.status.code().expect("an exit code")
}

fn read(path: &Path) -> String {
    std::fs::read_to_string(path).expect("reads the file")
}

fn paths(files: &[Collected]) -> Vec<&str> {
    files.iter().map(|file| file.path.as_str()).collect()
}

#[test]
fn rejects_invalid_environment() {
    let root = tempfile::tempdir().expect("a temporary directory");
    let injected = root.path().join("injected");
    let command = format!("BAD=value\nprintf injected > {} #", injected.display());
    let mut spec = spec(&[]);
    spec.environment
        .insert(command.clone(), EnvironmentValue::Literal("value".into()));
    let folders = Folders {
        owner: "",
        work: "/unused/work",
        control: "/unused/control",
        home: "/unused/home",
        outbox: "",
    };

    let text = wrapper(&spec, &folders);
    assert!(!text.contains(&command));
    let output = Command::new("bash")
        .arg("-c")
        .arg(text)
        .output()
        .expect("runs the failure wrapper");
    assert_eq!(output.status.code(), Some(1));
    assert_eq!(
        String::from_utf8_lossy(&output.stderr),
        "iunctus: invalid task specification\n"
    );
    assert!(!injected.exists());
}

#[test]
fn collects_success_outputs() {
    let root = tempfile::tempdir().expect("a temporary directory");
    let script = "echo \"$GREETING in ${TASK_DIR##*/}\"\necho oops >&2\necho hi > out.txt\n\
                  mkdir -p dir/a && echo b > dir/a/b.txt\necho l > x.log\necho s > .hidden.log\n";
    let spec = spec(&["out.txt", "dir/", "*.log", "missing.txt"]);
    assert_eq!(run(root.path(), &spec, script), 0);
    let control = root.path().join("control");
    assert_eq!(read(&control.join("exit")), "0\n");
    // The captured streams stay out of the work directory, where a pattern could match them.
    assert_eq!(read(&control.join("stdout")), "it's me in work\n");
    assert_eq!(read(&control.join("stderr")), "oops\n");
    let files = parse(&read(&control.join("manifest"))).expect("parses");
    assert_eq!(paths(&files), ["out.txt", "dir", ".hidden.log", "x.log"]);
    assert_eq!(
        files[..2],
        [
            Collected {
                path: "out.txt".into(),
                directory: false,
            },
            Collected {
                path: "dir".into(),
                directory: true,
            }
        ]
    );
}

#[test]
fn expands_output_patterns() {
    let root = tempfile::tempdir().expect("a temporary directory");
    let elsewhere = root.path().join("elsewhere");
    let script = format!(
        "mkdir -p a/b && touch a/b/c.fq top.fq r.html r.zip r.txt\n\
         mkdir -p '{0}' && echo far > '{0}/far.txt'\n",
        elsewhere.display()
    );
    let far = format!("{}/*.txt", elsewhere.display());
    let spec = spec(&["**/*.fq", "r.{html,zip}", &far]);
    assert_eq!(run(root.path(), &spec, &script), 0);
    let control = root.path().join("control");
    let files = parse(&read(&control.join("manifest"))).expect("parses");
    let far = format!("{}/far.txt", elsewhere.display());
    assert_eq!(
        paths(&files),
        ["a/b/c.fq", "top.fq", "r.html", "r.zip", far.as_str()]
    );
    // An output at an absolute path is copied below `root` in the control directory.
    let copy = control.join("root").join(far.trim_start_matches('/'));
    assert_eq!(read(&copy), "far\n");
}

#[test]
fn exports_symlink_content() {
    let root = tempfile::tempdir().unwrap();
    let target = root.path().join("target");
    let output = root.path().join("link");
    std::fs::write(&target, "container output").unwrap();
    std::os::unix::fs::symlink(&target, &output).unwrap();
    let output = output.to_str().unwrap();
    assert_eq!(run(root.path(), &spec(&[output]), "true"), 0);
    let exported = root
        .path()
        .join("control/root")
        .join(output.trim_start_matches('/'));
    assert!(!std::fs::symlink_metadata(&exported).unwrap().is_symlink());
    std::fs::remove_file(target).unwrap();
    assert_eq!(read(&exported), "container output");
}

#[test]
fn provides_writable_home() {
    let root = tempfile::tempdir().expect("a temporary directory");
    let script = "echo \"$HOME\" > home.txt\necho x > \"$HOME/.settings\"\n";
    assert_eq!(run(root.path(), &spec(&["home.txt"]), script), 0);
    let home = root.path().join("home");
    assert_eq!(
        read(&root.path().join("work/home.txt")),
        format!("{}\n", home.display())
    );
    assert_eq!(read(&home.join(".settings")), "x\n");
    // A task that sets its own home keeps it, as CWL tools have theirs in the work directory.
    let mut own = spec(&["home.txt"]);
    own.environment
        .insert("HOME".to_owned(), EnvironmentValue::WorkDirectory);
    let other = tempfile::tempdir().expect("a temporary directory");
    assert_eq!(run(other.path(), &own, script), 0);
    let work = other.path().join("work");
    assert_eq!(
        read(&work.join("home.txt")),
        format!("{}\n", work.display())
    );
}

#[test]
fn retains_failure_logs() {
    let root = tempfile::tempdir().expect("a temporary directory");
    let spec = spec(&["out.txt"]);
    assert_eq!(run(root.path(), &spec, "echo hi > out.txt\nexit 3\n"), 0);
    let control = root.path().join("control");
    assert_eq!(read(&control.join("exit")), "3\n");
    assert_eq!(read(&control.join("manifest")), "");
    assert!(control.join("stdout").is_file() && control.join("stderr").is_file());
}

#[test]
fn reads_standard_input() {
    let root = tempfile::tempdir().expect("a temporary directory");
    let mut spec = spec(&["got.txt"]);
    spec.stdin = Some(Digest::sha256(b"piped\n"));
    let control = root.path().join("control");
    std::fs::create_dir_all(&control).expect("creates the control directory");
    std::fs::write(control.join("stdin"), "piped\n").expect("writes the standard input");
    assert_eq!(run(root.path(), &spec, "cat > got.txt\n"), 0);
    assert_eq!(read(&root.path().join("work/got.txt")), "piped\n");
}

#[test]
fn manifest_errors() {
    let text = "file a.txt\nfile a.txt\nfile 12 b.txt\ndirectory d e\n";
    let files = parse(text).expect("parses");
    assert_eq!(paths(&files), ["a.txt", "12 b.txt", "d e"]);
    let error = parse("a.txt\n").unwrap_err();
    assert_eq!(
        error.to_string(),
        "line 1 of the result manifest is not `file <path>` or `directory <path>`: a.txt"
    );
}

#[test]
fn copies_outbox_outputs() {
    let root = tempfile::tempdir().expect("a temporary directory");
    let work = root.path().join("work");
    let control = root.path().join("control");
    let outbox = root.path().join("outbox");
    std::fs::create_dir_all(&work).expect("creates the work directory");
    std::fs::write(root.path().join("input.txt"), "in\n").expect("writes the input");
    let script = "mkdir -p dir/a && echo b > dir/a/b.txt\nln -s ../input.txt linked.txt\n";
    std::fs::write(work.join("task script.sh"), script).expect("writes the script");
    let spec = spec(&["dir/", "linked.txt"]);
    let home = root.path().join("home");
    let folders = Folders {
        owner: "",
        work: &work.to_string_lossy(),
        control: &control.to_string_lossy(),
        home: &home.to_string_lossy(),
        outbox: &outbox.to_string_lossy(),
    };
    let text = wrapper(&spec, &folders);
    let status = Command::new("bash").arg("-c").arg(text).status();
    assert_eq!(status.expect("runs bash").code(), Some(0));
    assert_eq!(read(&outbox.join("dir/a/b.txt")), "b\n");
    // A link becomes a copy of what it points to.
    let linked = outbox.join("linked.txt");
    assert!(
        !std::fs::symlink_metadata(&linked)
            .expect("exists")
            .is_symlink()
    );
    assert_eq!(read(&linked), "in\n");
    // Every user may change what the task made, also when the task ran as root.
    use std::os::unix::fs::PermissionsExt;
    let mode = std::fs::metadata(outbox.join("dir/a"))
        .expect("exists")
        .permissions()
        .mode();
    assert_eq!(mode & 0o777, 0o777);
}

#[test]
fn stops_tool_promptly() {
    let root = tempfile::tempdir().expect("a temporary directory");
    let work = root.path().join("work");
    let control = root.path().join("control");
    std::fs::create_dir_all(&work).expect("creates the work directory");
    // The tool reads the wrapper's standard input, then sleeps until it is stopped.
    std::fs::write(work.join("task script.sh"), "cat > in.txt\nsleep 30\n").expect("writes");
    let home = root.path().join("home");
    let folders = Folders {
        owner: "",
        work: &work.to_string_lossy(),
        control: &control.to_string_lossy(),
        home: &home.to_string_lossy(),
        outbox: "",
    };
    let text = wrapper(&spec(&["in.txt"]), &folders);
    let started = std::time::Instant::now();
    let mut child = Command::new("bash")
        .arg("-c")
        .arg(text)
        .stdin(std::process::Stdio::piped())
        .stdout(std::process::Stdio::piped())
        .spawn()
        .expect("runs bash");
    use std::io::Write;
    let mut stdin = child.stdin.take().expect("standard input");
    stdin.write_all(b"given\n").expect("writes standard input");
    drop(stdin);
    while !work.join("in.txt").exists() || read(&work.join("in.txt")).is_empty() {
        std::thread::sleep(std::time::Duration::from_millis(50));
    }
    std::thread::sleep(std::time::Duration::from_millis(200));
    let stopped = Command::new("kill")
        .args(["-TERM", &child.id().to_string()])
        .status();
    assert!(stopped.expect("runs kill").success());
    let output = child.wait_with_output().expect("waits for bash");
    // The wrapper passed the stop on, recorded the tool's end and ended itself.
    assert!(started.elapsed().as_secs() < 10, "{:?}", started.elapsed());
    assert_eq!(output.status.code(), Some(0));
    assert_eq!(
        String::from_utf8_lossy(&output.stdout),
        format!("{ENDED}\n")
    );
    assert_eq!(read(&control.join("exit")), "143\n");
    assert_eq!(read(&work.join("in.txt")), "given\n");
}

#[test]
fn continues_killed_dependencies() {
    use task_model::{ContainerSpec, Image};
    let root = tempfile::tempdir().expect("a temporary directory");
    let [work, control, home] = ["work", "control", "home"].map(|name| root.path().join(name));
    let container = |name: &str, script: &str, after: &[&str]| ContainerSpec {
        service: false,
        replaces_entrypoint: false,
        name: name.into(),
        image: Some(Image {
            reference: "debian".into(),
            platform: None,
        }),
        arguments: vec!["sh".into(), "-c".into(), script.into()],
        workdir: None,
        environment: BTreeMap::new(),
        after: after.iter().map(|name| (*name).to_owned()).collect(),
        attempts: 1,
    };
    // `killed` kills its own wrapper, as the kernel does for lack of memory.
    let main = container("main", "echo ran > ran.txt", &["killed"]);
    let spec = TaskSpec {
        arguments: main.arguments.clone(),
        containers: vec![main, container("killed", "kill -9 $PPID", &[])],
        ..spec(&[])
    };
    let folders = Folders {
        owner: "",
        work: &work.to_string_lossy(),
        control: &control.to_string_lossy(),
        home: &home.to_string_lossy(),
        outbox: "",
    };
    let groups = executor::containers::write_wrappers(&spec, &control, &folders)
        .expect("writes the wrappers");
    let [(_, group)] = groups.as_slice() else {
        panic!("one group: {groups:?}")
    };
    let output = Command::new("timeout")
        .args(["60", "sh", group])
        .output()
        .expect("runs sh");
    assert_eq!(output.status.code(), Some(0), "{output:?}");
    let codes: Vec<Option<i32>> = (executor::containers::ended(&spec, &control).iter())
        .map(|ended| ended.exit_code)
        .collect();
    assert_eq!(codes, [Some(1), Some(137)]);
    assert!(!work.join("ran.txt").exists());
}
