//! Isolated embedded runs: workflow code reaches only the folders and variables of its run.
// Copyright (c) 2026 The uwe Contributors
// SPDX-License-Identifier: MIT or Apache-2.0

use std::path::Path;

use serde_json::json;
use uwe::RunRequest;
use uwe::hosted::Isolation;
use uwe::run::Settings;

mod common;

/// A variable of the service process that no workflow may read.
const SERVICE: &str = "UWE_TEST_SERVICE_TOKEN";

/// The child process that runs a test has `SERVICE` set and reads no site or user config.
const CHILD: &str = "UWE_ISOLATION_CHILD";

/// Tries to read, delete and overwrite a file outside the run, and to read `SERVICE`.
const NEXTFLOW: &str = "workflow {\n  def seen = []\n  \
    try { seen << file(params.outside).text } catch (e) { seen << 'refused read' }\n  \
    try { seen << file(params.outside).countLines() } catch (e) { seen << 'refused count' }\n  \
    try { file(params.outside).delete(); seen << 'deleted' } catch (e) { seen << 'refused delete' }\n  \
    try { file(params.outside).text = 'changed'; seen << 'written' } catch (e) { seen << 'refused write' }\n  \
    seen << (env('UWE_TEST_SERVICE_TOKEN') ?: 'no variable')\n  \
    seen << (System.getenv('UWE_TEST_SERVICE_TOKEN') ?: 'no variable')\n  \
    seen << file(params.inside).text\n  \
    file(params.report).text = seen.join('\\n')\n}\n";

/// The same at the top level of a Snakefile.
const SNAKEFILE: &str = "import os\nseen = []\ntry:\n    seen.append(open(config['outside']).read())\n\
    except OSError:\n    seen.append('refused read')\ntry:\n    os.remove(config['outside'])\n    \
    seen.append('deleted')\nexcept OSError:\n    seen.append('refused delete')\n\
    seen.append(os.environ.get('UWE_TEST_SERVICE_TOKEN', 'no variable'))\n\
    seen.append(open(config['inside']).read())\nwith open(config['report'], 'w') as written:\n    \
    written.write('\\n'.join(seen))\n\nrule all:\n    input: []\n";

const NEXTFLOW_SEEN: &str =
    "refused read\nrefused count\nrefused delete\nrefused write\nno variable\nno variable\ninside";
const SNAKEMAKE_SEEN: &str = "refused read\nrefused delete\nno variable\ninside";

#[test]
fn confines_workflow_code() {
    if std::env::var_os(CHILD).is_none() {
        return in_child("confines_workflow_code", None);
    }
    let root = tempfile::tempdir().unwrap();
    let outside = root.path().join("outside/secret.txt");
    std::fs::create_dir_all(outside.parent().unwrap()).unwrap();
    for (name, script, seen) in [
        ("main.nf", NEXTFLOW, NEXTFLOW_SEEN),
        ("Snakefile", SNAKEFILE, SNAKEMAKE_SEEN),
    ] {
        std::fs::write(&outside, "server file").unwrap();
        let folder = root.path().join(name);
        let data = folder.join("data");
        std::fs::create_dir_all(&data).unwrap();
        std::fs::write(data.join("inside.txt"), "inside").unwrap();
        std::fs::write(folder.join(name), script).unwrap();
        let mut settings = settings(&folder, &folder.join(name));
        let path = |path: &Path| json!(path.to_string_lossy());
        settings.params.insert("outside".into(), path(&outside));
        settings
            .params
            .insert("inside".into(), path(&data.join("inside.txt")));
        settings
            .params
            .insert("report".into(), path(&data.join("report.txt")));
        settings.isolation = Some(Isolation {
            roots: vec![data.clone()],
            launch: Some(folder.join("launch")),
            ..Isolation::default()
        });
        let mut request = RunRequest::new(settings);
        request.policy = uwe::application::Policy::Relaxed;
        let runtime = tokio::runtime::Runtime::new().unwrap();
        let ran = runtime.block_on(request.run());
        let ran = ran.unwrap_or_else(|failure| panic!("{name}: {}", failure.message));
        assert!(
            ran.execution.error.is_none(),
            "{name}: {:?}",
            ran.execution.error
        );
        let report = std::fs::read_to_string(data.join("report.txt")).unwrap();
        assert_eq!(report, seen, "{name}");
        assert_eq!(std::fs::read_to_string(&outside).unwrap(), "server file");
    }
}

