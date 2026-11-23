//! The routes of a studio: launching, stopping and following runs, the tool library and the
//! combiner, GA4GH WES, the token, and the read-only page without a studio.
// Copyright (c) 2026 The uwe Contributors
// SPDX-License-Identifier: MIT or Apache-2.0

use std::collections::BTreeSet;
use std::sync::{Arc, Mutex};

use reqwest::blocking::{Client, RequestBuilder, Response};
use run_page::{
    Capabilities, ComposerRequest, Described, ExportRequest, Exported, ImportRequest,
    LaunchRequest, Launched, Phase, Refusal, Reply, RunStarted, ScanRequest, Server, Settings,
    Studio, SuggestRequest, ToolFilter, WorkflowEntry,
};
use serde_json::{Value, json};
use tokio::sync::broadcast;

/// A studio that records what it is asked and starts nothing.
struct Fake {
    launches: Mutex<Vec<Launched>>,
    live: Mutex<Option<broadcast::Sender<Value>>>,
    /// The run that each repeat repeats, and the composed workflow of each composed run.
    asked: Mutex<Vec<Value>>,
}

impl Fake {
    fn new() -> Arc<Fake> {
        let (sender, _) = broadcast::channel(16);
        Arc::new(Fake {
            launches: Mutex::new(Vec::new()),
            live: Mutex::new(Some(sender)),
            asked: Mutex::new(Vec::new()),
        })
    }

    fn start(&self, request: LaunchRequest) -> Result<RunStarted, Refusal> {
        if request.target.as_deref() == Some("missing") {
            return Err(Refusal::failed("there is no workflow missing"));
        }
        let mut launches = self.launches.lock().unwrap();
        let run = format!("run-{}", launches.len() + 1);
        launches.push(Launched {
            run: run.clone(),
            phase: Phase::Queued,
            error: None,
            time: 1_795_437_296_789,
            request,
        });
        Ok(RunStarted::running(run))
    }

    fn request(&self, run: &str) -> LaunchRequest {
        let launches = self.launches.lock().unwrap();
        let found = launches.iter().find(|launched| launched.run == run);
        found.map(|launched| launched.request.clone()).unwrap()
    }
}

impl Studio for Fake {
    fn capabilities(&self) -> Capabilities {
        Capabilities {
            executors: vec!["simulator".into()],
            languages: vec!["cwl".into(), "nextflow".into()],
        }
    }

