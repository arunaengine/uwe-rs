//! Checks the functions of `snakemake.utils` that Snakefiles call while they are planned.
// Copyright (c) 2026 The uwe Contributors
// SPDX-License-Identifier: MIT or Apache-2.0

use workflow_graph::Launch;

/// Snakemake 9.27 lists `data/a.txt {'name': 'a'}`, then the files below `data/x` and
/// `data/y` with the folder in the value, and restricts the second call to `y`; it walks in
/// the order of the file system, uwe in the order of the names.
#[test]
fn lists_matching_files() {
    let folder = tempfile::tempdir().expect("a folder");
    for file in [
        "data/a.txt",
        "data/b.txt",
        "data/x/c.txt",
        "data/y/d.txt",
        "data/z.csv",
    ] {
        let path = folder.path().join(file);
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(path, "").unwrap();
    }
    let snakefile = "from snakemake.utils import listfiles\nfound = [(f, dict(w.items()), w.name) for f, w in listfiles('data/{name}.txt')]\nkept = [f for f, w in listfiles('data/{d}/{n}.txt', restriction={'d': 'y'}, omit_value='q')]\nrule all:\n    output: 'o'\n    params: found=found, kept=kept\n    shell: 'echo {params.found} {params.kept} > {output}'\n";
    std::fs::write(folder.path().join("Snakefile"), snakefile).unwrap();
    let launch = Launch {
        target: folder.path().to_path_buf(),
        scratch: folder.path().join("scratch"),
        simulated: true,
        ..Launch::default()
    };
    let task = snakemake_runtime::driver(&launch)
        .unwrap()
        .start()
        .unwrap()
        .remove(0);
    let text = task.script.unwrap_or_default();
    let expected = "('data/a.txt', {'name': 'a'}, 'a') ('data/b.txt', {'name': 'b'}, 'b') ('data/x/c.txt', {'name': 'x/c'}, 'x/c') ('data/y/d.txt', {'name': 'y/d'}, 'y/d') data/y/d.txt";
    assert!(text.contains(expected), "{text}");
}
