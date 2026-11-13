//! nf-prov against the reports that nf-prov 1.7.0 wrote with Nextflow 26.04.6 for the same
//! pipeline; where 1.8.0 differs, the tests follow 1.8.0 and say so.
// Copyright (c) 2026 The uwe Contributors
// SPDX-License-Identifier: MIT or Apache-2.0

mod common;

use std::path::Path;

use serde_json::Value;

/// The config of the recording, with absolute report files, as the tests cannot change the
/// launch directory, and the params of its script.
fn config(folder: &Path, overwrite: bool) -> String {
    let results = folder.join("results");
    format!(
        "plugins {{ id 'nf-prov@1.8.0' }}\n\
         params.input = '{input}'\nparams.greeting = 'hello'\nparams.count = 2\n\
         manifest {{\n    name = 'probe-prov'\n    version = '0.0.1'\n    \
         description = 'Tiny probe workflow'\n    license = 'MIT'\n}}\n\
         prov {{\n  formats {{\n    bco {{ file = '{r}/bco.json'; overwrite = {overwrite} }}\n    \
         dag {{ file = '{r}/dag.html' }}\n    gexf {{ file = '{r}/prov.gexf' }}\n    \
         wrroc {{ file = '{r}/ro-crate-metadata.json'; license = 'https://spdx.org/licenses/MIT'\n      \
         agent {{ name = 'Test Agent'; orcid = 'https://orcid.org/0000-0000-0000-0000'; \
         email = 'agent@example.org' }}\n      \
         organization {{ name = 'Test Org'; ror = 'https://ror.org/000000000' }}\n      \
         publisher = 'https://ror.org/000000000' }}\n  }}\n}}\n",
        input = folder.join("input.txt").display(),
        r = results.display(),
    )
}

/// Runs the probe pipeline and gives the text of each report, by file name. The fixture's
/// `main.nf` is the recorded probe with its `publishDir` below `projectDir`.
fn reports(folder: &Path) -> impl Fn(&str) -> String {
    common::copy(
        &common::fixtures("nf_prov"),
        folder,
        &["main.nf", "input.txt"],
    );
    std::fs::write(folder.join("nextflow.config"), config(folder, false)).expect("writes");
    let (execution, printed) = common::run(folder, "sad_marconi");
    assert!(
        execution.error.is_none(),
        "{:?}\n{printed}",
        execution.error
    );
    let results = folder.join("results");
    move |name| std::fs::read_to_string(results.join(name)).expect("the report exists")
}

fn fixture(name: &str) -> String {
    std::fs::read_to_string(common::fixtures("nf_prov").join(name)).expect("reads the fixture")
}

/// The keys of the objects at one indent of a pretty-printed JSON text, in order.
fn keys(text: &str, indent: usize) -> Vec<String> {
    let prefix = format!("{}\"", " ".repeat(indent));
    (text.lines())
        .filter_map(|line| line.strip_prefix(&prefix))
        .filter_map(|rest| rest.split_once("\":").map(|(key, _)| key.to_owned()))
        .collect()
}

fn name(path: &str) -> &str {
    path.rsplit('/').next().unwrap_or(path)
}

