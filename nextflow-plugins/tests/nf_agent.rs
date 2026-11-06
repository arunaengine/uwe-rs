//! nf-agent: real runs of a pipeline with agents against a local fake chat completions server,
//! for text and structured answers, a failed model call, `-resume` and the refused features.
// Copyright (c) 2026 The uwe Contributors
// SPDX-License-Identifier: MIT or Apache-2.0

#[path = "common/fake.rs"]
mod fake;

use std::cell::RefCell;
use std::path::Path;
use std::rc::Rc;

use fake::Fake;
use nextflow_evaluator::interpreter::Host;
use nextflow_runtime::driver::{Nextflow, Settings};
use serde_json::{Value, json};
use workflow_graph::{Attempt, Engine, Executor, Launch, Outcome, Running};

const SCRIPT: &str = r#"nextflow.enable.types = true

record Verdict {
    label: String
    confidence: Float
    notes: String?
}

agent qa {
    model 'openai/gpt-5-mini'
    instruction 'You are a concise scientific assistant.'

    input:
    question: String

    output:
    stdout()

    prompt:
    """
    Answer briefly: ${question}
    """
}

agent classify {
    model 'openai/gpt-5-mini'

    input:
    reads: Path
    sample: String

    output:
    verdict: Verdict

    prompt:
    "Classify ${sample} from ${reads}."
}

workflow {
    qa('What is FASTQ format?').view { answer -> "qa: ${answer}" }
    classify(file("${projectDir}/reads.fq"), 's1')
        .view { verdict -> "classify: ${verdict.label} ${verdict.confidence} ${verdict.notes}" }
}
"#;

/// The answer of the fake model, which is text to `qa` and a `Verdict` to `classify`.
const ANSWER: &str = r#"{"label":"pass","confidence":0.9,"notes":null}"#;

/// Runs the pipeline in `folder`; the result has the error of the run and what it printed.
fn run(folder: &Path, resume: bool) -> (Option<String>, Vec<String>) {
    let launch = Launch {
        target: folder.to_owned(),
        scratch: folder.join("scratch"),
        ..Launch::default()
    };
    let printed = Rc::new(RefCell::new(String::new()));
    let settings = Settings {
        host: Box::new(Printed(printed.clone())),
        resume,
        ..Settings::default()
    };
    let mut driver = Nextflow::new(&launch, settings).expect("a driver");
    let execution = Engine::new(&mut Recorded).run("main.nf", &mut driver);
    let mut lines: Vec<String> = printed.borrow().lines().map(str::to_owned).collect();
    lines.sort();
    (execution.error.map(|error| error.to_string()), lines)
}

/// A pipeline folder with the script, its config and the input file.
fn pipeline(script: &str, config: &str) -> tempfile::TempDir {
    let directory = tempfile::tempdir().expect("a temporary directory");
    std::fs::write(directory.path().join("main.nf"), script).expect("writes the script");
    std::fs::write(directory.path().join("nextflow.config"), config).expect("writes the config");
    std::fs::write(directory.path().join("reads.fq"), "@r1\nACGT\n+\nIIII\n").expect("writes");
    directory
}

fn chat(answer: &str) -> Value {
    json!([{"match": "/v1/chat/completions", "body": {"model": "gpt-5-mini-2026",
        "choices": [{"index": 0, "finish_reason": "stop",
            "message": {"role": "assistant", "content": answer}}]}}])
}

