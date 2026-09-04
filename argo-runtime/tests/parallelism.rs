//! Admission limits across nested templates, hooks and child workflows.
// Copyright (c) 2026 The iunctus Contributors
// SPDX-License-Identifier: MIT or Apache-2.0

use workflow_graph::{Driver, Launch, Outcome, OutputFile, Task};

fn bounded(test: fn()) {
    let (sender, receiver) = std::sync::mpsc::channel();
    std::thread::spawn(move || {
        test();
        let _ = sender.send(());
    });
    receiver
        .recv_timeout(std::time::Duration::from_secs(15))
        .expect("parallelism regression did not finish within 15 seconds");
}

fn driver(folder: &std::path::Path, spec: &str) -> Box<dyn Driver> {
    let target = folder.join("workflow.yaml");
    std::fs::write(&target, format!("apiVersion: argoproj.io/v1alpha1\nkind: Workflow\nmetadata: {{name: limits}}\nspec:\n{spec}")).unwrap();
    argo_runtime::driver(&Launch {
        target,
        ..Launch::default()
    })
    .unwrap()
}

fn complete(run: &mut dyn Driver, task: Task) -> Vec<Task> {
    run.complete(Outcome {
        id: task.id,
        ..Outcome::default()
    })
    .unwrap()
}

#[test]
fn nested_limits_count_invocations_and_refill_in_both_completion_orders() {
    bounded(|| {
        let folder = tempfile::tempdir().unwrap();
        let spec = r#"  entrypoint: main
  parallelism: 3
  templates:
  - name: main
    parallelism: 2
    dag:
      tasks:
      - {name: left, template: branch}
      - {name: right, template: branch}
  - name: branch
    parallelism: 1
    steps:
    - - {name: a, template: leaf}
      - {name: b, template: leaf}
  - name: leaf
    container: {image: alpine, command: [echo]}
"#;
        for reverse in [false, true] {
            let mut run = driver(folder.path(), spec);
            let mut active = run.start().unwrap();
            assert_eq!(active.len(), 2);
            let first = active.remove(usize::from(reverse));
            let branch = first.name.rsplit_once('.').unwrap().0.to_owned();
            let next = complete(run.as_mut(), first);
            assert_eq!(next.len(), 1);
            assert_eq!(next[0].name, format!("{branch}.b"));
            active.extend(next);
            let mut count = 1;
            while let Some(task) = active.pop() {
                active.extend(complete(run.as_mut(), task));
                assert!(active.len() <= 2);
                count += 1;
            }
            assert_eq!(count, 4);
            assert!(run.finish().is_ok());
        }
        for (workflow, template, expected) in
            [("", "1", 1), ("  parallelism: 1\n", "0", 1), ("", "0", 2)]
        {
            let spec = spec.replace("  parallelism: 3\n", workflow).replace(
                "    parallelism: 2\n",
                &format!("    parallelism: {template}\n"),
            );
            let mut run = driver(folder.path(), &spec);
            let mut active = run.start().unwrap();
            assert_eq!(active.len(), expected);
            while let Some(task) = active.pop() {
                active.extend(complete(run.as_mut(), task));
                assert!(active.len() <= expected);
            }
            assert!(run.finish().is_ok());
        }
        let mut run = driver(
            folder.path(),
            &spec.replace("  parallelism: 3\n", "  parallelism: 0\n"),
        );
        assert!(run.start().unwrap().is_empty());
        assert!(run.finish().is_err());
        let spec = spec
            .replace("  parallelism: 3\n", "")
            .replace("    parallelism: 1\n", "    parallelism: 2\n");
        let mut run = driver(folder.path(), &spec);
        let mut active = run.start().unwrap();
        assert_eq!(active.len(), 4);
        while let Some(task) = active.pop() {
            active.extend(complete(run.as_mut(), task));
        }
        assert!(run.finish().is_ok());
    });
}

#[test]
fn workflow_and_step_hooks_share_leaf_admission() {
    bounded(|| {
        let folder = tempfile::tempdir().unwrap();
        let spec = r#"  entrypoint: main
  parallelism: 1
  hooks:
    running: {expression: 'workflow.status == "Running"', template: hook}
  templates:
  - name: main
    parallelism: 1
    steps:
    - - name: first
        template: leaf
        hooks:
          success: {expression: 'steps.first.status == "Succeeded"', template: hook}
      - {name: second, template: leaf}
  - name: leaf
    container: {image: alpine, command: [echo]}
  - name: hook
    container: {image: alpine, command: [echo]}
"#;
        let mut run = driver(folder.path(), spec);
        let mut active = run.start().unwrap();
        let mut names = Vec::new();
        while let Some(task) = active.pop() {
            names.push(task.name.clone());
            active.extend(complete(run.as_mut(), task));
            assert!(active.len() <= 1);
        }
        assert_eq!(names.len(), 4);
        assert!(names[1].contains("first.hooks.success"));
        assert!(names.iter().any(|name| name == "limits.hooks.running"));
        assert!(run.finish().is_ok());
    });
}

