//! Authentication applies to every route of a remotely bound run page.
// Copyright (c) 2026 The iunctus Contributors
// SPDX-License-Identifier: MIT or Apache-2.0

use run_page::{PageError, Server, Settings};

#[test]
fn requires_remote_token() {
    let mut settings = Settings::new("unused.sqlite".into());
    settings.address = "0.0.0.0".parse().unwrap();
    assert!(matches!(
        Server::start(&settings),
        Err(PageError::TokenRequired)
    ));
}

#[test]
fn authenticates_all_routes() {
    let root = tempfile::tempdir().unwrap();
    let mut settings = Settings::new(root.path().join("runs.sqlite"));
    settings.token = Some("test-token".into());
    let server = Server::start(&settings).unwrap();
    let client = reqwest::blocking::Client::builder()
        .redirect(reqwest::redirect::Policy::none())
        .build()
        .unwrap();
    for route in [
        "/",
        "/page.js",
        "/api/runs",
        "/api/runs/missing/events",
        "/api/runs/missing/tasks/1/output/0",
    ] {
        let url = format!("{}{route}", server.url());
        assert_eq!(client.get(&url).send().unwrap().status(), 401);
        assert_eq!(
            client
                .get(&url)
                .bearer_auth("wrong")
                .send()
                .unwrap()
                .status(),
            401
        );
        assert_ne!(
            client
                .get(&url)
                .bearer_auth("test-token")
                .send()
                .unwrap()
                .status(),
            401
        );
    }
    let response = client
        .get(format!("{}/?token=test-token", server.url()))
        .send()
        .unwrap();
    assert_eq!(response.status(), 303);
    assert_eq!(response.headers()["location"], "/");
    let cookie = response.headers()["set-cookie"].to_str().unwrap();
    assert!(cookie.contains("HttpOnly; SameSite=Strict"));
    assert_eq!(
        client
            .get(format!("{}/api/runs", server.url()))
            .header("cookie", cookie.split(';').next().unwrap())
            .send()
            .unwrap()
            .status(),
        200
    );
    server.stop();
}
