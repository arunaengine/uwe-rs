//! Profile retries, thread caps and global resources reach job execution, and unsupported
//! settings fail explicitly.
// Copyright (c) 2026 The iunctus Contributors
// SPDX-License-Identifier: MIT or Apache-2.0

use workflow_graph::{Launch, Outcome};

#[test]
fn applies_retry_aliases() {
    for setting in ["retries", "restart-times"] {
        for explicit in ["", "    retries: 0\n", "    retries: None\n"] {
            let folder = tempfile::tempdir().expect("a folder");
            let profile = folder.path().join("profiles/default");
            std::fs::create_dir_all(&profile).expect("a profile folder");
            std::fs::write(profile.join("config.yaml"), format!("{setting}: 2\n"))
                .expect("a profile");
            std::fs::write(
                folder.path().join("Snakefile"),
                format!("rule make:\n    output: 'out'\n{explicit}    shell: 'false'\n"),
            )
            .expect("a Snakefile");
            let launch = Launch {
                target: folder.path().to_path_buf(),
                scratch: folder.path().join("scratch"),
                ..Launch::default()
            };
            let mut driver = snakemake_runtime::driver(&launch).expect("a driver");
            let task = driver.start().expect("starts").remove(0);
            let attempts = if explicit.contains('0') { 1 } else { 3 };
            for attempt in 1..=attempts {
                let next = driver
                    .complete(Outcome {
                        id: task.id,
                        attempt,
                        exit_code: 1,
                        ..Outcome::default()
                    })
                    .expect("handles a failed attempt");
                assert_eq!(next.len(), usize::from(attempt < attempts));
                if let Some(retry) = next.first() {
                    assert_eq!(retry.id, task.id);
                    assert_eq!(retry.reuse, workflow_graph::Reuse::Denied);
                }
            }
            assert!(driver.finish().is_err());
        }
    }
}

#[test]
fn diagnoses_profile_settings() {
    for (settings, unsupported) in [
        ("no-hooks: true\n", true),
        ("retries: -1\n", false),
        ("restart-times: two\n", false),
        ("retries: 4294967296\n", false),
    ] {
        let folder = tempfile::tempdir().expect("a folder");
        let profile = folder.path().join("profiles/default");
        std::fs::create_dir_all(&profile).expect("a profile folder");
        std::fs::write(profile.join("config.yaml"), settings).expect("a profile");
        std::fs::write(
            folder.path().join("Snakefile"),
            "rule make:\n    shell: 'true'\n",
        )
        .expect("a Snakefile");
        let launch = Launch {
            target: folder.path().to_path_buf(),
            scratch: folder.path().join("scratch"),
            ..Launch::default()
        };
        let error = snakemake_runtime::driver(&launch)
            .expect("a driver")
            .start()
            .expect_err("invalid or unsupported setting");
        assert_eq!(error.unsupported, unsupported, "{error}");
        assert!(error.message.contains("rofile"), "{error}");
    }
}

#[test]
fn ignores_execution_settings() {
    let folder = tempfile::tempdir().expect("a folder");
    let profile = folder.path().join("profiles/default");
    std::fs::create_dir_all(&profile).expect("a profile folder");
    std::fs::write(
        profile.join("config.yaml"),
        "cores: 3\njobs: 8\nuse-conda: true\nsoftware-deployment-method: conda\nexecutor: slurm\nrerun-triggers: [mtime]\nnolock: true\njobname: x\nshadow-prefix: /tmp\nslurm-logdir: logs\nseconds-between-status-checks: 5\n",
    )
    .expect("a profile");
    std::fs::write(
        folder.path().join("Snakefile"),
        "rule make:\n    cache: 'omit-software'\n    output: 'out'\n    shell: 'touch out'\n",
    )
    .expect("a Snakefile");
    let launch = Launch {
        target: folder.path().to_path_buf(),
        scratch: folder.path().join("scratch"),
        ..Launch::default()
    };
    snakemake_runtime::driver(&launch)
        .expect("a driver")
        .start()
        .expect("ignored settings and omit-software caching");
}

