//! Checks that the web proxy of the process stays the one that its first request set.
// Copyright (c) 2026 The uwe Contributors
// SPDX-License-Identifier: MIT or Apache-2.0

// The proxy is shared by the whole process, so this file holds a single test.
#[test]
fn refuses_other_proxies() {
    let first = "http://proxy.example.org:3128";
    storage::use_proxy(Some(first)).expect("the first proxy");
    storage::use_proxy(Some(first)).expect("the same proxy");
    let other = "http://user:secret-word@other.example.org:3128";
    let error = storage::use_proxy(Some(other)).expect_err("another proxy");
    assert!(error.contains("another proxy"), "{error}");
    assert!(
        !error.contains("secret-word"),
        "the message names the password"
    );
    let error = storage::use_proxy(None).expect_err("no proxy");
    assert!(error.contains("other proxy settings"), "{error}");
}