    fn workflows(&self) -> Reply<'_, Vec<WorkflowEntry>> {
        let entry = WorkflowEntry {
            id: "w.cwl".into(),
            name: "w.cwl".into(),
            ..WorkflowEntry::default()
        };
        Box::pin(async move { Ok(vec![entry]) })
    }

    fn describe(&self, request: LaunchRequest) -> Reply<'_, Described> {
        Box::pin(async move {
            match request.target.as_deref() {
                Some("missing") => Err(Refusal::failed("there is no workflow missing")),
                target => Ok(Described {
                    language: "cwl".into(),
                    outputs: json!({"asked": target}),
                    ..Described::default()
                }),
            }
        })
    }

    fn validate(&self, request: LaunchRequest) -> Reply<'_> {
        Box::pin(async move {
            let diagnostic = json!({"instance_path": "/x", "message": "x is no number"});
            Ok(json!({"accepted": false, "diagnostics": [diagnostic], "values": request.values}))
        })
    }

    fn launch(&self, request: LaunchRequest) -> Reply<'_, RunStarted> {
        Box::pin(async move { self.start(request) })
    }

    fn repeat(&self, run: String, changes: LaunchRequest) -> Reply<'_, RunStarted> {
        Box::pin(async move {
            self.asked.lock().unwrap().push(json!(run));
            self.start(changes)
        })
    }

    fn cancel(&self, run: &str) -> bool {
        let mut launches = self.launches.lock().unwrap();
        let found = launches.iter_mut().find(|launched| launched.run == run);
        match found {
            Some(launched) if launched.phase == Phase::Queued => {
                launched.phase = Phase::Canceling;
                true
            }
            _ => false,
        }
    }

    fn launched(&self, run: &str) -> Option<Launched> {
        let launches = self.launches.lock().unwrap();
        launches
            .iter()
            .find(|launched| launched.run == run)
            .cloned()
    }

    fn launches(&self) -> Vec<Launched> {
        self.launches
            .lock()
            .unwrap()
            .iter()
            .rev()
            .cloned()
            .collect()
    }

    fn follow(&self, run: &str) -> Option<broadcast::Receiver<Value>> {
        let live = self.live.lock().unwrap();
        live.as_ref()
            .filter(|_| run == "run-1")
            .map(broadcast::Sender::subscribe)
    }

    fn tools(&self, filter: ToolFilter) -> Reply<'_> {
        let address = "https://reader:hidden-password@tools.example/fastqc.xml";
        Box::pin(async move { Ok(json!({"query": filter, "origin": address})) })
    }

    fn tool(&self, id: String, version: String) -> Reply<'_> {
        Box::pin(async move { Ok(json!({"definition": {"id": id, "version": version}})) })
    }

    fn scan(&self, request: ScanRequest) -> Reply<'_> {
        Box::pin(async move { Ok(json!({"scanned": request.path})) })
    }

    fn suggest(&self, request: SuggestRequest) -> Reply<'_> {
        Box::pin(async move { Ok(json!({"mode": request.mode, "suggestions": []})) })
    }

    fn check(&self, request: ComposerRequest) -> Reply<'_> {
        Box::pin(async move { Ok(json!({"action": "check", "workflow": request.workflow})) })
    }

    fn export(&self, request: ExportRequest) -> Reply<'_, Exported> {
        Box::pin(async move {
            Ok(Exported {
                text: request.workflow.to_string(),
                file: format!("workflow.{:?}", request.format).to_lowercase(),
            })
        })
    }

    fn import(&self, request: ImportRequest) -> Reply<'_> {
        Box::pin(async move { Ok(json!({"action": "import", "text": request.text})) })
    }

    fn run(&self, request: ComposerRequest) -> Reply<'_, RunStarted> {
        Box::pin(async move {
            self.asked.lock().unwrap().push(request.workflow);
            self.start(LaunchRequest {
                values: Some(request.inputs),
                ..LaunchRequest::default()
            })
        })
    }
}

/// A page with the fake studio on the run database of a fresh folder, and an HTTP client.
fn serve(
    token: Option<&str>,
    runs: Option<BTreeSet<String>>,
) -> (Server, Arc<Fake>, tempfile::TempDir) {
    let root = tempfile::tempdir().unwrap();
    let mut settings = Settings::new(root.path().join("runs.sqlite"));
    settings.token = token.map(str::to_owned);
    let fake = Fake::new();
    let server = Server::start_studio(&settings, None, fake.clone(), runs).unwrap();
    (server, fake, root)
}

fn with_token(request: RequestBuilder, token: Option<&str>) -> RequestBuilder {
    match token {
        Some(token) => request.bearer_auth(token),
        None => request,
    }
}

fn post(server: &Server, route: &str, body: Value, token: Option<&str>) -> Response {
    let request = Client::new()
        .post(format!("{}{route}", server.url()))
        .json(&body);
    with_token(request, token).send().unwrap()
}

fn get(server: &Server, route: &str, token: Option<&str>) -> Response {
    with_token(Client::new().get(format!("{}{route}", server.url())), token)
        .send()
        .unwrap()
}

#[test]
fn read_only_page() {
    let root = tempfile::tempdir().unwrap();
    let server = Server::start(&Settings::new(root.path().join("runs.sqlite"))).unwrap();
    let offered: Value = get(&server, "/api/capabilities", None).json().unwrap();
    assert_eq!(
        offered,
        json!({"studio": false, "executors": [], "languages": []})
    );
    for (route, body) in [
        ("/api/runs", json!({"target": "main.nf"})),
        ("/api/workflows/describe", json!({})),
        ("/api/composer/check", json!({})),
        ("/ga4gh/wes/v1/runs", json!({"workflow_url": "main.nf"})),
    ] {
        assert_eq!(post(&server, route, body, None).status(), 404, "{route}");
    }
    assert_eq!(get(&server, "/api/tools", None).status(), 404);
    let page = get(&server, "/compose", None).text().unwrap();
    assert!(
        page.contains("/composer.js") && page.contains("id=\"tabs\""),
        "{page}"
    );
    server.stop();
}

