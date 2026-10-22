//! Checks durable container ownership, locks, and exact reconciliation identities.
// Copyright (c) 2026 The uwe Contributors
// SPDX-License-Identifier: MIT or Apache-2.0

use super::*;
use crate::test_engine::{Reply, Server};

fn journal(folder: &Path) -> Journal {
    Journal::new(folder, "socket".into(), "test-engine".into())
}

#[tokio::test]
async fn recovers_after_restart() {
    let server = Server::start(2, |request| {
        if request.method == "DELETE" {
            assert!(request.path.contains("/containers/immutable-id?"));
            Reply::Json(204, serde_json::Value::Null)
        } else {
            assert!(request.path.ends_with("/containers/task/json"));
            Reply::Json(
                200,
                serde_json::json!({"Id":"immutable-id",
                "Config":{"Labels":{OWNER:"owner"}}}),
            )
        }
    })
    .await;
    let folder = tempfile::tempdir().unwrap();
    let first = journal(folder.path());
    let pending = first.begin("task", "owner").await.unwrap();
    let record = first.root.join("0/record.json");
    let data: serde_json::Value = serde_json::from_slice(&std::fs::read(&record).unwrap()).unwrap();
    assert_eq!(data["socket"], "socket");
    assert_eq!(data["daemon"], "test-engine");
    assert!(data["id"].is_null());
    journal(folder.path())
        .recover(server.engine.client())
        .await
        .unwrap();
    assert!(
        record.is_file(),
        "a live lock must keep the attempt untouched"
    );
    drop(pending);
    drop(first);
    journal(folder.path())
        .recover(server.engine.client())
        .await
        .unwrap();
    assert!(!record.exists());
    server.finish();
}

#[tokio::test]
async fn retains_late_creation() {
    let mut checks = 0;
    let server = Server::start(3, move |request| {
        if request.method == "DELETE" {
            assert!(request.path.contains("/containers/late-id?"));
            return Reply::Json(204, serde_json::Value::Null);
        }
        checks += 1;
        if checks == 1 {
            Reply::Json(404, serde_json::json!({"message":"not yet created"}))
        } else {
            Reply::Json(
                200,
                serde_json::json!({"Id":"late-id","Config":{"Labels":{OWNER:"owner"}}}),
            )
        }
    })
    .await;
    let folder = tempfile::tempdir().unwrap();
    let first = journal(folder.path());
    drop(first.begin("task", "owner").await.unwrap());
    first.recover(server.engine.client()).await.unwrap();
    assert!(first.root.join("0/record.json").is_file());
    journal(folder.path())
        .recover(server.engine.client())
        .await
        .unwrap();
    assert!(!first.root.join("0/record.json").exists());
    server.finish();
}

#[tokio::test]
async fn preserves_foreign_owner() {
    let server = Server::start(1, |request| {
        assert_ne!(request.method, "DELETE");
        assert!(request.path.ends_with("/containers/foreign-id/json"));
        Reply::Json(
            200,
            serde_json::json!({"Id":"foreign-id","Config":{"Labels":{OWNER:"another-owner"}}}),
        )
    })
    .await;
    let folder = tempfile::tempdir().unwrap();
    let first = journal(folder.path());
    assert!(
        first
            .bind(
                first.begin("same-name", "owner").await.unwrap(),
                "foreign-id"
            )
            .is_ok()
    );
    first.release("foreign-id", false);
    first.recover(server.engine.client()).await.unwrap();
    let other = Journal::new(folder.path(), "socket".into(), "replacement-daemon".into());
    assert_ne!(first.root, other.root);
    other.recover(server.engine.client()).await.unwrap();
    assert!(first.root.join("0/record.json").is_file());
    server.finish();
}