/// A compiled Nextflow run is confined while it compiles too: workflow code that reads a file
/// outside the run when the workflow is built fails, as it does in a run.
#[test]
fn confines_compiled_code() {
    if std::env::var_os(CHILD).is_none() {
        return in_child("confines_compiled_code", None);
    }
    let root = tempfile::tempdir().unwrap();
    let outside = root.path().join("outside/secret.txt");
    std::fs::create_dir_all(outside.parent().unwrap()).unwrap();
    std::fs::write(&outside, "server file").unwrap();
    let folder = root.path().join("pipeline");
    let data = folder.join("data");
    std::fs::create_dir_all(&data).unwrap();
    let workflow = folder.join("main.nf");
    let script = "workflow {\n  channel.of(file(params.outside).text).view()\n}\n";
    std::fs::write(&workflow, script).unwrap();
    let mut settings = settings(&folder, &workflow);
    let outside = json!(outside.to_string_lossy());
    settings.params.insert("outside".into(), outside);
    settings.isolation = Some(Isolation {
        roots: vec![data],
        launch: Some(folder.join("launch")),
        ..Isolation::default()
    });
    let mut request = RunRequest::new(settings);
    request.policy = uwe::application::Policy::Relaxed;
    let runtime = tokio::runtime::Runtime::new().unwrap();
    let error = match runtime.block_on(request.run()) {
        Ok(ran) => (ran.execution.error)
            .map(|error| error.to_string())
            .unwrap_or_default(),
        Err(failure) => failure.message,
    };
    assert!(error.contains("workflow code may not reach"), "{error}");
}

/// Tries to change and list the shared store and cache, then reads a stored file by its path,
/// splits a file into chunk files and reads the output of its own task.
const NEXTFLOW_SHARED: &str = "process MAKE {\n  output: path 'own.txt'\n  \
    script: 'echo own > own.txt'\n}\nworkflow {\n  def seen = []\n  \
    try { file(params.stored).delete(); seen << 'deleted' } catch (e) { seen << 'refused delete' }\n  \
    try { file(params.stored).text = 'changed'; seen << 'written' } catch (e) { seen << 'refused write' }\n  \
    try { file(params.cached).text = 'changed'; seen << 'written' } catch (e) { seen << 'refused write' }\n  \
    try { file(params.store).listFiles(); seen << 'listed' } catch (e) { seen << 'refused list' }\n  \
    seen << file(params.stored).text\n  \
    seen << file(params.reads).splitFastq(by: 1, file: true).size()\n  \
    MAKE().map { own -> seen << own.exists(); file(params.report).text = seen.join('\\n') }\n}\n";

/// The same at the top level of a Snakefile.
const SNAKEFILE_SHARED: &str = "import os\nseen = []\ntry:\n    os.remove(config['stored'])\n    \
    seen.append('deleted')\nexcept OSError:\n    seen.append('refused delete')\n\
    for name in ['stored', 'cached']:\n    try:\n        open(config[name], 'w').write('changed')\n        \
    seen.append('written')\n    except OSError:\n        seen.append('refused write')\ntry:\n    \
    os.listdir(config['store'])\n    seen.append('listed')\nexcept OSError:\n    \
    seen.append('refused list')\nseen.append(open(config['stored']).read())\n\
    with open(config['report'], 'w') as written:\n    written.write('\\n'.join(seen))\n\n\
    rule all:\n    input: []\n";

