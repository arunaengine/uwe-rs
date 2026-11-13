//! Conda and container directives combine only when environment preparation is enabled.
// Copyright (c) 2026 The uwe Contributors
// SPDX-License-Identifier: MIT or Apache-2.0

use workflow_graph::Launch;

#[test]
fn combines_conda_container() {
    for enabled in [false, true] {
        let folder = tempfile::tempdir().unwrap();
        std::fs::write(folder.path().join("env.yaml"), "dependencies: []\n").unwrap();
        std::fs::write(folder.path().join("Snakefile"), "container: 'docker://debian:stable-slim'\nrule make:\n    output: 'out'\n    conda: 'env.yaml'\n    shell: 'echo made > {output}'\n").unwrap();
        let launch = Launch {
            target: folder.path().into(),
            scratch: folder.path().join("scratch"),
            ..Launch::default()
        };
        let options = snakemake_runtime::RunSettings {
            conda: enabled,
            ..Default::default()
        };
        let task = snakemake_runtime::driver_with_options(&launch, Default::default(), options)
            .unwrap()
            .start()
            .unwrap()
            .remove(0);
        let image = task.image.unwrap();
        if enabled {
            let pair: [String; 2] =
                serde_json::from_str(image.strip_prefix("conda-container:").unwrap()).unwrap();
            assert_eq!(pair[0], "debian:stable-slim");
            assert_eq!(pair[1], folder.path().join("env.yaml").to_string_lossy());
        } else {
            assert_eq!(image, "debian:stable-slim");
        }
    }
}