#[test]
fn launches_and_cancels() {
    let (server, fake, _root) = serve(None, None);
    let offered: Value = get(&server, "/api/capabilities", None).json().unwrap();
    assert_eq!(offered["studio"], true);
    assert_eq!(offered["executors"], json!(["simulator"]));
    let listed: Value = get(&server, "/api/workflows", None).json().unwrap();
    assert_eq!(listed["workflows"][0]["id"], "w.cwl");
    let described: Value = post(
        &server,
        "/api/workflows/describe",
        json!({"target": "w.cwl"}),
        None,
    )
    .json()
    .unwrap();
    assert_eq!(described["outputs"]["asked"], "w.cwl");
    let refused = post(
        &server,
        "/api/workflows/describe",
        json!({"target": "missing"}),
        None,
    );
    assert_eq!(refused.status(), 422);
    assert_eq!(refused.text().unwrap(), "there is no workflow missing");
    let checked: Value = post(
        &server,
        "/api/workflows/validate",
        json!({"values": {"x": "a"}}),
        None,
    )
    .json()
    .unwrap();
    assert_eq!(checked["diagnostics"][0]["instance_path"], "/x");
    let started = post(
        &server,
        "/api/runs",
        json!({"target": "w.cwl", "executor": "simulator"}),
        None,
    );
    assert_eq!(started.status(), 202);
    let started: Value = started.json().unwrap();
    assert_eq!(started, json!({"run_id": "run-1", "state": "running"}));
    assert_eq!(fake.request("run-1").executor.as_deref(), Some("simulator"));
    let unknown = post(&server, "/api/runs", json!({"threads": 2}), None);
    assert_eq!(unknown.status(), 400, "an unknown field is refused");
    let stopped = post(&server, "/api/runs/run-1/cancel", json!({}), None);
    assert_eq!(stopped.status(), 200);
    assert_eq!(stopped.json::<Value>().unwrap()["state"], "canceling");
    assert_eq!(
        post(&server, "/api/runs/run-1/cancel", json!({}), None).status(),
        409
    );
    let again = post(
        &server,
        "/api/runs/run-1/repeat",
        json!({"executor": "local"}),
        None,
    );
    assert_eq!(again.status(), 202);
    assert_eq!(fake.asked.lock().unwrap().as_slice(), [json!("run-1")]);
    assert_eq!(fake.request("run-2").executor.as_deref(), Some("local"));
    server.stop();
}

#[test]
fn streams_live_output() {
    let (server, fake, _root) = serve(None, None);
    let url = format!("{}/api/runs/run-1/logs/stream", server.url());
    let reader = std::thread::spawn(move || Client::new().get(url).send().unwrap().text().unwrap());
    // The page subscribes when the request arrives; events before that would be missed.
    let sender = loop {
        let live = fake.live.lock().unwrap();
        let sender = live.as_ref().unwrap();
        if sender.receiver_count() > 0 {
            break sender.clone();
        }
        drop(live);
        std::thread::yield_now();
    };
    let event = json!({"kind": "log", "task": 3, "chunk": "token https://u:secret-word@host/x\n"});
    sender.send(event).unwrap();
    sender
        .send(json!({"kind": "usage", "task": 3, "cpu_percent": 50.0}))
        .unwrap();
    drop(sender);
    fake.live.lock().unwrap().take();
    let body = reader.join().unwrap();
    assert!(
        body.contains("event: log") && body.contains("\"task\":3"),
        "{body}"
    );
    assert!(body.contains("event: usage"), "{body}");
    assert!(body.contains("event: end"), "{body}");
    assert!(
        !body.contains("secret-word"),
        "the page hides passwords: {body}"
    );
    let other = get(&server, "/api/runs/run-9/logs/stream", None)
        .text()
        .unwrap();
    assert!(other.starts_with("event: end"), "{other}");
    server.stop();
}