/// Workflow code of an isolated run reads a file of the shared store or cache by its path,
/// but cannot change, remove or list what other runs keep there.
#[test]
fn protects_shared_files() {
    if std::env::var_os(CHILD).is_none() {
        return in_child("protects_shared_files", None);
    }
    let root = tempfile::tempdir().unwrap();
    for (name, script, seen) in [
        (
            "main.nf",
            NEXTFLOW_SHARED,
            "refused delete\nrefused write\nrefused write\nrefused list\nkept\n2\ntrue",
        ),
        (
            "Snakefile",
            SNAKEFILE_SHARED,
            "refused delete\nrefused write\nrefused write\nrefused list\nkept",
        ),
    ] {
        let folder = root.path().join(name);
        let data = folder.join("data");
        let workflow = folder.join("workflow").join(name);
        std::fs::create_dir_all(workflow.parent().unwrap()).unwrap();
        std::fs::create_dir_all(&data).unwrap();
        std::fs::write(&workflow, script).unwrap();
        std::fs::write(data.join("reads.fq"), "@a\nAC\n+\nII\n@b\nGT\n+\nII\n").unwrap();
        let stored = folder.join("store/other-run/result.txt");
        let cached = folder.join("cache/scratch/other-run/result.txt");
        for file in [&stored, &cached] {
            std::fs::create_dir_all(file.parent().unwrap()).unwrap();
            std::fs::write(file, "kept").unwrap();
        }
        let mut settings = settings(&folder, &workflow);
        // The simulator makes up the output of the task; a simulated Snakefile writes nothing.
        settings.executor = (name == "main.nf").then(|| "simulator".into());
        let path = |path: &Path| json!(path.to_string_lossy());
        for (key, value) in [
            ("stored", stored.clone()),
            ("cached", cached.clone()),
            ("store", folder.join("store")),
            ("reads", data.join("reads.fq")),
            ("report", data.join("report.txt")),
        ] {
            settings.params.insert(key.into(), path(&value));
        }
        settings.isolation = Some(Isolation {
            roots: vec![data.clone()],
            launch: Some(folder.join("launch")),
            ..Isolation::default()
        });
        let mut request = RunRequest::new(settings);
        request.policy = uwe::application::Policy::Relaxed;
        let runtime = tokio::runtime::Runtime::new().unwrap();
        let ran = runtime.block_on(request.run());
        let ran = ran.unwrap_or_else(|failure| panic!("{name}: {}", failure.message));
        assert!(
            ran.execution.error.is_none(),
            "{name}: {:?}",
            ran.execution.error
        );
        let report = std::fs::read_to_string(data.join("report.txt")).unwrap();
        assert_eq!(report, seen, "{name}");
        for file in [&stored, &cached] {
            assert_eq!(std::fs::read_to_string(file).unwrap(), "kept", "{name}");
        }
    }
}

/// Lists and reads each folder of `params.folders`, then tries to delete and overwrite its
/// `input.txt` and to make a file and a folder in it.
const NEXTFLOW_READ_ONLY: &str = "workflow {\n  def seen = []\n  \
    params.folders.each { folder ->\n    def input = file(\"${folder}/input.txt\")\n    \
    seen << file(folder).listFiles().collect { it.name }.sort().join(',')\n    \
    seen << input.text\n    \
    try { input.delete(); seen << 'deleted' } catch (e) { seen << 'refused delete' }\n    \
    try { input.text = 'changed'; seen << 'written' } catch (e) { seen << 'refused write' }\n    \
    try { file(\"${folder}/new.txt\").text = 'new'; seen << 'created' } \
    catch (e) { seen << 'refused create' }\n    \
    seen << (file(\"${folder}/made\").mkdirs() ? 'made' : 'refused mkdir')\n  }\n  \
    file(params.report).text = seen.join('\\n')\n}\n";

/// The same at the top level of a Snakefile, which also tries to rename `input.txt`.
const SNAKEFILE_READ_ONLY: &str = "import os\nseen = []\n\
    def tried(done, refused, change):\n    try:\n        change()\n        seen.append(done)\n    \
    except OSError:\n        seen.append(refused)\n\
    for folder in config['folders']:\n    input = os.path.join(folder, 'input.txt')\n    \
    seen.append(','.join(sorted(os.listdir(folder))))\n    seen.append(open(input).read())\n    \
    tried('deleted', 'refused delete', lambda: os.remove(input))\n    \
    tried('written', 'refused write', lambda: open(input, 'w').write('changed'))\n    \
    tried('created', 'refused create', \
    lambda: open(os.path.join(folder, 'new.txt'), 'w').write('new'))\n    \
    tried('made', 'refused mkdir', lambda: os.mkdir(os.path.join(folder, 'made')))\n    \
    tried('renamed', 'refused rename', \
    lambda: os.rename(input, os.path.join(folder, 'moved.txt')))\n\
    with open(config['report'], 'w') as written:\n    written.write('\\n'.join(seen))\n\n\
    rule all:\n    input: []\n";