#[test]
fn child_resource_call_holds_the_enclosing_template_allowance() {
    bounded(|| {
        let folder = tempfile::tempdir().unwrap();
        let spec = r#"  entrypoint: main
  parallelism: 2
  templates:
  - name: main
    parallelism: 1
    dag:
      tasks:
      - {name: child, template: submit}
      - {name: sibling, template: leaf}
  - name: submit
    resource:
      action: create
      successCondition: status.phase == Succeeded
      manifest: |
        apiVersion: argoproj.io/v1alpha1
        kind: Workflow
        metadata: {name: child}
        spec:
          entrypoint: main
          parallelism: 1
          hooks:
            running: {expression: 'workflow.status == "Running"', template: leaf}
          templates:
          - name: main
            dag:
              tasks:
              - {name: a, template: leaf}
              - {name: b, template: leaf}
          - name: leaf
            container: {image: alpine, command: [echo]}
  - name: leaf
    container: {image: alpine, command: [echo]}
"#;
        for parent_first in [false, true] {
            let spec = if parent_first {
                spec.replace(
                "      - {name: child, template: submit}\n      - {name: sibling, template: leaf}",
                "      - {name: sibling, template: leaf}\n      - {name: child, template: submit}",
            )
            } else {
                spec.to_owned()
            };
            let mut run = driver(folder.path(), &spec);
            let mut active = run.start().unwrap();
            let mut names = Vec::new();
            while let Some(task) = active.pop() {
                names.push(task.name.clone());
                active.extend(complete(run.as_mut(), task));
                assert!(active.len() <= 1);
            }
            assert_eq!(names.len(), 4);
            let sibling = if parent_first {
                names.first()
            } else {
                names.last()
            };
            assert!(sibling.unwrap().ends_with("sibling"));
            assert!(names.iter().any(|name| name == "child.hooks.running"));
            assert!(run.finish().is_ok());
        }
        let spec = spec
            .replace("  parallelism: 2\n", "  parallelism: 1\n")
            .replacen("    parallelism: 1\n", "    parallelism: 2\n", 1)
            .replace("          parallelism: 1\n", "          parallelism: 2\n");
        let mut run = driver(folder.path(), &spec);
        let mut active = run.start().unwrap();
        assert_eq!(active.len(), 2);
        while let Some(task) = active.pop() {
            active.extend(complete(run.as_mut(), task));
        }
        assert!(run.finish().is_ok());
    });
}

#[test]
fn loops_refill_without_changing_skipped_omitted_or_empty_behavior() {
    bounded(|| {
        let folder = tempfile::tempdir().unwrap();
        let spec = r#"  entrypoint: main
  parallelism: 1
  templates:
  - name: main
    dag:
      tasks:
      - name: loop
        template: leaf
        withItems: [one, two]
        arguments: {parameters: [{name: item, value: '{{item}}'}]}
      - {name: skip, template: leaf, when: 'false', withParam: '{{missing}}'}
      - {name: omit, template: leaf, depends: 'skip.Succeeded'}
  - name: leaf
    inputs: {parameters: [{name: item, default: none}]}
    container: {image: alpine, command: [echo, '{{inputs.parameters.item}}']}
"#;
        for (list, count) in [("[one, two]", 2), ("[]", 0)] {
            let mut run = driver(folder.path(), &spec.replace("[one, two]", list));
            let mut active = run.start().unwrap();
            let mut names = Vec::new();
            while let Some(task) = active.pop() {
                names.push(task.command[1].clone());
                active.extend(complete(run.as_mut(), task));
                assert!(active.len() <= 1);
            }
            assert_eq!(names.len(), count);
            if count > 0 {
                assert_eq!(names, ["one", "two"]);
            }
            assert!(run.finish().is_ok());
        }
    });
}

#[test]
fn serialized_volume_updates_reach_the_next_admitted_writer() {
    bounded(|| {
        let folder = tempfile::tempdir().unwrap();
        let spec = r#"  entrypoint: main
  parallelism: 1
  volumes: [{name: data, persistentVolumeClaim: {claimName: data}}]
  templates:
  - name: main
    steps:
    - - {name: first, template: writer}
      - {name: second, template: writer}
      - {name: third, template: writer}
  - name: writer
    container:
      image: alpine
      command: [update-counter]
      volumeMounts: [{name: data, mountPath: /data}]
"#;
        let mut run = driver(folder.path(), spec);
        let tasks = run.start().unwrap();
        assert_eq!(tasks.len(), 1);
        let directory = folder.path().join("first");
        std::fs::create_dir(&directory).unwrap();
        std::fs::write(directory.join("counter"), "1").unwrap();
        let next = run
            .complete(Outcome {
                id: tasks[0].id,
                files: vec![OutputFile {
                    path: "/data".into(),
                    location: directory.to_string_lossy().into_owned(),
                    directory: true,
                    ..OutputFile::default()
                }],
                ..Outcome::default()
            })
            .unwrap();
        assert_eq!(next.len(), 1);
        assert_eq!(next[0].inputs[0].path, "/data/counter");
        assert_eq!(std::fs::read(&next[0].inputs[0].source).unwrap(), b"1");
        std::fs::write(directory.join("counter"), "2").unwrap();
        let next = run
            .complete(Outcome {
                id: next[0].id,
                files: vec![OutputFile {
                    path: "/data".into(),
                    location: directory.to_string_lossy().into_owned(),
                    directory: true,
                    ..OutputFile::default()
                }],
                ..Outcome::default()
            })
            .unwrap();
        assert_eq!(next.len(), 1);
        assert_eq!(next[0].inputs[0].path, "/data/counter");
        assert_eq!(std::fs::read(&next[0].inputs[0].source).unwrap(), b"2");
        assert!(complete(run.as_mut(), next.into_iter().next().unwrap()).is_empty());
        assert!(run.finish().is_ok());
    });
}