#[test]
fn passes_composer_requests() {
    let (server, fake, _root) = serve(None, None);
    let found: Value = get(&server, "/api/tools?q=fastqc&kind=galaxy", None)
        .json()
        .unwrap();
    assert_eq!(
        (found["query"]["kind"].clone(), found["query"]["q"].clone()),
        (json!("galaxy"), json!("fastqc"))
    );
    assert!(
        !found["origin"]
            .as_str()
            .unwrap()
            .contains("hidden-password"),
        "{found}"
    );
    let tool: Value = get(&server, "/api/tools/fastqc/0.12.1", None)
        .json()
        .unwrap();
    assert_eq!(
        tool["definition"],
        json!({"id": "fastqc", "version": "0.12.1"})
    );
    let scanned: Value = post(&server, "/api/tools/scan", json!({"path": "/tools"}), None)
        .json()
        .unwrap();
    assert_eq!(scanned["scanned"], "/tools");
    let suggested: Value = post(
        &server,
        "/api/tools/suggest",
        json!({"mode": "followers"}),
        None,
    )
    .json()
    .unwrap();
    assert_eq!(suggested["mode"], "followers");
    let workflow = json!({"uwe_version": "1", "steps": {}});
    let checked: Value = post(
        &server,
        "/api/composer/check",
        json!({"workflow": workflow}),
        None,
    )
    .json()
    .unwrap();
    assert_eq!(checked["action"], "check");
    let body = json!({"workflow": workflow, "format": "wir"});
    let exported: Value = post(&server, "/api/composer/export", body, None)
        .json()
        .unwrap();
    assert_eq!(exported["file"], "workflow.wir");
    let wrong = json!({"workflow": workflow, "format": "svg"});
    assert_eq!(
        post(&server, "/api/composer/export", wrong, None).status(),
        400
    );
    let imported: Value = post(
        &server,
        "/api/composer/import",
        json!({"text": "a: 1"}),
        None,
    )
    .json()
    .unwrap();
    assert_eq!(imported["action"], "import");
    let started = post(
        &server,
        "/api/composer/run",
        json!({"workflow": workflow, "inputs": {"x": 1}}),
        None,
    );
    assert_eq!(started.status(), 202);
    assert_eq!(fake.asked.lock().unwrap().as_slice(), [workflow]);
    let values = fake.request("run-1").values;
    assert_eq!(values.map(serde_json::Value::Object), Some(json!({"x": 1})));
    server.stop();
}

#[test]
fn requires_studio_token() {
    let (server, _fake, _root) = serve(Some("studio-token"), Some(["allowed".to_owned()].into()));
    let body = json!({"target": "w.cwl"});
    assert_eq!(post(&server, "/api/runs", body.clone(), None).status(), 401);
    assert_eq!(
        post(&server, "/ga4gh/wes/v1/runs", json!({}), None).status(),
        401
    );
    assert_eq!(get(&server, "/api/tools", None).status(), 401);
    let started = post(&server, "/api/runs", body, Some("studio-token"));
    assert_eq!(started.status(), 202);
    let token = Some("studio-token");
    // A page for some runs shows the runs that it started too.
    assert_eq!(
        post(&server, "/api/runs/run-1/cancel", json!({}), token).status(),
        200
    );
    assert_eq!(
        post(&server, "/api/runs/foreign/cancel", json!({}), token).status(),
        404
    );
    assert_eq!(
        get(&server, "/ga4gh/wes/v1/runs/run-1/status", token).status(),
        200
    );
    assert_eq!(
        get(&server, "/ga4gh/wes/v1/runs/foreign/status", token).status(),
        404
    );
    server.stop();
}