/// Workflow code of an isolated run lists and reads the read-only folders of the host and the
/// folder of its workflow, but cannot change, remove or add anything in them.
#[test]
fn protects_read_only() {
    if std::env::var_os(CHILD).is_none() {
        return in_child("protects_read_only", None);
    }
    let root = tempfile::tempdir().unwrap();
    let refused = "input\nrefused delete\nrefused write\nrefused create\nrefused mkdir";
    for (name, seen) in [
        (
            "main.nf",
            format!("input.txt\n{refused}\ninput.txt,main.nf\n{refused}"),
        ),
        (
            "Snakefile",
            format!(
                "input.txt\n{refused}\nrefused rename\nSnakefile,input.txt\n{refused}\nrefused rename"
            ),
        ),
    ] {
        let script = match name {
            "main.nf" => NEXTFLOW_READ_ONLY,
            _ => SNAKEFILE_READ_ONLY,
        };
        let folder = root.path().join(name);
        let data = folder.join("data");
        let shared = folder.join("shared");
        let code = folder.join("workflow");
        let workflow = code.join(name);
        for created in [&data, &shared, &code] {
            std::fs::create_dir_all(created).unwrap();
            std::fs::write(created.join("input.txt"), "input").unwrap();
        }
        std::fs::write(&workflow, script).unwrap();
        let mut settings = settings(&folder, &workflow);
        let path = |path: &Path| json!(path.to_string_lossy());
        let folders = json!([path(&shared), path(&code)]);
        (settings.params).insert("folders".into(), folders);
        (settings.params).insert("report".into(), path(&data.join("report.txt")));
        settings.isolation = Some(Isolation {
            roots: vec![data.clone()],
            read_only: vec![shared.clone()],
            launch: Some(folder.join("launch")),
            ..Isolation::default()
        });
        let mut request = RunRequest::new(settings);
        request.policy = uwe::application::Policy::Relaxed;
        let runtime = tokio::runtime::Runtime::new().unwrap();
        let ran = runtime.block_on(request.run());
        let ran = ran.unwrap_or_else(|failure| panic!("{name}: {}", failure.message));
        assert!(
            ran.execution.error.is_none(),
            "{name}: {:?}",
            ran.execution.error
        );
        let report = std::fs::read_to_string(data.join("report.txt")).unwrap();
        assert_eq!(report, seen, "{name}");
        for (kept, count) in [(&shared, 1), (&code, 2)] {
            assert_eq!(std::fs::read_dir(kept).unwrap().count(), count, "{name}");
            let input = std::fs::read_to_string(kept.join("input.txt")).unwrap();
            assert_eq!(input, "input", "{name}");
        }
    }
}

/// An isolated Nextflow run without a launch folder of the host launches in one of its own.
#[test]
fn launches_isolated_run() {
    if std::env::var_os(CHILD).is_none() {
        return in_child("launches_isolated_run", None);
    }
    let folder = tempfile::tempdir().unwrap();
    let workflow = folder.path().join("main.nf");
    std::fs::write(
        &workflow,
        "workflow {\n  file('marker.txt').text = 'here'\n}\n",
    )
    .unwrap();
    let mut settings = settings(folder.path(), &workflow);
    settings.isolation = Some(Isolation::default());
    let mut request = RunRequest::new(settings);
    request.policy = uwe::application::Policy::Relaxed;
    let runtime = tokio::runtime::Runtime::new().unwrap();
    let ran = runtime.block_on(request.run());
    let ran = ran.unwrap_or_else(|failure| panic!("{}", failure.message));
    assert!(ran.execution.error.is_none(), "{:?}", ran.execution.error);
    let launch = folder.path().join("cache/scratch/launch").join(&ran.id);
    assert_eq!(
        std::fs::read_to_string(launch.join("marker.txt")).unwrap(),
        "here"
    );
}

/// Reads and overwrites `service.txt` by a relative path, in the folder that the run starts in.
const SNAKEFILE_RELATIVE: &str = "seen = []\ntry:\n    seen.append(open('service.txt').read())\n\
    except OSError:\n    seen.append('no service file')\nwith open('service.txt', 'w') as written:\n    \
    written.write('changed')\nwith open(config['report'], 'w') as written:\n    \
    written.write('\\n'.join(seen))\n\nrule all:\n    input: []\n";

