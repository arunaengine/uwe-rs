//! The studio of `uwe serve` over HTTP: the tool library, the combiner, describing, checking
//! and starting workflows, a composed run, a run again and GA4GH WES, on the simulator.
// Copyright (c) 2026 The uwe Contributors
// SPDX-License-Identifier: MIT or Apache-2.0

use std::path::Path;
use std::sync::Arc;

use run_page::{Server, Settings};
use serde_json::{Value, json};
use uwe::serve::{Backend, Options};

const SORT: &str = r#"#!/usr/bin/env cwl-runner
cwlVersion: v1.2
class: CommandLineTool
label: Sort lines
$namespaces:
  edam: http://edamontology.org/
hints:
  DockerRequirement:
    dockerPull: debian:stable-slim
baseCommand: [sort]
inputs:
  text:
    type: File
    format: edam:format_2330
    inputBinding:
      position: 1
stdout: sorted.txt
outputs:
  sorted:
    type: stdout
    format: edam:format_2330
"#;

const COUNT: &str = r#"<tool id="count_lines" name="Count lines" version="1.0" profile="22.05">
    <command detect_errors="exit_code"><![CDATA[
        wc -l < '$input' > '$output'
    ]]></command>
    <inputs>
        <param name="input" type="data" format="txt" label="Lines to count"/>
    </inputs>
    <outputs>
        <data name="output" format="txt"/>
    </outputs>
</tool>
"#;

const WORKFLOW: &str = r#"uwe_version: "1"
name: counting
description: Sorts and counts the lines of each file.
tools:
  - tools
inputs:
  texts:
    type: file
    list: true
    format: format_2330
steps:
  sort:
    tool: sort
    in:
      text: texts
  count:
    tool: count_lines
    in:
      input: sort/sorted
"#;

/// A studio over the tools of a fresh folder, with a page on its run database.
struct Studio {
    root: tempfile::TempDir,
    backend: Arc<Backend>,
    server: Server,
}

impl Studio {
    fn start() -> Studio {
        let root = tempfile::tempdir().unwrap();
        let tools = root.path().join("tools");
        std::fs::create_dir_all(&tools).unwrap();
        std::fs::write(tools.join("sort.cwl"), SORT).unwrap();
        std::fs::write(tools.join("count.xml"), COUNT).unwrap();
        std::fs::write(root.path().join("counting.uwe.yaml"), WORKFLOW).unwrap();
        std::fs::write(root.path().join("a.txt"), "b\na\n").unwrap();
        let database = root.path().join("runs.sqlite");
        let mut options = Options::new(database.clone(), root.path().join("studio"));
        options.tools = vec![tools];
        options.workflows = vec![root.path().to_owned()];
        options.cache_folder = Some(root.path().join("cache"));
        options.setup = Some(Arc::new(uwe::setup::Setup::default));
        let backend = Arc::new(Backend::new(options));
        let server = Server::start_studio(&Settings::new(database), None, backend.clone(), None)
            .expect("the page starts");
        Studio {
            root,
            backend,
            server,
        }
    }

    fn path(&self, name: &str) -> String {
        self.root.path().join(name).to_string_lossy().into_owned()
    }

    fn get(&self, route: &str) -> (u16, Value) {
        let response = reqwest::blocking::get(format!("{}{route}", self.server.url())).unwrap();
        answer(response)
    }

    fn post(&self, route: &str, body: Value) -> (u16, Value) {
        let response = reqwest::blocking::Client::new()
            .post(format!("{}{route}", self.server.url()))
            .header("content-type", "application/json")
            .body(body.to_string())
            .send()
            .unwrap();
        answer(response)
    }

    /// Waits until the run ends: the stream of its live output ends with it.
    fn ended(&self, run: &str) -> Value {
        let url = format!("{}/api/runs/{run}/logs/stream", self.server.url());
        let streamed = reqwest::blocking::get(url).unwrap().text().unwrap();
        assert!(streamed.contains("event: end"), "{streamed}");
        let (code, status) = self.get(&format!("/ga4gh/wes/v1/runs/{run}/status"));
        assert_eq!(code, 200, "{status}");
        status
    }

    fn stop(self) {
        self.server.stop();
        self.backend.stop();
    }
}

fn answer(response: reqwest::blocking::Response) -> (u16, Value) {
    let code = response.status().as_u16();
    let text = response.text().unwrap();
    (
        code,
        serde_json::from_str(&text).unwrap_or(Value::String(text)),
    )
}

/// The workflow of the combiner, in the JSON form of a `.uwe.yaml` file.
fn composed(tools: &Value) -> Value {
    let version = |id: &str| {
        let found = tools
            .as_array()
            .unwrap()
            .iter()
            .find(|tool| tool["id"] == id);
        found.expect("the tool is in the library")["version"].clone()
    };
    json!({
        "uwe_version": "1",
        "name": "page",
        "inputs": {"texts": {"type": "file", "list": true, "format": "format_2330"}},
        "steps": {
            "sort": {"tool": "sort", "version": version("sort"), "in": {"text": "texts"}},
            "count": {"tool": "count_lines", "version": version("count_lines"),
                "in": {"input": {"from": "sort/sorted", "asserts": ["indexed"]}}},
        },
        "outputs": {"counts": {"from": "count/output"}},
    })
}