#[test]
fn writes_bco() {
    let directory = tempfile::tempdir().expect("a temporary directory");
    let text = reports(directory.path())("bco.json");
    let recorded = fixture("bco.json");
    assert_eq!(keys(&text, 4), keys(&recorded, 4), "the domains in order");
    let nulls = ["derived_from", "obsolete_after", "embargo"];
    let domain: Vec<String> = (keys(&recorded, 8).into_iter())
        .filter(|key| !nulls.contains(&key.as_str()))
        .collect();
    assert_eq!(
        keys(&text, 8)[..domain.len()],
        domain[..],
        "1.8.0 leaves nulls out"
    );
    let bco: Value = serde_json::from_str(&text).expect("JSON");
    let recorded: Value = serde_json::from_str(&recorded).expect("JSON");
    let etag = bco["etag"].as_str().expect("an etag");
    assert!(etag.len() == 64 && etag.bytes().all(|byte| byte.is_ascii_hexdigit()));
    assert!(
        bco["object_id"]
            .as_str()
            .is_some_and(|id| id.starts_with("urn:uuid:"))
    );
    let steps = |bco: &Value| -> Vec<(String, u64, Vec<String>)> {
        let steps = bco["description_domain"]["pipeline_steps"]
            .as_array()
            .cloned();
        (steps.unwrap_or_default().iter())
            .map(|step| {
                let files = (step["output_list"].as_array().into_iter().flatten())
                    .map(|file| name(file["uri"].as_str().unwrap_or_default()).to_owned())
                    .collect();
                let description = step["description"].as_str().unwrap_or_default().to_owned();
                (
                    description,
                    step["step_number"].as_u64().unwrap_or(0),
                    files,
                )
            })
            .collect()
    };
    let mut expected = steps(&recorded);
    for (index, step) in expected.iter_mut().enumerate() {
        // 1.7.0 numbered the tasks; 1.8.0 gives each its process depth.
        step.1 = if index == 0 { 1 } else { 2 };
    }
    assert_eq!(steps(&bco), expected);
    let outputs = |bco: &Value| -> Vec<(String, String, String)> {
        let outputs = bco["io_domain"]["output_subdomain"].as_array().cloned();
        let mut outputs: Vec<_> = (outputs.unwrap_or_default().iter())
            .map(|output| {
                let target = output["uri"]["uri"].as_str().unwrap_or_default();
                let target = target.rsplit_once("/results/").map_or("", |(_, rest)| rest);
                (
                    target.to_owned(),
                    name(output["uri"]["filename"].as_str().unwrap_or_default()).to_owned(),
                    output["mediatype"].as_str().unwrap_or_default().to_owned(),
                )
            })
            .collect();
        outputs.sort();
        outputs
    };
    assert_eq!(outputs(&bco), outputs(&recorded));
    let targets: Vec<&str> = (bco["io_domain"]["output_subdomain"]
        .as_array()
        .into_iter()
        .flatten())
    .filter_map(|output| output["uri"]["uri"].as_str())
    .collect();
    assert!(targets.is_sorted(), "1.8.0 sorts the outputs: {targets:?}");
    let input = format!("file://{}/input.txt", directory.path().display());
    assert_eq!(
        bco["io_domain"]["input_subdomain"][0]["uri"]["uri"],
        input.as_str()
    );
    assert_eq!(
        bco["parametric_domain"][2]["step"], "1",
        "SPLIT reads params.input"
    );
}

#[test]
fn writes_mermaid() {
    let directory = tempfile::tempdir().expect("a temporary directory");
    let text = reports(directory.path())("dag.html");
    let diagram = |text: &str| {
        let start = text.find("flowchart TD").expect("a diagram");
        let end = text[start..].find("</pre>").expect("its end");
        text[start..start + end].to_owned()
    };
    // The recording with the tasks in uwe's completion order and 1.8.0's quoted labels.
    let recorded = diagram(&fixture("dag.html"))
        .replace("SHOUT (3)", "SHOUT (x)")
        .replace("SHOUT (1)", "SHOUT (3)")
        .replace("SHOUT (x)", "SHOUT (1)")
        .replace("part_02.txt| t1", "part_00.txt| t1")
        .replace("part_00.txt| t3", "part_02.txt| t3")
        .replace("out0[part_02.up.txt]", "out0[part_00.up.txt]")
        .replace("out2[part_00.up.txt]", "out2[part_02.up.txt]");
    let quoted: Vec<String> = (recorded.lines())
        .map(|line| match line.split_once("-->|") {
            Some((from, rest)) => {
                let (label, to) = rest.split_once('|').unwrap_or_default();
                format!("{from}-->|\"{label}\"|{to}")
            }
            None => match line.split_once("[") {
                Some((node, label)) if line.trim().starts_with("out") => {
                    format!("{node}[\"{}", label.replace(']', "\"]"))
                }
                _ => line.to_owned(),
            },
        })
        .collect();
    assert_eq!(diagram(&text), format!("{}\n", quoted.join("\n")));
}

#[test]
fn writes_gexf() {
    let directory = tempfile::tempdir().expect("a temporary directory");
    let text = reports(directory.path())("prov.gexf");
    let recorded = fixture("prov.gexf");
    let header = |text: &str| {
        let end = text.find("<nodes>").expect("nodes");
        let date = text.find("lastmodifieddate=").expect("a date");
        format!("{}{}", &text[..date], &text[date + 30..end])
    };
    assert_eq!(header(&text), header(&recorded));
    let nodes = |text: &str| {
        let mut nodes: Vec<String> = (text.split("<node id=\"").skip(1))
            .map(|node| {
                let label = node
                    .split("label=\"")
                    .nth(1)
                    .and_then(|rest| rest.split('"').next());
                let size = node
                    .split("<viz:size value=\"")
                    .nth(1)
                    .and_then(|rest| rest.split('"').next());
                format!("{} {}", label.unwrap_or_default(), size.unwrap_or("-"))
            })
            .collect();
        nodes.sort();
        nodes
    };
    assert_eq!(nodes(&text), nodes(&recorded));
    let ids: Vec<&str> = (text.split("<node id=\"").skip(1))
        .filter_map(|node| node.split('"').next())
        .collect();
    let edges: Vec<&str> = text.split("<edge ").skip(1).collect();
    assert_eq!(edges.len(), recorded.split("<edge ").count() - 1);
    for edge in edges {
        for end in ["source=\"", "target=\""] {
            let id = edge
                .split(end)
                .nth(1)
                .and_then(|rest| rest.split('"').next());
            assert!(
                ids.contains(&id.unwrap_or_default()),
                "{edge} has its nodes"
            );
        }
    }
}