#[tokio::test]
async fn retries_failed_removal() {
    let mut deletes = 0;
    let server = Server::start(3, move |request| {
        if request.method == "DELETE" {
            deletes += 1;
            return if deletes == 1 {
                Reply::Json(503, serde_json::json!({"message":"engine unavailable"}))
            } else {
                Reply::Json(204, serde_json::Value::Null)
            };
        }
        assert!(request.path.ends_with("/containers/owned-id/json"));
        Reply::Json(
            200,
            serde_json::json!({"Id":"owned-id","Config":{"Labels":{OWNER:"owner"}}}),
        )
    })
    .await;
    let journal = &server.engine.journal;
    let pending = journal.begin("task", "owner").await.unwrap();
    assert!(journal.bind(pending, "owned-id").is_ok());
    let saved = journal.root.join("0/record.json");
    let data: serde_json::Value = serde_json::from_slice(&std::fs::read(&saved).unwrap()).unwrap();
    assert_eq!(data["id"], "owned-id");
    journal
        .at(journal.root.parent().unwrap())
        .recover(server.engine.client())
        .await
        .unwrap();
    assert!(!crate::engine::remove_owned(&server.engine, "owned-id").await);
    assert!(saved.is_file());
    assert!(journal.active.lock().unwrap().is_empty());
    journal.recover(server.engine.client()).await.unwrap();
    assert!(!saved.exists());
    server.finish();
}

#[tokio::test]
async fn resumes_paged_recovery() {
    let mut checks = 0;
    let server = Server::start(17, move |request| {
        assert!(
            request
                .path
                .ends_with(&format!("/containers/task-{checks}/json"))
        );
        checks += 1;
        Reply::Json(404, serde_json::json!({"message":"unknown outcome"}))
    })
    .await;
    let folder = tempfile::tempdir().unwrap();
    for number in 0..17 {
        drop(
            journal(folder.path())
                .begin(&format!("task-{number}"), &format!("owner-{number}"))
                .await
                .unwrap(),
        );
    }
    journal(folder.path())
        .recover(server.engine.client())
        .await
        .unwrap();
    journal(folder.path())
        .recover(server.engine.client())
        .await
        .unwrap();
    server.finish();
}

#[tokio::test]
async fn concurrent_engines_create() {
    let server = Server::start(4, |request| {
        if request.method == "DELETE" {
            assert!(!request.path.contains("task-one") && !request.path.contains("task-two"));
            Reply::Json(204, serde_json::Value::Null)
        } else {
            let owner = request.body["Labels"][OWNER].as_str().unwrap();
            Reply::Json(
                201,
                serde_json::json!({"Id":format!("{owner}-id"),"Warnings":[]}),
            )
        }
    })
    .await;
    let folder = tempfile::tempdir().unwrap();
    let first = server
        .engine
        .clone()
        .with_journal_root(folder.path().to_owned());
    let second = server
        .engine
        .clone()
        .with_journal_root(folder.path().to_owned());
    first
        .journal
        .begin("warmup", "warmup")
        .await
        .unwrap()
        .reject()
        .unwrap();
    let held = queue_lock(&first.journal.root).unwrap();
    let mut one = Box::pin(first.journal.begin("intent-one", "one"));
    let mut two = Box::pin(second.journal.begin("intent-two", "two"));
    std::future::poll_fn(|context| {
        assert!(std::future::Future::poll(one.as_mut(), context).is_pending());
        assert!(std::future::Future::poll(two.as_mut(), context).is_pending());
        std::task::Poll::Ready(())
    })
    .await;
    drop(held);
    let (one, two) = tokio::join!(one, two);
    one.unwrap().reject().unwrap();
    two.unwrap().reject().unwrap();
    let options = |name: &str| bollard::query_parameters::CreateContainerOptions {
        name: Some(name.into()),
        ..Default::default()
    };
    let cancel = crate::CancellationToken::new();
    let (one, two) = tokio::join!(
        crate::process::create_id(&first, options("task-one"), Default::default(), &cancel),
        crate::process::create_id(&second, options("task-two"), Default::default(), &cancel)
    );
    let one = one.unwrap();
    let two = two.unwrap();
    assert_ne!(one, two);
    assert!(super::super::remove_owned(&first, &one).await);
    assert!(super::super::remove_owned(&second, &two).await);
    server.finish();
}