#[test]
fn explores_tools() {
    let studio = Studio::start();
    let (code, found) = studio.get("/api/tools");
    assert_eq!(code, 200, "{found}");
    assert_eq!(found["count"], 2);
    assert_eq!(
        found["facets"]["ecosystems"],
        json!({"cwl": 1, "galaxy": 1})
    );
    assert_eq!(found["labels"]["format_2330"], "Textual format");
    let (_, cwl) = studio.get("/api/tools?kind=cwl");
    assert_eq!(cwl["tools"][0]["id"], "sort");
    assert_eq!(
        cwl["tools"][0]["inputs"][0]["formats"],
        json!(["format_2330"])
    );
    let (_, counted) = studio.get("/api/tools?q=count&kind=all");
    assert_eq!(counted["tools"].as_array().unwrap().len(), 1);
    let sort = &cwl["tools"][0];
    let route = format!("/api/tools/sort/{}", sort["version"].as_str().unwrap());
    let (code, tool) = studio.get(&route);
    assert_eq!(code, 200, "{tool}");
    assert_eq!(tool["definition"]["title"], "Sort lines");
    assert_eq!(studio.get("/api/tools/sort/0.0").0, 404);
    let more = studio.root.path().join("more");
    std::fs::create_dir_all(&more).unwrap();
    std::fs::write(
        more.join("copy.xml"),
        COUNT.replace("count_lines", "copy_lines"),
    )
    .unwrap();
    std::fs::write(more.join("notes.xml"), "<notes/>").unwrap();
    let (code, scanned) = studio.post("/api/tools/scan", json!({"path": more}));
    assert_eq!(code, 200, "{scanned}");
    assert_eq!(
        (scanned["count"].clone(), scanned["added"][0]["id"].clone()),
        (json!(3), json!("copy_lines"))
    );
    assert_eq!(
        scanned["skipped"][0]["reason"],
        "the XML file is no Galaxy tool"
    );
    let followers = json!({"mode": "followers", "tool": "sort", "port": "sorted"});
    let (code, suggested) = studio.post("/api/tools/suggest", followers);
    assert_eq!(code, 200, "{suggested}");
    let ports: Vec<String> = (suggested["suggestions"].as_array().unwrap().iter())
        .map(|item| {
            format!(
                "{}/{}",
                item["tool"].as_str().unwrap(),
                item["port"].as_str().unwrap()
            )
        })
        .collect();
    assert!(ports.contains(&"count_lines/input".to_owned()), "{ports:?}");
    assert_eq!(suggested["suggestions"][0]["ecosystem"], "galaxy");
    let producers = json!({"mode": "producers", "tool": "count_lines", "port": "input"});
    let (_, suggested) = studio.post("/api/tools/suggest", producers);
    assert!(
        suggested["suggestions"]
            .as_array()
            .unwrap()
            .iter()
            .any(|item| item["tool"] == "sort")
    );
    let (code, _) = studio.post("/api/tools/suggest", json!({"mode": "sideways"}));
    assert_eq!(code, 400);
    studio.stop();
}

#[test]
fn checks_composed_workflows() {
    let studio = Studio::start();
    let (_, found) = studio.get("/api/tools");
    let workflow = composed(&found["tools"]);
    let (code, checked) = studio.post("/api/composer/check", json!({"workflow": workflow}));
    assert_eq!(code, 200, "{checked}");
    let report = &checked["report"];
    assert_ne!(report["fit"], "does_not_fit", "{report}");
    assert_eq!(report["nodes"][0]["id"], "sort");
    assert_eq!(report["nodes"][0]["scatter"], true);
    assert_eq!(
        checked["edges"][0]["from"],
        json!({"node": "sort", "port": "sorted"})
    );
    let mut broken = workflow.clone();
    broken["steps"]["count"]["tool"] = json!("nothing");
    let (_, checked) = studio.post("/api/composer/check", json!({"workflow": broken}));
    assert_eq!(checked["report"]["fit"], "does_not_fit");
    for (format, part) in [
        ("yaml", "asserts:"),
        ("json", "\"uwe_version\""),
        ("wir", "\"wir_version\""),
    ] {
        let (code, exported) = studio.post(
            "/api/composer/export",
            json!({"workflow": workflow, "format": format}),
        );
        assert_eq!(code, 200, "{format}: {exported}");
        assert!(
            exported["text"].as_str().unwrap().contains(part),
            "{format}: {exported}"
        );
    }
    let (code, refused) = studio.post(
        "/api/composer/export",
        json!({"workflow": broken, "format": "wir"}),
    );
    assert_eq!(code, 422, "{refused}");
    let text = std::fs::read_to_string(studio.root.path().join("counting.uwe.yaml")).unwrap();
    let (code, imported) = studio.post("/api/composer/import", json!({"text": text}));
    assert_eq!(code, 200, "{imported}");
    assert_eq!(
        imported["workflow"]["steps"]["count"]["in"]["input"],
        "sort/sorted"
    );
    assert_eq!(imported["missing"], json!([]));
    let (code, _) = studio.post("/api/composer/import", json!({"text": "steps: {}"}));
    assert_eq!(code, 400);
    studio.stop();
}