/// An isolated Snakemake run without a launch folder of the host starts in a folder of its
/// own, not in the working directory of the process, whose files its code must not reach.
#[test]
fn launches_isolated_snakefile() {
    if std::env::var_os(CHILD).is_none() {
        let process = tempfile::tempdir().unwrap();
        let service = process.path().join("service.txt");
        std::fs::write(&service, "service file").unwrap();
        in_child("launches_isolated_snakefile", Some(process.path()));
        assert_eq!(std::fs::read_to_string(&service).unwrap(), "service file");
        return;
    }
    let folder = tempfile::tempdir().unwrap();
    let workflow = folder.path().join("workflow/Snakefile");
    let data = folder.path().join("data");
    std::fs::create_dir_all(workflow.parent().unwrap()).unwrap();
    std::fs::create_dir_all(&data).unwrap();
    std::fs::write(&workflow, SNAKEFILE_RELATIVE).unwrap();
    let mut settings = settings(folder.path(), &workflow);
    let report = data.join("report.txt");
    (settings.params).insert("report".into(), json!(report.to_string_lossy()));
    settings.isolation = Some(Isolation {
        roots: vec![data.clone()],
        ..Isolation::default()
    });
    let mut request = RunRequest::new(settings);
    request.policy = uwe::application::Policy::Relaxed;
    let runtime = tokio::runtime::Runtime::new().unwrap();
    let ran = runtime.block_on(request.run());
    let ran = ran.unwrap_or_else(|failure| panic!("{}", failure.message));
    assert!(ran.execution.error.is_none(), "{:?}", ran.execution.error);
    assert_eq!(std::fs::read_to_string(&report).unwrap(), "no service file");
    let launch = folder.path().join("cache/scratch/launch").join(&ran.id);
    assert_eq!(
        std::fs::read_to_string(launch.join("service.txt")).unwrap(),
        "changed"
    );
}

/// A Snakefile whose target lies below `results`, relative to the folder it starts in.
const SNAKEFILE_TARGET: &str = "rule all:\n    input: 'results/made.txt'\n\n\
    rule make:\n    output: 'results/made.txt'\n    shell: 'echo made > {output}'\n";

/// A compiled Snakemake run starts in its launch folder: the one of the host when confined,
/// else one of its own. It delivers its target from there and writes nothing into the working
/// directory of the process.
#[test]
fn delivers_launched_targets() {
    if std::env::var_os(CHILD).is_none() {
        let process = tempfile::tempdir().unwrap();
        in_child("delivers_launched_targets", Some(process.path()));
        let left: Vec<_> = std::fs::read_dir(process.path())
            .unwrap()
            .flatten()
            .collect();
        assert!(left.is_empty(), "the runs wrote {left:?}");
        return;
    }
    let folder = tempfile::tempdir().unwrap();
    let workflow = folder.path().join("workflow/Snakefile");
    std::fs::create_dir_all(workflow.parent().unwrap()).unwrap();
    std::fs::write(&workflow, SNAKEFILE_TARGET).unwrap();
    let launch = folder.path().join("launch");
    for confined in [true, false] {
        let mut settings = settings(folder.path(), &workflow);
        settings.executor = Some("simulator".into());
        if confined {
            settings.isolation = Some(Isolation {
                launch: Some(launch.clone()),
                ..Isolation::default()
            });
        }
        let mut request = RunRequest::new(settings);
        request.policy = uwe::application::Policy::Relaxed;
        let runtime = tokio::runtime::Runtime::new().unwrap();
        let ran = runtime.block_on(request.run());
        let ran = ran.unwrap_or_else(|failure| panic!("{}", failure.message));
        assert!(ran.execution.error.is_none(), "{:?}", ran.execution.error);
        let started = match confined {
            true => launch.clone(),
            false => folder.path().join("cache/scratch/launch").join(&ran.id),
        };
        // The compiled document names the target where the run makes it.
        let database = run_database::Database::open(&folder.path().join("runs.sqlite")).unwrap();
        let document = database.run(&ran.id).unwrap().unwrap().document.unwrap();
        let text = std::fs::read_to_string(document).unwrap();
        let target = started.join("results/made.txt");
        assert!(
            text.contains(target.to_str().unwrap()),
            "confined {confined}: {text}"
        );
    }
}

/// Splits a file that the run names, like a channel operator of a compiled pipeline does.
const SPLIT: &str = "workflow {\n  channel.of(file(params.outside)).splitText().view()\n}\n";

