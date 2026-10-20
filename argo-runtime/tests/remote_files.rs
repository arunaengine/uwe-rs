//! Remote files and completion ordering of shared Argo volumes.
// Copyright (c) 2026 The iunctus Contributors
// SPDX-License-Identifier: MIT or Apache-2.0

use std::path::{Path, PathBuf};
use std::sync::Arc;

use workflow_graph::{Delivery, Driver, Files, Launch, Outcome, OutputFile, Task};

struct Elsewhere(PathBuf);

impl Files for Elsewhere {
    fn local(&self, location: &str) -> std::io::Result<PathBuf> {
        Ok(match location.strip_prefix("s3://store/") {
            Some(key) => self.0.join(key),
            None => PathBuf::from(location),
        })
    }

    fn deliver(&self, _: &Delivery) -> std::io::Result<()> {
        unreachable!("these driver tests do not deliver files")
    }
}

fn driver(folder: &Path, spec: &str) -> Box<dyn Driver> {
    let target = folder.join("workflow.yaml");
    std::fs::write(&target, format!(
        "apiVersion: argoproj.io/v1alpha1\nkind: Workflow\nmetadata:\n  name: remote\nspec:\n{spec}"
    )).unwrap();
    let mut driver = argo_runtime::driver(&Launch {
        target,
        ..Launch::default()
    })
    .unwrap();
    driver.use_files(Arc::new(Elsewhere(folder.to_owned())));
    driver
}

fn outcome(task: &Task, location: &str, directory: bool) -> Outcome {
    Outcome {
        id: task.id,
        files: vec![OutputFile {
            path: task.outputs[0].clone(),
            location: location.to_owned(),
            directory,
            ..OutputFile::default()
        }],
        ..Outcome::default()
    }
}

#[test]
fn reads_child_parameters() {
    // Argo checks simple tags of the manifest against the outer template, so the child uses
    // expression tags.
    let folder = tempfile::tempdir().unwrap();
    std::fs::write(folder.path().join("items"), "[\"a\",\"b\"]").unwrap();
    let spec = r#"  entrypoint: submit
  templates:
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
          templates:
          - name: main
            steps:
            - - {name: list, template: list}
            - - name: use
                template: use
                withParam: "{{=steps.list.outputs.parameters.items}}"
                arguments: {parameters: [{name: item, value: "{{item}}"}]}
          - name: list
            container: {image: alpine, command: [list]}
            outputs: {parameters: [{name: items, valueFrom: {path: /items}}]}
          - name: use
            inputs: {parameters: [{name: item}]}
            container: {image: alpine, command: [echo, "{{=inputs.parameters.item}}"]}
"#;
    let mut run = driver(folder.path(), spec);
    let tasks = run.start().unwrap();
    let next = run
        .complete(outcome(&tasks[0], "s3://store/items", false))
        .unwrap();
    assert_eq!(next.len(), 2);
    assert_eq!(next[0].command, ["echo", "a"]);
    assert_eq!(next[1].command, ["echo", "b"]);

    let mut run = driver(folder.path(), spec);
    let tasks = run.start().unwrap();
    assert!(
        run.complete(outcome(&tasks[0], "s3://store/missing", false))
            .unwrap()
            .is_empty()
    );
    assert!(run.finish().is_err());
}

#[test]
fn propagates_remote_volumes() {
    let folder = tempfile::tempdir().unwrap();
    let host = folder.path().join("host");
    let output = folder.path().join("output");
    std::fs::create_dir_all(&host).unwrap();
    std::fs::create_dir_all(&output).unwrap();
    std::fs::write(output.join("a.txt"), "remote bytes").unwrap();
    let spec = format!(
        r#"  entrypoint: main
  volumes:
  - name: data
    hostPath: {{path: {}, type: Directory}}
  templates:
  - name: main
    steps:
    - - {{name: write, template: mounted}}
    - - {{name: read, template: mounted}}
  - name: mounted
    container:
      image: alpine
      command: [echo]
      volumeMounts: [{{name: data, mountPath: /data}}]
"#,
        host.display()
    );
    let mut run = driver(folder.path(), &spec);
    let tasks = run.start().unwrap();
    let next = run
        .complete(outcome(&tasks[0], "s3://store/output", true))
        .unwrap();
    assert_eq!(
        std::fs::read_to_string(host.join("a.txt")).unwrap(),
        "remote bytes"
    );
    let input = next[0]
        .inputs
        .iter()
        .find(|input| input.path == "/data")
        .unwrap();
    let file = std::path::Path::new(&input.source).join("a.txt");
    assert_eq!(std::fs::read_to_string(file).unwrap(), "remote bytes");
}