#[test]
fn runs_from_page() {
    let studio = Studio::start();
    let (code, listed) = studio.get("/api/workflows");
    assert_eq!(code, 200, "{listed}");
    let workflow = studio.path("counting.uwe.yaml");
    let entry = listed["workflows"]
        .as_array()
        .unwrap()
        .iter()
        .find(|item| item["path"] == workflow.as_str());
    assert_eq!(
        entry.expect("the workflow is listed")["language"],
        "composed"
    );
    let (code, described) = studio.post("/api/workflows/describe", json!({"target": workflow}));
    assert_eq!(code, 200, "{described}");
    assert_eq!(
        described["form"]["schema"]["properties"]["texts"]["type"],
        "array"
    );
    assert_eq!(described["support"]["parts"].as_array().unwrap().len(), 2);
    let wrong = json!({"target": workflow, "values": {"texts": [5]}});
    let (_, checked) = studio.post("/api/workflows/validate", wrong);
    assert_eq!(checked["accepted"], false);
    assert_eq!(checked["diagnostics"][0]["instance_path"], "/texts/0");
    let missing = json!({"target": studio.path("none.cwl"), "executor": "simulator"});
    assert_eq!(studio.post("/api/runs", missing).0, 422);
    assert_eq!(
        studio.post("/api/runs", json!({"executor": "simulator"})).0,
        400
    );
    // A plain path takes the format that the workflow input declares, as the CWL tool needs it.
    let values = json!({"texts": [studio.path("a.txt")]});
    let request = json!({"target": workflow, "executor": "simulator", "values": values});
    let (code, started) = studio.post("/api/runs", request);
    assert_eq!(code, 202, "{started}");
    let run = started["run_id"].as_str().unwrap().to_owned();
    assert_eq!(studio.ended(&run)["state"], "COMPLETE");
    let (_, log) = studio.get(&format!("/ga4gh/wes/v1/runs/{run}"));
    assert_eq!(log["task_logs"].as_array().unwrap().len(), 2, "{log}");
    assert_eq!(log["request"]["workflow_params"], values);
    let (code, again) = studio.post(&format!("/api/runs/{run}/repeat"), json!({}));
    assert_eq!(code, 202, "{again}");
    let again = again["run_id"].as_str().unwrap().to_owned();
    assert_ne!(again, run);
    assert_eq!(studio.ended(&again)["state"], "COMPLETE");
    let (_, found) = studio.get("/api/tools");
    let request =
        json!({"workflow": composed(&found["tools"]), "inputs": values, "executor": "simulator"});
    let (code, started) = studio.post("/api/composer/run", request);
    assert_eq!(code, 202, "{started}");
    let composed_run = started["run_id"].as_str().unwrap().to_owned();
    assert_eq!(studio.ended(&composed_run)["state"], "COMPLETE");
    let database = run_database::Database::open(Path::new(&studio.path("runs.sqlite"))).unwrap();
    for id in [&run, &again, &composed_run] {
        assert_eq!(
            database.run(id).unwrap().unwrap().state,
            "succeeded",
            "{id}"
        );
    }
    studio.stop();
}

#[test]
fn runs_from_wes() {
    let studio = Studio::start();
    let (code, info) = studio.get("/ga4gh/wes/v1/service-info");
    assert_eq!(code, 200, "{info}");
    assert!(info["workflow_type_versions"]["CWL"].is_object(), "{info}");
    let request = json!({
        "workflow_url": studio.path("tools/sort.cwl"),
        "workflow_type": "CWL",
        "workflow_params": {"text": studio.path("a.txt")},
        "workflow_engine_parameters": {"executor": "simulator"},
        "tags": {"lab": "one"},
    });
    let (code, started) = studio.post("/ga4gh/wes/v1/runs", request.clone());
    assert_eq!(code, 200, "{started}");
    let run = started["run_id"].as_str().unwrap().to_owned();
    assert_eq!(studio.ended(&run)["state"], "COMPLETE");
    let (_, log) = studio.get(&format!("/ga4gh/wes/v1/runs/{run}"));
    assert_eq!(log["request"], request);
    assert_eq!(log["state"], "COMPLETE");
    let (_, listed) = studio.get("/ga4gh/wes/v1/runs");
    assert_eq!(listed["runs"][0]["run_id"], run.as_str());
    assert_eq!(listed["runs"][0]["tags"], json!({"lab": "one"}));
    let (code, _) = studio.post(&format!("/ga4gh/wes/v1/runs/{run}/cancel"), json!({}));
    assert_eq!(code, 409, "a run that ended does not stop");
    studio.stop();
}
