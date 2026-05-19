//! Tests the cache key contract: golden keys, what changes a key and what must not.
// Copyright (c) 2026 The iunctus Contributors
// SPDX-License-Identifier: MIT or Apache-2.0

use std::collections::BTreeMap;

use task_model::{
    Digest, EnvironmentValue, FORMAT, FileKind, Image, InputFile, OutputRule, SpecError, TaskSpec,
    Tree, TreeEntry,
};

fn file_digest(content: &str) -> Digest {
    Digest::sha256(content.as_bytes())
}

fn input(path: &str, content: &str) -> InputFile {
    InputFile {
        path: path.to_owned(),
        kind: FileKind::File,
        digest: file_digest(content),
        executable: false,
        writable: false,
    }
}

/// The FASTQC example of design section 5.2, with fixed contents instead of real files.
fn fastqc() -> TaskSpec {
    TaskSpec {
        format: FORMAT,
        namespace: String::new(),
        image: Some(Image {
            reference: format!("quay.io/biocontainers/fastqc@{}", file_digest("image")),
            platform: Some("linux/amd64".to_owned()),
        }),
        arguments: vec!["/bin/bash".into(), "-ue".into(), ".command.sh".into()],
        environment: BTreeMap::from([(
            "NXF_TASK_WORKDIR".to_owned(),
            EnvironmentValue::WorkDirectory,
        )]),
        inputs: vec![
            input(".command.sh", "fastqc --threads 2 test_1.fastq.gz\n"),
            input("1/test_1.fastq.gz", "reads"),
        ],
        outputs: vec![
            OutputRule {
                pattern: "*.html".into(),
            },
            OutputRule {
                pattern: "*.zip".into(),
            },
            OutputRule {
                pattern: ".command.env".into(),
            },
        ],
        stdin: None,
        stdout: ".command.out".into(),
        stderr: ".command.err".into(),
    }
}

#[test]
fn golden_fastqc_key() {
    let key = fastqc().cache_key().expect("valid specification");
    // A new value here means cached results of every user become unreachable.
    // Only accept it together with a new FORMAT.
    assert_eq!(
        key.as_str(),
        "sha256:3726a522e2c2d41f664f08a7c5f4873240b6885150b55b652084a2052cc90ee2"
    );
}

#[test]
fn stable_canonical_json() {
    let canonical = fastqc().canonical().expect("valid specification");
    let text = String::from_utf8(canonical).expect("canonical JSON is UTF-8");
    assert!(text.starts_with(r#"{"arguments":["/bin/bash","-ue",".command.sh"],"environment":"#));
    assert!(
        !text.contains(' '),
        "canonical JSON has no insignificant spaces: {text}"
    );
}

#[test]
fn ignores_input_order() {
    let mut reordered = fastqc();
    reordered.inputs.reverse();
    reordered.outputs.reverse();
    reordered.outputs.push(OutputRule {
        pattern: "*.zip".into(),
    });
    assert_eq!(reordered.cache_key(), fastqc().cache_key());
}

#[test]
fn every_field_counts() {
    let base = fastqc().cache_key().expect("valid specification");
    let changes: Vec<fn(&mut TaskSpec)> = vec![
        |spec| spec.namespace = "salt".into(),
        |spec| spec.image = None,
        |spec| spec.arguments.push("-x".into()),
        |spec| {
            let value = EnvironmentValue::Literal("1".into());
            spec.environment.insert("A".into(), value);
        },
        |spec| spec.inputs[1].digest = Digest::sha256(b"other reads"),
        |spec| spec.inputs[1].path = "2/test_1.fastq.gz".into(),
        |spec| spec.inputs[1].executable = true,
        |spec| {
            spec.outputs.push(OutputRule {
                pattern: "*.txt".into(),
            })
        },
        |spec| spec.stdout = "out.txt".into(),
    ];
    for (index, change) in changes.into_iter().enumerate() {
        let mut spec = fastqc();
        change(&mut spec);
        assert_ne!(spec.cache_key().expect("valid"), base, "change {index}");
    }
}

#[test]
fn rejects_invalid_specs() {
    let mut escaping = fastqc();
    escaping.inputs[1].path = "../outside".into();
    assert_eq!(
        escaping.cache_key(),
        Err(SpecError::Path("../outside".into()))
    );

    let mut duplicate = fastqc();
    duplicate.inputs.push(input("1/test_1.fastq.gz", "again"));
    assert!(matches!(
        duplicate.validate(),
        Err(SpecError::DuplicateInput(_))
    ));

    let mut empty = fastqc();
    empty.arguments.clear();
    assert_eq!(empty.validate(), Err(SpecError::NoProgram));

    let mut future = fastqc();
    future.format = FORMAT + 1;
    assert_eq!(future.validate(), Err(SpecError::Format(FORMAT + 1)));
}

#[test]
fn detects_pinned_images() {
    assert!(fastqc().image_pinned());
    let mut floating = fastqc();
    floating.image = Some(Image {
        reference: "quay.io/biocontainers/fastqc:0.12.1".into(),
        platform: None,
    });
    assert!(!floating.image_pinned());
}

#[test]
fn digest_round_trip() {
    let digest = Digest::sha256(b"");
    let expected = "sha256:e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855";
    assert_eq!(digest.as_str(), expected);
    assert_eq!(expected.parse::<Digest>(), Ok(digest.clone()));
    assert!("md5:abc".parse::<Digest>().is_err());
    assert!("sha256:ABC".parse::<Digest>().is_err());
    let json = serde_json::to_string(&digest).expect("serializes");
    assert_eq!(
        serde_json::from_str::<Digest>(&json).expect("parses"),
        digest
    );
}

#[test]
fn tree_order_ignored() {
    let file = TreeEntry::File {
        path: "a/b.txt".into(),
        digest: file_digest("b"),
        size: 1,
        executable: false,
    };
    let empty = TreeEntry::EmptyDirectory { path: "c".into() };
    let forward = Tree {
        entries: vec![file.clone(), empty.clone()],
    };
    let backward = Tree {
        entries: vec![empty, file.clone()],
    };
    assert_eq!(forward.digest(), backward.digest());
    assert_eq!(forward.size(), 1);
    let doubled = Tree {
        entries: vec![file.clone(), file],
    };
    assert!(doubled.digest().is_err());
}
