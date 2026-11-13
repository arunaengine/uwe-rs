//! The `dag` report of nf-prov: an HTML page with a Mermaid graph of the tasks, their input
//! files and the outputs that no task used.
// Copyright (c) 2026 The uwe Contributors
// SPDX-License-Identifier: MIT or Apache-2.0

use std::collections::HashSet;

use super::{Run, file_name};

/// The page, which loads Mermaid from jsDelivr to draw the graph.
pub(super) fn render(run: &Run) -> String {
    format!(
        "<html>\n<head>\n<meta name=\"viewport\" content=\"width=device-width, \
         user-scalable=no, initial-scale=1, maximum-scale=1\">\n</head>\n<body>\n\
         <pre class=\"mermaid\" style=\"text-align: center;\">\n{}\n</pre>\n\
         <script type=\"module\">\n  import mermaid from \
         'https://cdn.jsdelivr.net/npm/mermaid@10/dist/mermaid.esm.min.mjs';\n  \
         mermaid.initialize({{ startOnLoad: true, maxTextSize: Number.MAX_SAFE_INTEGER, \
         maxEdges: Number.MAX_SAFE_INTEGER }});\n</script>\n</body>\n</html>\n",
        diagram(run)
    )
}

/// The Mermaid text of the graph.
fn diagram(run: &Run) -> String {
    let inputs = run.inputs();
    let mut lines = vec!["subgraph \" \"".to_owned()];
    for (index, path) in inputs.iter().enumerate() {
        lines.push(format!("in{index}[{}]", quote(file_name(path))));
        if path.contains("://") && !path.starts_with("file://") {
            lines.push(format!("click in{index} href \"{path}\" _blank"));
        }
    }
    lines.push("end".to_owned());
    lines.extend(groups(run));
    for (index, task) in run.tasks.iter().enumerate() {
        for input in &task.inputs {
            match run.producer(&input.path) {
                Some((producer, _)) => lines.push(format!(
                    "t{producer} -->|{}| t{index}",
                    quote(file_name(&input.path))
                )),
                None => {
                    if let Some(place) = inputs.iter().position(|path| *path == input.path) {
                        lines.push(format!("in{place} --> t{index}"));
                    }
                }
            }
        }
    }
    let used: HashSet<&str> = (run.tasks.iter())
        .flat_map(|task| task.inputs.iter().map(|input| input.path.as_str()))
        .collect();
    let mut outputs = Vec::new();
    for (index, task) in run.tasks.iter().enumerate() {
        for output in &task.outputs {
            if !used.contains(output.path.as_str()) {
                lines.push(format!("t{index} --> out{}", outputs.len()));
                outputs.push(file_name(&output.path));
            }
        }
    }
    lines.push("subgraph \" \"".to_owned());
    for (index, name) in outputs.iter().enumerate() {
        lines.push(format!("out{index}[{}]", quote(name)));
    }
    lines.push("end".to_owned());
    let mut text = "flowchart TD".to_owned();
    for line in lines {
        text.push_str("\n    ");
        text.push_str(&line);
    }
    text
}

/// The task nodes, inside one subgraph for each workflow that their process names nest in.
fn groups(run: &Run) -> Vec<String> {
    let mut lines = Vec::new();
    let mut open: Vec<String> = Vec::new();
    let mut count = 0;
    for (index, task) in run.tasks.iter().enumerate() {
        let process = task.record["process"].as_str().unwrap_or_default();
        let parts: Vec<&str> = process.split(':').collect();
        let workflows = &parts[..parts.len().saturating_sub(1)];
        let shared = (open.iter().zip(workflows))
            .take_while(|(open, name)| open == name)
            .count();
        while open.len() > shared {
            open.pop();
            lines.push("end".to_owned());
        }
        for name in &workflows[shared..] {
            lines.push(format!("subgraph s{count} [{}]", quote(name)));
            count += 1;
            open.push((*name).to_owned());
        }
        let name = task.record["name"].as_str().unwrap_or_default();
        let last = parts.last().copied().unwrap_or_default();
        let label = match name.strip_prefix(process) {
            Some(rest) => format!("{last}{rest}"),
            None => name.to_owned(),
        };
        lines.push(format!("t{index}([{}])", quote(&label)));
    }
    lines.extend(open.iter().map(|_| "end".to_owned()));
    lines
}

/// A label in double quotes, with Mermaid's entity for a quote inside.
fn quote(text: &str) -> String {
    format!("\"{}\"", text.replace('"', "#quot;"))
}