#[test]
fn serves_wes() {
    let (server, fake, _root) = serve(None, None);
    let info: Value = get(&server, "/ga4gh/wes/v1/service-info", None)
        .json()
        .unwrap();
    let types = info["workflow_type_versions"].as_object().unwrap();
    assert_eq!(types.keys().collect::<Vec<_>>(), ["NFL", "CWL"]);
    assert_eq!(info["supported_wes_versions"], json!(["1.0.0", "1.1.0"]));
    let request = json!({
        "workflow_url": "file:///work/w.cwl",
        "workflow_type": "CWL",
        "workflow_type_version": "v1.2",
        "workflow_params": {"x": 1},
        "workflow_engine_parameters": {"executor": "simulator", "no_cache": "true"},
        "tags": {"project": "a"},
    });
    let started: Value = post(&server, "/ga4gh/wes/v1/runs", request.clone(), None)
        .json()
        .unwrap();
    assert_eq!(started, json!({"run_id": "run-1"}));
    let asked = json!(fake.request("run-1"));
    assert_eq!(asked["target"], "/work/w.cwl");
    assert_eq!(asked["language"], "cwl");
    assert_eq!(asked["values"], json!({"x": 1}));
    assert_eq!(
        (asked["executor"].clone(), asked["no_cache"].clone()),
        (json!("simulator"), json!(true))
    );
    assert_eq!(asked["wes"], request);
    let form =
        "workflow_url=main.nf&workflow_type=NFL&workflow_params=%7B%22reads%22%3A%22a.fq%22%7D";
    let response = Client::new()
        .post(format!("{}/ga4gh/wes/v1/runs", server.url()))
        .header("content-type", "application/x-www-form-urlencoded")
        .body(form)
        .send()
        .unwrap();
    assert_eq!(response.json::<Value>().unwrap()["run_id"], "run-2");
    assert_eq!(
        json!(fake.request("run-2"))["values"],
        json!({"reads": "a.fq"})
    );
    let parts = "--edge\r\nContent-Disposition: form-data; name=\"workflow_url\"\r\n\r\nmain.nf\r\n\
                 --edge\r\nContent-Disposition: form-data; name=\"tags\"\r\n\r\n{\"a\": \"b\"}\r\n--edge--\r\n";
    let multipart = |body: &'static str| {
        Client::new()
            .post(format!("{}/ga4gh/wes/v1/runs", server.url()))
            .header("content-type", "multipart/form-data; boundary=edge")
            .body(body)
            .send()
            .unwrap()
    };
    assert_eq!(multipart(parts).json::<Value>().unwrap()["run_id"], "run-3");
    let attached = "--edge\r\nContent-Disposition: form-data; name=\"workflow_attachment\"; \
                    filename=\"main.nf\"\r\n\r\nworkflow {}\r\n--edge--\r\n";
    let refused = multipart(attached);
    assert_eq!(refused.status(), 400);
    let problem: Value = refused.json().unwrap();
    assert_eq!(problem["status_code"], 400);
    assert!(
        problem["msg"]
            .as_str()
            .unwrap()
            .contains("workflow_attachment"),
        "{problem}"
    );
    for (body, said) in [
        (
            json!({"workflow_url": "a.wdl", "workflow_type": "WDL"}),
            "no WDL workflows",
        ),
        (
            json!({"workflow_url": "a.nf", "workflow_engine_parameters": {"threads": "2"}}),
            "unknown engine parameter threads",
        ),
        (json!({"workflow_type": "CWL"}), "no workflow_url"),
    ] {
        let refused = post(&server, "/ga4gh/wes/v1/runs", body, None);
        assert_eq!(refused.status(), 400);
        let problem: Value = refused.json().unwrap();
        assert!(problem["msg"].as_str().unwrap().contains(said), "{problem}");
    }
    let status: Value = get(&server, "/ga4gh/wes/v1/runs/run-1/status", None)
        .json()
        .unwrap();
    assert_eq!(status, json!({"run_id": "run-1", "state": "QUEUED"}));
    let page: Value = get(&server, "/ga4gh/wes/v1/runs?page_size=2", None)
        .json()
        .unwrap();
    let ids: Vec<&str> = page["runs"]
        .as_array()
        .unwrap()
        .iter()
        .map(|run| run["run_id"].as_str().unwrap())
        .collect();
    assert_eq!(
        (ids, page["next_page_token"].as_str()),
        (vec!["run-3", "run-2"], Some("2"))
    );
    let rest: Value = get(&server, "/ga4gh/wes/v1/runs?page_size=2&page_token=2", None)
        .json()
        .unwrap();
    assert_eq!(rest["runs"][0]["run_id"], "run-1");
    assert_eq!(rest["runs"][0]["tags"], json!({"project": "a"}));
    assert_eq!(rest["next_page_token"], "");
    let log: Value = get(&server, "/ga4gh/wes/v1/runs/run-1", None)
        .json()
        .unwrap();
    assert_eq!(log["request"], request);
    assert_eq!(log["run_log"]["start_time"], "2026-11-23T12:34:56Z");
    let canceled: Value = post(&server, "/ga4gh/wes/v1/runs/run-1/cancel", json!({}), None)
        .json()
        .unwrap();
    assert_eq!(canceled, json!({"run_id": "run-1"}));
    let status: Value = get(&server, "/ga4gh/wes/v1/runs/run-1/status", None)
        .json()
        .unwrap();
    assert_eq!(status["state"], "CANCELING");
    let again = post(&server, "/ga4gh/wes/v1/runs/run-1/cancel", json!({}), None);
    assert_eq!(again.status(), 409);
    let missing = get(&server, "/ga4gh/wes/v1/runs/nothing/status", None);
    assert_eq!(missing.status(), 404);
    assert_eq!(missing.json::<Value>().unwrap()["status_code"], 404);
    server.stop();
}