#[test]
fn answers_and_resumes() {
    let fake = Fake::start(chat(ANSWER));
    let config = format!("agent.baseUrl = '{}/v1'\n", fake.address);
    let directory = pipeline(SCRIPT, &config);
    let (error, printed) = run(directory.path(), false);
    assert_eq!(error, None, "{printed:?}");
    let expected = [
        "classify: pass 0.9 null".to_owned(),
        format!("qa: {ANSWER}"),
    ];
    assert_eq!(printed, expected);
    let mut requests = fake.requests();
    assert_eq!(requests.len(), 2);
    requests.sort_by_key(|request| request.json()["response_format"].is_object());
    let (text, structured) = (requests[0].json(), requests[1].json());
    assert_eq!(
        requests[0].header("Authorization"),
        Some("Bearer nxf-no-credential")
    );
    assert_eq!(text["model"], "gpt-5-mini");
    assert_eq!(
        text["messages"],
        json!([
            {"role": "system", "content": "You are a concise scientific assistant."},
            {"role": "user", "content":
                "Answer briefly: What is FASTQ format?\n\nInput (JSON):\n\"What is FASTQ format?\""}
        ])
    );
    assert!(text.get("temperature").is_none() && text.get("response_format").is_none());
    assert_eq!(
        structured["messages"],
        json!([{"role": "user", "content": "Classify s1 from reads.fq.\n\nInput (JSON):\n\
            {\"reads\":\"reads.fq\",\"sample\":\"s1\"}"}])
    );
    let format = &structured["response_format"];
    assert_eq!(format["type"], "json_schema");
    assert_eq!(format["json_schema"]["name"], "Output");
    assert_eq!(format["json_schema"]["strict"], true);
    let schema = &format["json_schema"]["schema"];
    assert_eq!(schema["required"], json!(["label", "confidence", "notes"]));
    assert_eq!(
        schema["properties"]["notes"]["type"],
        json!(["string", "null"])
    );
    // A resumed run takes the kept answers and asks the model nothing; a reuse refreshes the
    // time of an answer, which `uwe clean` keeps by its last use.
    let answers = directory.path().join("scratch/nextflow/agents");
    let kept: Vec<_> = (std::fs::read_dir(&answers).expect("lists the answers"))
        .map(|entry| entry.expect("an answer").path())
        .collect();
    assert_eq!(kept.len(), 2);
    for answer in &kept {
        let file = std::fs::File::options().write(true).open(answer);
        let file = file.expect("opens an answer");
        file.set_modified(std::time::UNIX_EPOCH)
            .expect("sets the time");
    }
    let (error, resumed) = run(directory.path(), true);
    assert_eq!(error, None, "{resumed:?}");
    assert_eq!(resumed, expected);
    assert_eq!(fake.requests().len(), 2);
    for answer in &kept {
        let used = std::fs::metadata(answer).and_then(|found| found.modified());
        assert!(
            used.expect("a time") > std::time::UNIX_EPOCH,
            "{}",
            answer.display()
        );
    }
    // Without `-resume` the agents ask again, as Nextflow's tasks run again.
    let (error, _) = run(directory.path(), false);
    assert_eq!(error, None);
    assert_eq!(fake.requests().len(), 4);
}

#[test]
fn fails_model_calls() {
    let routes = json!([{"match": "/v1/chat/completions", "status": 401,
        "body": {"error": {"message": "Incorrect API key provided"}}}]);
    let fake = Fake::start(routes);
    let config = format!("agent.baseUrl = '{}/v1'\n", fake.address);
    let directory = pipeline(SCRIPT, &config);
    let (error, printed) = run(directory.path(), false);
    let error = error.expect("a failed run");
    let message = format!(
        "the model endpoint {}/v1/chat/completions answered HTTP status 401: Incorrect API key \
         provided",
        fake.address
    );
    assert!(error.contains(&message), "{error}\n{printed:?}");
    // A wrong answer for a typed output fails its task too.
    let fake = Fake::start(chat("not JSON"));
    let config = format!("agent.baseUrl = '{}/v1'\n", fake.address);
    let directory = pipeline(SCRIPT, &config);
    let (error, _) = run(directory.path(), false);
    let error = error.expect("a failed run");
    let message = "Agent `classify`: the answer of the model does not fit the output `verdict`";
    assert!(error.contains(message), "{error}");
}

#[test]
fn stops_without_credentials() {
    // Nextflow stops the run before the model call, whatever the agent's `errorStrategy` says.
    let directory = pipeline(SCRIPT, "agent.errorStrategy = 'ignore'\n");
    let (error, printed) = run(directory.path(), false);
    let error = error.expect("a stopped run");
    let message = "Missing LLM provider credential for agent model `openai/gpt-5-mini` - set \
         `agent.apiKey` in the Nextflow configuration, or the `OPENAI_API_KEY` environment \
         variable; the credential namespace is `openai`";
    assert!(error.contains(message), "{error}\n{printed:?}");
}

#[test]
fn refuses_features() {
    let config = "agent.baseUrl = 'http://127.0.0.1:9/v1'\n";
    let tools = SCRIPT.replace(
        "    model 'openai/gpt-5-mini'\n    instruction",
        "    model 'openai/gpt-5-mini'\n    tools 'fs:read'\n    instruction",
    );
    let directory = pipeline(&tools, config);
    let (error, _) = run(directory.path(), false);
    let error = error.expect("a refused run");
    assert!(
        error.contains("Not supported yet: the `tools` directive of agent `qa`"),
        "{error}"
    );
    let directory = pipeline(SCRIPT, "agent.runner = 'pi'\n");
    let (error, _) = run(directory.path(), false);
    let error = error.expect("a refused run");
    assert!(
        error.contains("Not supported yet: `agent.runner = 'pi'`"),
        "{error}"
    );
}

/// Keeps what the run prints.
struct Printed(Rc<RefCell<String>>);

impl Host for Printed {
    fn print(&mut self, text: &str) {
        self.0.borrow_mut().push_str(text);
    }

    // No credential of the machine reaches the runs.
    fn environment(&self, _: &str) -> Option<String> {
        None
    }
}

/// Finishes each task at once, as the executors of uwe do with the tasks of agents, which
/// have neither a command nor an image.
struct Recorded;

impl Executor for Recorded {
    fn run(&mut self, attempt: Attempt) -> Running {
        Running::ended(Ok(Outcome {
            id: attempt.task.id,
            ..Outcome::default()
        }))
    }
}