#[test]
fn avoids_init_replay() {
    let folder = tempfile::tempdir().unwrap();
    for (name, content) in [("old", "old"), ("new", "new")] {
        std::fs::create_dir(folder.path().join(name)).unwrap();
        std::fs::write(folder.path().join(name).join("x"), content).unwrap();
    }
    let spec = r#"  entrypoint: main
  volumes: [{name: data, persistentVolumeClaim: {claimName: data}}]
  templates:
  - name: main
    steps:
    - - {name: initialized, template: initialized}
      - {name: concurrent, template: mounted}
    - - {name: read, template: mounted}
  - name: initialized
    initContainers:
    - name: init
      image: alpine
      command: [echo]
      volumeMounts: [{name: data, mountPath: /data}]
    container: {image: alpine, command: [echo]}
  - name: mounted
    container:
      image: alpine
      command: [echo]
      volumeMounts: [{name: data, mountPath: /data}]
"#;
    let mut run = driver(folder.path(), spec);
    let tasks = run.start().unwrap();
    let init = tasks
        .iter()
        .find(|task| task.name.contains("(init "))
        .unwrap();
    let concurrent = tasks
        .iter()
        .find(|task| task.name.ends_with(".concurrent"))
        .unwrap();
    let main = run.complete(outcome(init, "s3://store/old", true)).unwrap();
    assert_eq!(main.len(), 1);
    assert!(
        run.complete(outcome(concurrent, "s3://store/new", true))
            .unwrap()
            .is_empty()
    );
    let reader = run
        .complete(Outcome {
            id: main[0].id,
            ..Outcome::default()
        })
        .unwrap();
    let input = reader[0]
        .inputs
        .iter()
        .find(|input| input.path == "/data")
        .unwrap();
    let file = std::path::Path::new(&input.source).join("x");
    assert_eq!(std::fs::read_to_string(file).unwrap(), "new");
}

#[test]
fn selects_host_subpaths() {
    let folder = tempfile::tempdir().unwrap();
    let host = folder.path().join("host");
    std::fs::create_dir_all(host.join("selected")).unwrap();
    std::fs::write(host.join("root.txt"), "root").unwrap();
    std::fs::write(host.join("selected/input.txt"), "selected").unwrap();
    let spec = |sub_path: &str| {
        format!(
            r#"  entrypoint: mounted
  volumes: [{{name: data, hostPath: {{path: {}, type: Directory}}}}]
  templates:
  - name: mounted
    container:
      image: alpine
      command: [echo]
      volumeMounts: [{{name: data, mountPath: /data, subPath: {sub_path}}}]
"#,
            host.display()
        )
    };
    let mut run = driver(folder.path(), &spec("./selected"));
    let tasks = run.start().unwrap();
    // A writable host path comes as a copy of the selected folder.
    assert!(tasks[0].inputs[0].directory);
    let copy = std::path::Path::new(&tasks[0].inputs[0].source).join("input.txt");
    assert_eq!(std::fs::read_to_string(copy).unwrap(), "selected");
    let output = folder.path().join("output");
    std::fs::create_dir(&output).unwrap();
    std::fs::write(output.join("input.txt"), "selected").unwrap();
    std::fs::write(output.join("new.txt"), "new").unwrap();
    run.complete(outcome(&tasks[0], "s3://store/output", true))
        .unwrap();
    assert_eq!(
        std::fs::read_to_string(host.join("selected/new.txt")).unwrap(),
        "new"
    );
    assert!(!host.join("new.txt").exists());
    assert_eq!(
        std::fs::read_to_string(host.join("root.txt")).unwrap(),
        "root"
    );

    let mut run = driver(folder.path(), &spec("selected/input.txt"));
    let tasks = run.start().unwrap();
    assert_eq!(
        tasks[0].inputs[0].source,
        host.join("selected/input.txt").to_string_lossy()
    );
    assert!(!tasks[0].inputs[0].directory);
    assert!(tasks[0].outputs.is_empty());
    for sub_path in ["../outside", "/outside"] {
        let mut run = driver(folder.path(), &spec(sub_path));
        assert!(run.start().is_err());
    }
}