#[test]
fn refuses_foreign_origins() {
    let (server, fake, _root) = serve(Some("studio-token"), None);
    let send = |route: &str, body: Value, origin: &str, token: Option<&str>| {
        let request = Client::new()
            .post(format!("{}{route}", server.url()))
            .header("origin", origin)
            .header("cookie", "uwe_token=studio-token")
            .json(&body);
        with_token(request, token).send().unwrap().status()
    };
    let launch = json!({"target": "w.cwl"});
    let wes = json!({"workflow_url": "w.cwl"});
    let foreign = "http://evil.example";
    assert_eq!(send("/api/runs", launch.clone(), foreign, None), 403);
    assert_eq!(send("/ga4gh/wes/v1/runs", wes.clone(), foreign, None), 403);
    assert!(fake.launches.lock().unwrap().is_empty(), "nothing started");
    let own = server.url().to_owned();
    assert_eq!(send("/api/runs", launch.clone(), &own, None), 202);
    assert_eq!(
        send("/api/runs/run-1/cancel", json!({}), foreign, None),
        403
    );
    assert_eq!(send("/api/runs/run-1/cancel", json!({}), &own, None), 200);
    let token = Some("studio-token");
    assert_eq!(send("/ga4gh/wes/v1/runs", wes, foreign, token), 200);
    assert_eq!(
        send("/api/runs/run-2/cancel", json!({}), foreign, token),
        200
    );
    server.stop();
}

#[test]
fn resumes_allowed_runs() {
    let token = Some("studio-token");
    let (server, fake, _root) = serve(token, Some(["allowed".to_owned()].into()));
    let foreign = json!({"target": "w.cwl", "resume": "foreign"});
    assert_eq!(post(&server, "/api/runs", foreign, token).status(), 404);
    let engine = json!({"resume": "foreign"});
    let wes = json!({"workflow_url": "w.cwl", "workflow_engine_parameters": engine});
    assert_eq!(
        post(&server, "/ga4gh/wes/v1/runs", wes, token).status(),
        404
    );
    let repeat = json!({"resume": "foreign"});
    assert_eq!(
        post(&server, "/api/runs/allowed/repeat", repeat, token).status(),
        404
    );
    assert!(fake.launches.lock().unwrap().is_empty(), "nothing started");
    let own = json!({"target": "w.cwl", "resume": "allowed"});
    assert_eq!(post(&server, "/api/runs", own, token).status(), 202);
    let last = json!({"target": "w.cwl", "resume": true});
    assert_eq!(post(&server, "/api/runs", last, token).status(), 202);
    let allowed = Some(["allowed".to_owned()].into());
    assert_eq!(fake.request("run-1").allowed, allowed);
    let shown = fake.request("run-2").allowed.unwrap();
    assert!(
        shown.contains("allowed") && !shown.contains("foreign"),
        "{shown:?}"
    );
    server.stop();
}

#[test]
fn redacts_wes_lists() {
    let (server, _fake, _root) = serve(None, None);
    let address = "https://reader:tag-password@data.example/x?X-Amz-Signature=tag-signature";
    let request = json!({"workflow_url": "w.cwl", "tags": {"source": address}});
    assert_eq!(
        post(&server, "/ga4gh/wes/v1/runs", request, None).status(),
        200
    );
    let listed = get(&server, "/ga4gh/wes/v1/runs", None).text().unwrap();
    assert!(listed.contains("data.example"), "{listed}");
    assert!(
        !listed.contains("tag-password") && !listed.contains("tag-signature"),
        "{listed}"
    );
    let beyond = get(
        &server,
        "/ga4gh/wes/v1/runs?page_token=18446744073709551615",
        None,
    );
    assert_eq!(beyond.status(), 200);
    assert_eq!(beyond.json::<Value>().unwrap()["runs"], json!([]));
    server.stop();
}