/// The file operators of a compiled run keep to the folders of an isolated run, as the code
/// of the workflow does: a split of a file outside them fails without reading it.
#[test]
fn confines_file_operators() {
    if std::env::var_os(CHILD).is_none() {
        return in_child("confines_file_operators", None);
    }
    let root = tempfile::tempdir().unwrap();
    let outside = root.path().join("outside/private.txt");
    std::fs::create_dir_all(outside.parent().unwrap()).unwrap();
    std::fs::write(&outside, "private line").unwrap();
    let folder = root.path().join("pipeline");
    let data = folder.join("data");
    std::fs::create_dir_all(&data).unwrap();
    std::fs::write(folder.join("main.nf"), SPLIT).unwrap();
    let mut settings = settings(&folder, &folder.join("main.nf"));
    settings
        .params
        .insert("outside".into(), json!(outside.to_string_lossy()));
    settings.isolation = Some(Isolation {
        roots: vec![data],
        launch: Some(folder.join("launch")),
        ..Isolation::default()
    });
    let printed = std::sync::Arc::new(std::sync::Mutex::new(String::new()));
    let sink = printed.clone();
    settings.print_sink = Some(uwe::run::PrintSink::new(move |text: &str| {
        sink.lock().unwrap().push_str(text);
    }));
    let mut request = RunRequest::new(settings);
    request.policy = uwe::application::Policy::Relaxed;
    let runtime = tokio::runtime::Runtime::new().unwrap();
    let message = match runtime.block_on(request.run()) {
        Ok(ran) => (ran.execution.error.map(|error| error.to_string())).unwrap_or_default(),
        Err(failure) => failure.message,
    };
    assert!(
        message.contains("outside the folders of the run"),
        "{message}"
    );
    assert!(!printed.lock().unwrap().contains("private line"));
}

/// A confined run reads the documents that its workflow includes or imports from the web
/// through its storage, which refuses a host that is not among its allowed hosts, also while
/// the workflow compiles.
#[test]
fn refuses_private_imports() {
    let server = common::WebServer::start(&[
        ("/word.txt", "private-word"),
        (
            "/lib.wdl",
            "version 1.1\ntask hello {\n  command <<< echo private-word >>>\n}\n",
        ),
    ]);
    let address = &server.address;
    let cwl = format!(
        "cwlVersion: v1.2\nclass: CommandLineTool\nbaseCommand: echo\narguments:\n  - $include: {address}/word.txt\ninputs: []\noutputs: []\n"
    );
    let wdl = format!(
        "version 1.1\nimport \"{address}/lib.wdl\" as lib\n\nworkflow main {{\n  call lib.hello\n}}\n"
    );
    for (name, text) in [("tool.cwl", cwl), ("main.wdl", wdl)] {
        let root = tempfile::tempdir().unwrap();
        let data = root.path().join("data");
        std::fs::create_dir_all(&data).unwrap();
        std::fs::write(root.path().join(name), text).unwrap();
        let mut settings = settings(root.path(), &root.path().join(name));
        settings.executor = Some("simulator".into());
        settings.isolation = Some(Isolation {
            roots: vec![data],
            ..Isolation::default()
        });
        let mut request = RunRequest::new(settings);
        request.policy = uwe::application::Policy::Relaxed;
        let runtime = tokio::runtime::Runtime::new().unwrap();
        let message = match runtime.block_on(request.run()) {
            Ok(ran) => (ran.execution.error.map(|error| error.to_string())).unwrap_or_default(),
            Err(failure) => failure.message,
        };
        assert!(message.contains("127.0.0.1"), "{name}: {message}");
    }
}

/// A run of `workflow` with its database, store, cache and outputs in `folder`.
fn settings(folder: &Path, workflow: &Path) -> Settings {
    let mut settings = Settings::new(workflow);
    settings.database = Some(folder.join("runs.sqlite"));
    settings.store = Some(folder.join("store").to_string_lossy().into_owned());
    settings.output_dir = Some(folder.join("outputs"));
    settings.cache = false;
    settings.cache_folder = Some(folder.join("cache"));
    settings
}

/// Runs one test of this file again in a child process with `SERVICE` set and without config
/// files, in the working directory `folder` when one is given.
fn in_child(test: &str, folder: Option<&Path>) {
    let config = tempfile::tempdir().unwrap();
    let mut command = std::process::Command::new(std::env::current_exe().unwrap());
    command.current_dir(folder.unwrap_or(config.path()));
    let output = command
        .args(["--exact", test, "--nocapture", "--test-threads=1"])
        .env(CHILD, "1")
        .env("UWE_SITE_CONFIG", "")
        .env("XDG_CONFIG_HOME", config.path())
        .env(SERVICE, "service-value")
        .output()
        .unwrap();
    let text = [output.stdout, output.stderr].concat();
    let text = String::from_utf8_lossy(&text);
    assert!(output.status.success(), "{test} failed:\n{text}");
}