#[test]
fn preserves_host_source() {
    let folder = tempfile::tempdir().unwrap();
    let host = folder.path().join("host-file");
    std::fs::write(&host, "file bytes").unwrap();
    let spec = format!(
        r#"  entrypoint: mounted
  volumes: [{{name: data, hostPath: {{path: {}, type: File}}}}]
  templates:
  - name: mounted
    container:
      image: alpine
      command: [echo]
      volumeMounts: [{{name: data, mountPath: /data}}]
"#,
        host.display()
    );
    let mut run = driver(folder.path(), &spec);
    let tasks = run.start().unwrap();
    assert_eq!(tasks[0].inputs[0].source, host.to_string_lossy());
    assert!(!tasks[0].inputs[0].directory);
    assert!(tasks[0].outputs.is_empty());
}

#[cfg(unix)]
#[test]
fn rejects_redirected_writeback() {
    let folder = tempfile::tempdir().unwrap();
    let host = folder.path().join("host");
    let outside = folder.path().join("outside");
    let output = folder.path().join("output");
    for path in [&host, &outside, &output] {
        std::fs::create_dir(path).unwrap();
    }
    std::fs::write(outside.join("x"), "untouched").unwrap();
    std::os::unix::fs::symlink(&outside, host.join("redirect")).unwrap();
    std::fs::create_dir(output.join("redirect")).unwrap();
    std::fs::write(output.join("redirect/x"), "replacement").unwrap();
    let spec = format!(
        r#"  entrypoint: mounted
  volumes: [{{name: data, hostPath: {{path: {}, type: Directory}}}}]
  templates:
  - name: mounted
    container:
      image: alpine
      command: [echo]
      volumeMounts: [{{name: data, mountPath: /data}}]
"#,
        host.display()
    );
    let mut run = driver(folder.path(), &spec);
    let tasks = run.start().unwrap();
    run.complete(outcome(&tasks[0], "s3://store/output", true))
        .unwrap();
    assert!(run.finish().unwrap_err().to_string().contains("escapes"));
    assert_eq!(
        std::fs::read_to_string(outside.join("x")).unwrap(),
        "untouched"
    );
}

#[test]
fn requires_selected_keys() {
    let folder = tempfile::tempdir().unwrap();
    for (key, optional) in [("present", false), ("missing", false), ("missing", true)] {
        let spec = format!(
            r#"  entrypoint: mounted
  volumes:
  - name: config
    configMap: {{name: settings, optional: {optional}, items: [{{key: {key}, path: chosen}}]}}
  templates:
  - name: mounted
    container:
      image: alpine
      command: [echo]
      volumeMounts: [{{name: config, mountPath: /config}}]
---
apiVersion: v1
kind: ConfigMap
metadata: {{name: settings}}
data: {{present: contents}}
"#
        );
        let mut run = driver(folder.path(), &spec);
        let tasks = run.start();
        if key == "missing" && !optional {
            assert!(tasks.unwrap_err().to_string().contains("missing ConfigMap"));
        } else {
            let tasks = tasks.unwrap();
            assert_eq!(tasks[0].inputs[0].directory, optional);
            if !optional {
                assert_eq!(tasks[0].inputs[0].content.as_deref(), Some("contents"));
                assert_eq!(tasks[0].inputs[0].path, "/config/chosen");
            }
        }
    }
}