/// A launch of `snakefile` with a workflow profile of `settings`, whose tasks are simulated.
fn profile_launch(folder: &std::path::Path, settings: &str, snakefile: &str) -> Launch {
    let profile = folder.join("profiles/default");
    std::fs::create_dir_all(&profile).expect("a profile folder");
    std::fs::write(profile.join("config.yaml"), settings).expect("a profile");
    std::fs::write(folder.join("Snakefile"), snakefile).expect("a Snakefile");
    Launch {
        target: folder.to_path_buf(),
        scratch: folder.join("scratch"),
        simulated: true,
        ..Launch::default()
    }
}

#[test]
fn caps_profile_threads() {
    let snakefile = "rule bwa:\n    output: 'out'\n    threads: 32\n    shell: 'bwa mem -t {threads} > {output}'\n";
    // Snakemake 9.27 runs `bwa mem -t 8` with `cores: 8`; `jobs` stands for the cores of a
    // local run only.
    for (settings, threads) in [
        ("cores: 8\n", 8),
        ("max-threads: 4\n", 4),
        ("cores: 8\nmax-threads: 2\n", 2),
        ("jobs: 6\n", 6),
        ("jobs: 6\nexecutor: slurm\n", 32),
        // Snakemake 9.27 renders this yte profile and runs `bwa mem -t 3`.
        (
            "__use_yte__: true\ncores: ?2 * 4\nset-threads:\n  bwa: ?16 // 4\n?if True:\n  max-threads: 3\n",
            3,
        ),
    ] {
        let folder = tempfile::tempdir().expect("a folder");
        let launch = profile_launch(folder.path(), settings, snakefile);
        let mut driver = snakemake_runtime::driver(&launch).expect("a driver");
        let task = driver.start().expect("starts").remove(0);
        let text = format!("{:?} {:?}", task.command, task.script);
        assert!(
            text.contains(&format!("-t {threads} ")),
            "{settings}: {text}"
        );
        assert_eq!(task.resources.cpus, Some(threads as f64), "{settings}");
    }
}

#[test]
fn limits_global_resources() {
    let snakefile = "rule all:\n    input: expand('o{i}', i=range(3)), 'bwa'\nrule fetch:\n    output: 'o{i}'\n    resources: ncbi=2\n    shell: 'echo {resources.ncbi} > {output}'\nrule bwa:\n    output: 'bwa'\n    shell: 'touch {output}'\n";
    let registered = format!("workflow.register_resource('ncbi', 1)\n{snakefile}");
    for (settings, snakefile) in [("resources: [ncbi=1]\n", snakefile), ("{}\n", &registered)] {
        let folder = tempfile::tempdir().expect("a folder");
        let launch = profile_launch(folder.path(), settings, snakefile);
        check_limits(&launch);
    }
}

/// Runs the fetch jobs of a launch whose global `ncbi` is 1.
fn check_limits(launch: &Launch) {
    let mut driver = snakemake_runtime::driver(launch).expect("a driver");
    let started = driver.start().expect("starts");
    let fetches: Vec<_> = (started.iter())
        .filter(|task| task.step == "fetch")
        .collect();
    assert_eq!(fetches.len(), 1, "one fetch at a time: {started:?}");
    assert_eq!(started.len(), 2, "bwa needs no ncbi");
    // Snakemake caps the resource of the job at the global one.
    let text = format!("{:?} {:?}", fetches[0].command, fetches[0].script);
    assert!(text.contains("echo 1 >"), "{text}");
    let mut current = fetches[0].id;
    for _ in 0..2 {
        let next = driver
            .complete(Outcome {
                id: current,
                attempt: 1,
                ..Outcome::default()
            })
            .expect("the fetch ends");
        assert_eq!(next.len(), 1, "the next fetch starts: {next:?}");
        assert_eq!(next[0].step, "fetch");
        current = next[0].id;
    }
}

