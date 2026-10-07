//! Qualifies native pip locking and hash-checked installation with a real local Docker build.
// Copyright (c) 2026 The iunctus Contributors
// SPDX-License-Identifier: MIT or Apache-2.0

use std::time::Duration;

use bollard::Docker;
use bollard::models::{ContainerCreateBody, HostConfig};
use bollard::query_parameters::{
    BuildImageOptions, BuilderVersion, CreateImageOptions, LogsOptions, RemoveContainerOptions,
    RemoveImageOptions, WaitContainerOptions,
};
use conda_environment::{Acquisition, Environment, ImagePreparation, VirtualPackage};
use futures_util::{FutureExt, StreamExt};

const BASE: &str = "docker.io/mambaorg/micromamba:2.3.2@sha256:955819619f303e2aa1dc1ba89beefe7f326a378b3d0782a4a0d28b1cf11b68b6";

async fn run(docker: &Docker, image: &str, command: Vec<String>) -> Result<String, String> {
    let container = docker
        .create_container(
            None,
            ContainerCreateBody {
                image: Some(image.into()),
                cmd: Some(command),
                entrypoint: Some(vec![]),
                host_config: Some(HostConfig {
                    network_mode: Some("none".into()),
                    memory: Some(256 << 20),
                    nano_cpus: Some(1_000_000_000),
                    ..HostConfig::default()
                }),
                ..ContainerCreateBody::default()
            },
        )
        .await
        .map_err(|error| error.to_string())?;
    let result = async {
        docker
            .start_container(&container.id, None)
            .await
            .map_err(|error| error.to_string())?;
        let mut waiting = docker.wait_container(
            &container.id,
            Some(WaitContainerOptions {
                condition: "not-running".into(),
            }),
        );
        let status = waiting
            .next()
            .await
            .ok_or("no container status")?
            .map_err(|error| error.to_string())?;
        let mut logs = docker.logs(
            &container.id,
            Some(LogsOptions {
                stdout: true,
                stderr: true,
                ..LogsOptions::default()
            }),
        );
        let mut output = String::new();
        while let Some(line) = logs.next().await {
            output.push_str(&line.map_err(|error| error.to_string())?.to_string());
        }
        if status.status_code != 0 {
            return Err(format!("exit {}: {output}", status.status_code));
        }
        Ok(output)
    }
    .await;
    docker
        .remove_container(
            &container.id,
            Some(RemoveContainerOptions {
                force: true,
                v: true,
                ..RemoveContainerOptions::default()
            }),
        )
        .await
        .map_err(|error| error.to_string())?;
    result
}

#[tokio::test]
#[ignore = "requires PyPI, conda-forge and the local Docker engine; serial downloads and one CPU"]
async fn builds_requests_image() {
    tokio::time::timeout(Duration::from_secs(1200), qualify())
        .await
        .expect("bounded real build");
}

async fn qualify() {
    let docker = Docker::connect_with_local_defaults().unwrap();
    let existed = docker.inspect_image(BASE).await.is_ok();
    let tag = format!("iunctus-pip-test-{}", std::process::id());
    let result = std::panic::AssertUnwindSafe(async {
    if !existed {
        let mut pulling = docker.create_image(
            Some(CreateImageOptions {
                from_image: Some(BASE.into()),
                ..CreateImageOptions::default()
            }),
            None,
            None,
        );
        while let Some(progress) = pulling.next().await {
            progress.unwrap();
        }
    }
    let output = run(
        &docker,
        BASE,
        vec!["/usr/bin/getconf".into(), "GNU_LIBC_VERSION".into()],
    )
    .await
    .unwrap();
    let version = output.split_whitespace().nth(1).expect("target glibc");
    let capabilities = vec![
        VirtualPackage {
            name: "__glibc".into(),
            version: version.into(),
            build: "0".into(),
        },
        VirtualPackage {
            name: "__linux".into(),
            version: "0".into(),
            build: "0".into(),
        },
        VirtualPackage {
            name: "__unix".into(),
            version: "0".into(),
            build: "0".into(),
        },
    ];
    let cache = tempfile::tempdir().unwrap();
    let acquisition = Acquisition::new(cache.path(), false).unwrap();
    let environment = Environment::parse(
        "channels: [conda-forge]\ndependencies:\n  - python=3.12\n  - pip: [requests==2.32.3]\n",
    )
    .unwrap();
    let lock = acquisition
        .lock(&environment, "linux-64", Some(&capabilities))
        .await
        .unwrap();
    println!(
        "locked {} Conda packages, Python {}, {} pip packages, {}",
        lock.packages.len(),
        lock.pypi.as_ref().unwrap().python,
        lock.pypi.as_ref().unwrap().packages.len(),
        lock.digest()
    );
    for package in &lock.pypi.as_ref().unwrap().packages {
        println!(
            "{}=={} {} sha256:{}",
            package.name, package.version, package.file, package.sha256
        );
    }
    let offline = Acquisition::new(cache.path(), true).unwrap();
    let cached = offline
        .lock(&environment, "linux-64", Some(&capabilities))
        .await
        .unwrap();
    assert_eq!(cached.digest(), lock.digest());
    let packages = cache.path().join("packages");
    acquisition.fetch_packages(&lock, &packages).await.unwrap();
    let preparation = ImagePreparation::new(&lock, BASE, "linux/amd64", &capabilities).unwrap();
    let mut context = Vec::new();
    preparation.write_context(&packages, &mut context).unwrap();
    assert!(context.len() < 256 << 20);
    let result = async {
        let mut building = docker.build_image(BuildImageOptions {
            t: Some(tag.clone()), platform: "linux/amd64".into(), version: BuilderVersion::BuilderV1,
            networkmode: Some("none".into()), memory: Some(1 << 30), memswap: Some(1 << 30),
            cpuperiod: Some(100_000), cpuquota: Some(100_000), forcerm: true,
            ..BuildImageOptions::default()
        }, None, Some(bollard::body_full(context.into())));
        while let Some(step) = building.next().await {
            let step = step.map_err(|error| error.to_string())?;
            if let Some(error) = step.error_detail.and_then(|error| error.message) { return Err(error); }
            if let Some(stream) = step.stream { print!("{stream}"); }
        }
        let output = run(&docker, &tag, vec!["/opt/iunctus/bin/python".into(), "-c".into(), "import requests; assert requests.__version__ == '2.32.3'; print('requests=' + requests.__version__)".into()]).await?;
        println!("{output}");
        Ok::<_, String>(())
    }.await;
    result.unwrap();
    }).catch_unwind().await;
    if docker.inspect_image(&tag).await.is_ok() {
        docker
            .remove_image(
                &tag,
                Some(RemoveImageOptions {
                    force: true,
                    noprune: false,
                    ..RemoveImageOptions::default()
                }),
                None,
            )
            .await
            .unwrap();
    }
    if !existed {
        docker
            .remove_image(
                BASE,
                Some(RemoveImageOptions {
                    force: false,
                    noprune: false,
                    ..RemoveImageOptions::default()
                }),
                None,
            )
            .await
            .unwrap();
    }
    if let Err(error) = result {
        std::panic::resume_unwind(error);
    }
}