#[test]
fn unpacks_input_archives() {
    let folder = tempfile::tempdir().unwrap();
    let tree = folder.path().join("tree");
    std::fs::create_dir(&tree).unwrap();
    std::fs::write(tree.join("a"), "a").unwrap();
    std::fs::write(tree.join("b"), "b").unwrap();
    let packed = std::fs::File::create(folder.path().join("tree.tgz")).unwrap();
    let encoder = flate2::write::GzEncoder::new(packed, flate2::Compression::default());
    let mut archive = tar::Builder::new(encoder);
    archive.append_dir_all("tree", &tree).unwrap();
    archive.into_inner().unwrap().finish().unwrap();
    let zipped = std::fs::File::create(folder.path().join("files.zip")).unwrap();
    let mut zip = zip::ZipWriter::new(zipped);
    for name in ["x", "y"] {
        zip.start_file(name, zip::write::SimpleFileOptions::default())
            .unwrap();
        std::io::Write::write_all(&mut zip, name.as_bytes()).unwrap();
    }
    zip.finish().unwrap();
    std::fs::write(folder.path().join("tool"), "#!/bin/sh\n").unwrap();
    let spec = r#"  entrypoint: main
  templates:
  - name: main
    inputs:
      artifacts:
      - {name: tree, path: /in/tree, s3: {bucket: store, key: tree.tgz}}
      - {name: plain, path: /in/plain, archive: {none: {}}, s3: {bucket: store, key: tree.tgz}}
      - {name: files, path: /in/files, archive: {zip: {}}, s3: {bucket: store, key: files.zip}}
      - {name: tool, path: /in/tool, mode: 0755, s3: {bucket: store, key: tool}}
    container: {image: alpine, command: [ls, /in]}
"#;
    let mut run = driver(folder.path(), spec);
    let tasks = run.start().unwrap();
    let input = |path: &str| {
        let found = tasks[0].inputs.iter().find(|input| input.path == path);
        found.unwrap_or_else(|| panic!("no input {path}")).clone()
    };
    let names = |source: &str| {
        let mut names: Vec<String> = std::fs::read_dir(source)
            .unwrap()
            .map(|entry| entry.unwrap().file_name().to_string_lossy().into_owned())
            .collect();
        names.sort();
        names
    };
    // A tarball of one folder becomes that folder; zip files unpack only when asked.
    let tree = input("/in/tree");
    assert!(tree.directory, "{tree:?}");
    assert_eq!(names(&tree.source), ["a", "b"]);
    assert_eq!(input("/in/plain").source, "s3://store/tree.tgz");
    let files = input("/in/files");
    assert!(files.directory, "{files:?}");
    assert_eq!(names(&files.source), ["x", "y"]);
    let tool = input("/in/tool");
    assert_eq!(tool.source, "s3://store/tool");
    assert!(tool.executable, "mode 0755 makes the tool executable");
}

#[test]
fn matches_s3_endpoints() {
    let folder = tempfile::tempdir().unwrap();
    let target = folder.path().join("workflow.yaml");
    let artifact = "{name: data, path: /in, s3: {endpoint: 'minio.lab.example.org:9000', insecure: true, bucket: lab-data, key: in.txt, accessKeySecret: {name: s3, key: id}, secretKeySecret: {name: s3, key: secret}}}";
    std::fs::write(&target, format!("apiVersion: argoproj.io/v1alpha1\nkind: Workflow\nmetadata: {{name: s3}}\nspec:\n  entrypoint: main\n  templates:\n  - name: main\n    inputs: {{artifacts: [{artifact}]}}\n    container: {{image: alpine}}\n")).unwrap();
    let launch = Launch {
        target,
        simulated: true,
        ..Launch::default()
    };
    let provider = |address: &str| argo_runtime::Endpoint {
        provider: "lab".into(),
        prefixes: vec!["s3://lab-data/".into()],
        address: Some(address.into()),
    };
    let start = |endpoints| {
        argo_runtime::driver_with(&launch, endpoints)
            .unwrap()
            .start()
    };
    // The provider that serves the bucket at that service reads the artifact.
    let tasks = start(vec![provider("http://minio.lab.example.org:9000/")]).unwrap();
    assert_eq!(tasks[0].inputs[0].source, "s3://lab-data/in.txt");
    let error = start(vec![provider("https://s3.other.example.org")]).unwrap_err();
    assert!(
        error.unsupported && error.message.contains("provider `lab`"),
        "{error}"
    );
    let error = start(Vec::new()).unwrap_err();
    assert!(
        error
            .message
            .contains("add one with endpoint minio.lab.example.org:9000"),
        "{error}"
    );
}