#[test]
fn writes_crate() {
    let directory = tempfile::tempdir().expect("a temporary directory");
    let text = reports(directory.path())("ro-crate-metadata.json");
    let document: Value = serde_json::from_str(&text).expect("JSON");
    let recorded: Value = serde_json::from_str(&fixture("ro-crate-metadata.json")).expect("JSON");
    let types = |document: &Value| -> Vec<String> {
        let mut types: Vec<String> = (document["@graph"].as_array().into_iter().flatten())
            .map(|entity| entity["@type"].to_string())
            .collect();
        types.sort();
        types
    };
    // 1.8.0 gives each module its script, and copies the input inside the project.
    assert_eq!(types(&document), types(&recorded));
    let entity = |id: &str| {
        (document["@graph"].as_array().into_iter().flatten())
            .find(|entity| entity["@id"] == id)
            .cloned()
            .unwrap_or_else(|| panic!("the crate has {id}"))
    };
    let engine = "https://github.com/nextflow-io/nextflow/releases/tag/v26.04.6";
    assert_eq!(entity(engine)["softwareVersion"], "26.04.6");
    assert_eq!(
        entity("#process-step/SHOUT")["workExample"]["@id"],
        "#module/main.nf/SHOUT"
    );
    assert_eq!(entity("#process-step/SHOUT")["position"], 2);
    assert_eq!(
        entity("./")["publisher"]["@id"],
        "https://ror.org/000000000"
    );
    assert_eq!(
        entity("https://ror.org/000000000")["url"],
        "https://ror.org/000000000"
    );
    assert_eq!(entity("input.txt")["encodingFormat"], "text/plain");
    assert_eq!(
        entity("#param/input/value")["exampleOfWork"]["@id"],
        "#param/input"
    );
    let start = entity("#sad_marconi")["startTime"]
        .as_str()
        .unwrap_or_default()
        .to_owned();
    assert_eq!(start.len(), "2026-11-06T10:22:01+01:00".len(), "{start}");
    for name in [
        "main.nf",
        "nextflow.config",
        "input.txt",
        "shout/part_00.up.txt",
    ] {
        assert!(
            directory.path().join("results").join(name).is_file(),
            "{name} is in the crate"
        );
    }
    let config = std::fs::read_to_string(directory.path().join("results/nextflow.config"))
        .expect("the config");
    assert!(
        config.starts_with("manifest {\n   description = 'Tiny probe workflow'\n"),
        "{config}"
    );
}

#[test]
fn refuses_existing_reports() {
    let directory = tempfile::tempdir().expect("a temporary directory");
    let folder = directory.path();
    common::copy(
        &common::fixtures("nf_prov"),
        folder,
        &["main.nf", "input.txt"],
    );
    std::fs::write(folder.join("nextflow.config"), config(folder, false)).expect("writes");
    let existing = folder.join("results/bco.json");
    std::fs::create_dir_all(folder.join("results")).expect("makes the folder");
    std::fs::write(&existing, "{}").expect("writes the file");
    let (execution, _) = common::run(folder, "sad_marconi");
    let error = execution
        .error
        .map(|error| error.to_string())
        .unwrap_or_default();
    let message = format!("Provenance file already exists: {}", existing.display());
    assert!(error.contains(&message), "{error}");
    std::fs::write(folder.join("nextflow.config"), config(folder, true)).expect("writes");
    let (execution, _) = common::run(folder, "sad_marconi");
    assert!(execution.error.is_none(), "{:?}", execution.error);
}

/// A crate in the project folder never replaces the pipeline's own `nextflow.config`.
#[test]
fn keeps_project_config() {
    let directory = tempfile::tempdir().expect("a temporary directory");
    let folder = directory.path();
    common::copy(
        &common::fixtures("nf_prov"),
        folder,
        &["main.nf", "input.txt"],
    );
    let config = format!(
        "plugins {{ id 'nf-prov@1.8.0' }}\n\
         params.input = '{}'\nparams.greeting = 'hello'\nparams.count = 2\n\
         prov.formats.wrroc.file = '{}/ro-crate-metadata.json'\n",
        folder.join("input.txt").display(),
        folder.display(),
    );
    std::fs::write(folder.join("nextflow.config"), &config).expect("writes");
    let (execution, _) = common::run(folder, "sad_marconi");
    let kept = std::fs::read_to_string(folder.join("nextflow.config")).expect("the config");
    assert_eq!(kept, config);
    assert!(execution.error.is_none(), "{:?}", execution.error);
}