/// Snakemake's scheduler holds a group job back while the global resources it needs are taken,
/// as for a single job; the jobs of one layer of the group add up.
#[test]
fn limits_group_resources() {
    let snakefile = "rule all:\n    input: expand('b{i}', i=range(2))\nrule a:\n    output: 'a{i}'\n    group: lambda wildcards: 'g' + wildcards.i\n    resources: ncbi=1\n    shell: 'touch {output}'\nrule b:\n    input: 'a{i}'\n    output: 'b{i}'\n    group: lambda wildcards: 'g' + wildcards.i\n    shell: 'cp {input} {output}'\n";
    let folder = tempfile::tempdir().expect("a folder");
    let launch = profile_launch(folder.path(), "resources: [ncbi=1]\n", snakefile);
    let mut driver = snakemake_runtime::driver(&launch).expect("a driver");
    let started = driver.start().expect("starts");
    assert_eq!(started.len(), 1, "one group at a time: {started:?}");
    let next = driver
        .complete(Outcome {
            id: started[0].id,
            attempt: 1,
            ..Outcome::default()
        })
        .expect("the group ends");
    assert_eq!(next.len(), 1, "the other group starts: {next:?}");
    assert_ne!(next[0].id, started[0].id);
}

/// Snakemake 9.27 takes `all-temp` from a profile and marks every output `temp()`, and
/// ignores `max-jobs-per-timespan`, which only paces job submission.
#[test]
fn marks_all_temporary() {
    let snakefile = "rule all:\n    input: 'b'\nrule a:\n    output: 'a'\n    shell: 'touch {output}'\nrule b:\n    input: 'a'\n    output: 'b'\n    shell: 'cp {input} {output}'\n";
    let folder = tempfile::tempdir().expect("a folder");
    let settings = "all-temp: true\nmax-jobs-per-timespan: 10/1s\n";
    let launch = profile_launch(folder.path(), settings, snakefile);
    let mut driver = snakemake_runtime::driver(&launch).expect("a driver");
    let mut executor = workflow_graph::Simulator::new(folder.path().join("made"));
    let mut delivered = Delivered::default();
    let run = workflow_graph::Engine::new(&mut executor)
        .observer(&mut delivered)
        .run("temp", driver.as_mut());
    assert!(run.error.is_none(), "{:?}", run.error);
    // The target stays; the file that only the next job reads goes.
    assert_eq!(delivered.0, ["b"]);
}

/// Snakemake 9.27 with `notemp` keeps `a`, which `temp()` would remove after `b` read it.
#[test]
fn keeps_temporary_outputs() {
    let snakefile = "rule all:\n    input: 'b'\nrule a:\n    output: temp('a')\n    shell: 'touch {output}'\nrule b:\n    input: 'a'\n    output: 'b'\n    shell: 'cp {input} {output}'\n";
    let folder = tempfile::tempdir().expect("a folder");
    let launch = profile_launch(folder.path(), "notemp: true\n", snakefile);
    let mut driver = snakemake_runtime::driver(&launch).expect("a driver");
    let mut executor = workflow_graph::Simulator::new(folder.path().join("made"));
    let mut delivered = Delivered::default();
    let run = workflow_graph::Engine::new(&mut executor)
        .observer(&mut delivered)
        .run("notemp", driver.as_mut());
    assert!(run.error.is_none(), "{:?}", run.error);
    assert_eq!(delivered.0, ["a", "b"]);
}

#[derive(Default)]
struct Delivered(Vec<String>);

impl workflow_graph::Observer for Delivered {
    fn event(&mut self, event: workflow_graph::Event<'_>) {
        if let workflow_graph::Event::Delivered { delivery } = event {
            let name = std::path::Path::new(&delivery.target).file_name();
            self.0
                .push(name.unwrap_or_default().to_string_lossy().into_owned());
        }
    }
}

/// A run that submits jobs elsewhere leaves resources of the scope `local` out of the global
/// tally, as Snakemake does: each job gets a pool of them.
#[test]
fn spares_local_scopes() {
    let snakefile = "resource_scopes:\n    ncbi='local'\nrule all:\n    input: expand('o{i}', i=range(2))\nrule fetch:\n    output: 'o{i}'\n    resources: ncbi=1\n    shell: 'touch {output}'\n";
    let folder = tempfile::tempdir().expect("a folder");
    let launch = profile_launch(folder.path(), "resources: [ncbi=1]\n", snakefile);
    let started = snakemake_runtime::driver(&launch)
        .expect("a driver")
        .start()
        .expect("starts");
    assert_eq!(started.len(), 2, "both fetches run at once: {started:?}");
}

