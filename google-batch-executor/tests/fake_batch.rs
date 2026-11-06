//! Checks the Google Batch backend against a local fake of Google Batch, the OAuth token
//! endpoint and Cloud Logging that runs each job's runnables here.
// Copyright (c) 2026 The uwe Contributors
// SPDX-License-Identifier: MIT or Apache-2.0

mod common;

use std::collections::BTreeMap;
use std::path::Path;
use std::sync::OnceLock;
use std::time::Duration;

use aws_batch_executor::Helper;
use executor::remote::RemoteStage;
use executor::{Backend, ExecutorError, Job, Staging};
use google_batch_executor::{Client, Credentials, GoogleBackend, Settings};
use rsa::RsaPrivateKey;
use rsa::pkcs8::{EncodePrivateKey, LineEnding};
use rsa::rand_core::{CryptoRng, RngCore};
use serde_json::json;
use task_model::{FORMAT, Image, OutputRule, Placement, TaskSpec};
use tokio_util::sync::CancellationToken;

use common::FakeGoogle;

/// An image with a digest, which needs no registry.
const IMAGE: &str =
    "ubuntu@sha256:0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef";

/// A generator of fixed bytes, so that every run makes the same test key.
struct Seeded(u64);

impl RngCore for Seeded {
    fn next_u32(&mut self) -> u32 {
        self.next_u64() as u32
    }

    fn next_u64(&mut self) -> u64 {
        // SplitMix64.
        self.0 = self.0.wrapping_add(0x9e37_79b9_7f4a_7c15);
        let mut value = self.0;
        value = (value ^ (value >> 30)).wrapping_mul(0xbf58_476d_1ce4_e5b9);
        value = (value ^ (value >> 27)).wrapping_mul(0x94d0_49bb_1331_11eb);
        value ^ (value >> 31)
    }

    fn fill_bytes(&mut self, bytes: &mut [u8]) {
        for chunk in bytes.chunks_mut(8) {
            let value = self.next_u64().to_le_bytes();
            chunk.copy_from_slice(&value[..chunk.len()]);
        }
    }

    fn try_fill_bytes(&mut self, bytes: &mut [u8]) -> Result<(), rsa::rand_core::Error> {
        self.fill_bytes(bytes);
        Ok(())
    }
}

impl CryptoRng for Seeded {}

/// A made-up service account key, the same in every run.
fn key() -> &'static RsaPrivateKey {
    static KEY: OnceLock<RsaPrivateKey> = OnceLock::new();
    KEY.get_or_init(|| RsaPrivateKey::new(&mut Seeded(7), 1024).expect("a key"))
}

/// The test program as the helper of the jobs: it runs `stage_helper` with the arguments.
fn helper(folder: &Path) -> String {
    let program = std::env::current_exe().expect("this program");
    let script = format!(
        "#!/bin/sh\nIFS=$(printf '\\037')\nUWE_TEST_STAGE=\"$*\" exec '{}' --exact \
         stage_helper --nocapture --test-threads=1\n",
        program.to_string_lossy().replace('\'', r"'\''")
    );
    let path = folder.join("helper");
    std::fs::create_dir_all(folder).expect("makes the folder");
    std::fs::write(&path, script).expect("writes the helper");
    let mode = std::os::unix::fs::PermissionsExt::from_mode(0o755);
    std::fs::set_permissions(&path, mode).expect("runs");
    path.to_string_lossy().into_owned()
}

#[test]
fn stage_helper() {
    if let Ok(arguments) = std::env::var("UWE_TEST_STAGE") {
        let arguments: Vec<String> = arguments.split('\u{1f}').map(str::to_owned).collect();
        // The first argument is `__stage`, as the uwe program takes it.
        executor::helper::main(&arguments[1..]).expect("the helper stages");
    }
}

/// A backend in `folder` whose calls go to the fake, with a service account key file whose
/// `token_uri` is the fake.
fn backend(folder: &Path, fake: &FakeGoogle) -> GoogleBackend {
    let pem = key().to_pkcs8_pem(LineEnding::LF).expect("a PEM key");