/// Snakemake 9.27 gives a handover job without resources the global ones and the cores as
/// threads, so it runs `echo 20 4 4` here; no other job runs beside it.
#[test]
fn hands_over_resources() {
    let snakefile = "rule all:\n    input: 'a', 'b'\nrule a:\n    output: 'a'\n    handover: True\n    shell: 'echo {resources.mem_mb} {resources._cores} {threads} > {output}'\nrule b:\n    output: 'b'\n    shell: 'touch {output}'\n";
    let folder = tempfile::tempdir().expect("a folder");
    let launch = profile_launch(
        folder.path(),
        "resources: [mem_mb=20]\ncores: 4\n",
        snakefile,
    );
    let started = snakemake_runtime::driver(&launch)
        .expect("a driver")
        .start()
        .expect("starts");
    assert_eq!(started.len(), 1, "the handover job runs alone: {started:?}");
    let text = format!("{:?} {:?}", started[0].command, started[0].script);
    assert!(text.contains("echo 20 4 4"), "{text}");
}

/// Snakemake's test05 asks for `gpu=1`, which a local run of Snakemake only counts against
/// the global resources; the task asks for no accelerator.
#[test]
fn counts_local_gpus() {
    let snakefile = "rule a:\n    output: 'a'\n    resources: gpu=1\n    shell: 'touch {output}'\n";
    for (local, gpus) in [(true, None), (false, Some(1))] {
        let folder = tempfile::tempdir().expect("a folder");
        let launch = profile_launch(folder.path(), "{}\n", snakefile);
        let options = snakemake_runtime::RunSettings {
            local,
            ..Default::default()
        };
        let mut driver =
            snakemake_runtime::driver_with_options(&launch, Default::default(), options).unwrap();
        let task = driver.start().expect("starts").remove(0);
        assert_eq!(task.resources.gpus, gpus, "local {local}");
    }
}

/// Snakemake 9.27 finds the wrapper `demo` under the profile's `wrapper-prefix`, here a local
/// folder, and runs its `wrapper.py`.
#[test]
fn prefixes_wrapper_names() {
    let snakefile = "rule a:\n    output: 'o'\n    wrapper: 'demo'\n";
    let folder = tempfile::tempdir().expect("a folder");
    let settings = "wrapper-prefix: 'file:wraps/'\n";
    let launch = profile_launch(folder.path(), settings, snakefile);
    let script = folder.path().join("wraps/demo");
    std::fs::create_dir_all(&script).expect("a wrapper folder");
    std::fs::write(script.join("wrapper.py"), "print('from prefix')\n").expect("a wrapper");
    let mut driver = snakemake_runtime::driver(&launch).expect("a driver");
    let task = driver.start().expect("starts").remove(0);
    assert!(format!("{task:?}").contains("from prefix"), "{task:?}");
}

/// Snakemake 9.27 with `allowed-rules: [b]` makes no file with rule `a`: `b` misses `a`
/// until it is there, and then runs alone.
#[test]
fn limits_allowed_rules() {
    let snakefile = "rule all:\n    input: 'b'\nrule a:\n    output: 'a'\n    shell: 'touch {output}'\nrule b:\n    input: 'a'\n    output: 'b'\n    shell: 'cp {input} {output}'\n";
    let folder = tempfile::tempdir().expect("a folder");
    let launch = Launch {
        simulated: false,
        ..profile_launch(folder.path(), "allowed-rules: [b]\n", snakefile)
    };
    let error = snakemake_runtime::driver(&launch)
        .expect("a driver")
        .start()
        .expect_err("rule a may not run");
    assert!(
        error.message.contains("Missing input files for rule b"),
        "{error}"
    );
    std::fs::write(folder.path().join("a"), "").expect("the input");
    let started = snakemake_runtime::driver(&launch)
        .expect("a driver")
        .start()
        .expect("starts");
    let names: Vec<&str> = started.iter().map(|task| task.step.as_str()).collect();
    assert_eq!(names, ["b"]);
}
