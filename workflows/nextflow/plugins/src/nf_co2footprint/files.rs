//! The text files of nf-co2footprint: the trace of the tasks, the summary of the run, and the
//! JSON-LD provenance of the estimate as a tree from the session down to the tasks.
// Copyright (c) 2026 The uwe Contributors
// SPDX-License-Identifier: MIT or Apache-2.0

use super::calculation::Metrics;
use super::readable::{duration, iso_duration, percent, plain, quantity, scientific};
use crate::report_format::Json;

/// The unit of carbon intensity.
pub(super) const INTENSITY: &str = "gCO\u{2082}e/kWh";

/// The columns of the trace file.
pub(super) const TRACE: &str = "task_id\tstatus\tname\tenergy_consumption\tCO2e\tCO2e_market\t\
     raw_energy_processor\traw_energy_memory\tcarbon_intensity\t%cpu\tmemory\trealtime\tcpus\t\
     powerdraw_cpu\tcpu_model\n";

/// A group of the estimate: the session, the head job, the workflow, a process or a task.
pub(super) struct Node {
    pub(super) level: &'static str,
    pub(super) name: String,
    pub(super) metrics: Metrics,
    pub(super) children: Vec<Node>,
}

/// The trace line of a task; a value that is not known is `-`.
pub(super) fn trace_row(task: &Metrics) -> String {
    let cells = [
        task.task_id.map_or("-".to_owned(), |id| id.to_string()),
        text(&task.status),
        text(&task.name),
        quantity(task.energy * 1000.0, "Wh"),
        quantity(task.co2, "g"),
        task.co2_market
            .map_or("-".to_owned(), |market| quantity(market, "g")),
        quantity(task.raw_processor * 1000.0, "Wh"),
        quantity(task.raw_memory * 1000.0, "Wh"),
        quantity(task.intensity, INTENSITY),
        percent(task.cpu),
        quantity(task.memory, "B"),
        duration(task.realtime, usize::MAX),
        plain(task.cpus),
        quantity(task.powerdraw_cpu, "W"),
        text(&task.cpu_model),
    ];
    format!("{}\n", cells.join("\t"))
}

fn text(value: &Option<String>) -> String {
    value.clone().unwrap_or_else(|| "-".to_owned())
}

/// The summary of the session, with the options as `name: value` lines.
pub(super) fn summary(session: &Metrics, options: &[(&str, String)]) -> String {
    let co2 = (session.co2 * 100.0).round() / 100.0;
    let market = session
        .co2_market
        .map_or("-".to_owned(), |market| quantity(market, "g"));
    let flight = session.co2 * 100.0 / 50_000.0;
    let flight = match flight >= 100.0 {
        true => format!("{} flights from Paris to London", (flight / 100.0).floor()),
        false => format!("{} % of a flight from Paris to London", scientific(flight)),
    };
    let tree = duration(session.co2 / 917.0 * 4.35 * 7.0 * 86_400_000.0, 2);
    let mut text = format!(
        "Total CO\u{2082}e footprint measures of this workflow run (including cached tasks):\n  \
         CO\u{2082}e emissions: {}\n  Energy consumption: {}\n  CO\u{2082}e emissions (market): \
         {market}\n\nWhich equals:\n  - {} km travelled by car\n  - It takes one tree {tree} \
         to sequester the equivalent amount of CO\u{2082} from the atmosphere\n  - {flight}\n\n\
         The calculation of these values is based on the carbon footprint computation method \
         developed in the Green Algorithms project:\n  Lannelongue, L., Grealey, J., Inouye, M., \
         Green Algorithms: Quantifying the Carbon Footprint of Computation.\n  Adv. Sci. 2021, \
         2100707. https://doi.org/10.1002/advs.202100707\n\nnf-co2footprint plugin version: \
         1.4.1\n\nnf-co2footprint options:\n",
        quantity(co2, "g"),
        quantity(session.energy * 1000.0, "Wh"),
        scientific(session.co2 / 175.0),
    );
    for (name, value) in options {
        text.push_str(&format!("  {name}: {value}\n"));
    }
    text
}

/// The provenance file: the tree of the estimate as JSON-LD.
pub(super) fn provenance(session: &Node, metrics_only: bool, nulls: bool) -> String {
    let mut root = Json::object([
        (
            "@context",
            Json::object([
                ("schema", Json::from("https://schema.org/")),
                ("bioschemas", "https://bioschemas.org".into()),
                ("prov", "http://www.w3.org/ns/prov#".into()),
            ]),
        ),
        ("@version", Json::from(1.1)),
        (
            "prov:wasGeneratedBy",
            Json::object([
                ("@type", Json::from("prov:Activity")),
                (
                    "prov:used",
                    Json::object([
                        ("@type", Json::from("schema:SoftwareApplication")),
                        ("name", "nf-co2footprint".into()),
                        ("softwareVersion", "1.4.1".into()),
                        (
                            "archivedAt",
                            "https://doi.org/10.5281/zenodo.14622304".into(),
                        ),
                        (
                            "license",
                            "https://www.apache.org/licenses/LICENSE-2.0.html".into(),
                        ),
                        (
                            "usageInfo",
                            "https://nextflow-io.github.io/nf-co2footprint/".into(),
                        ),
                    ]),
                ),
            ]),
        ),
    ]);
    if let Json::Object(entries) = node(session, metrics_only, nulls) {
        for (key, value) in entries {
            root.insert(&key, value);
        }
    }
    root.pretty()
}

fn node(node: &Node, metrics_only: bool, nulls: bool) -> Json {
    let kind = match node.level {
        "workflow" => "bioschemas:ComputationalWorkflow",
        "task" => "schema:Action",
        _ => "schema:SoftwareApplication",
    };
    let mut json = Json::object([
        ("@id", Json::from(format!("urn:co2footprint:{}", node.name))),
        ("@type", kind.into()),
        (
            "workflowLevel",
            Json::object([
                ("@type", Json::from("schema:PropertyValue")),
                ("value", node.level.into()),
            ]),
        ),
    ]);
    let task = &node.metrics;
    let task_id = match task.task_id {
        Some(id) => (Json::from(id), id.to_string()),
        None => (Json::from("-1"), "-1".to_owned()),
    };
    let cpus = match task.task_id {
        Some(_) => Json::from(task.cpus.round() as i64),
        None => Json::from(task.cpus),
    };
    let words =
        |value: &Option<String>| value.as_ref().map(|text| (Json::from(text), text.clone()));
    let wh = |kwh: f64| Some((Json::from(kwh * 1000.0), "Wh", quantity(kwh * 1000.0, "Wh")));
    let metrics: Vec<(&str, Entry)> = vec![
        ("task_id", Entry::Text(Some(task_id))),
        ("status", Entry::Text(words(&task.status))),
        ("name", Entry::Text(words(&task.name))),
        ("energy_consumption", Entry::Number(wh(task.energy))),
        (
            "CO2e",
            Entry::Number(Some((task.co2.into(), "g", quantity(task.co2, "g")))),
        ),
        (
            "CO2e_market",
            Entry::Number(
                task.co2_market
                    .map(|value| (value.into(), "g", quantity(value, "g"))),
            ),
        ),
        (
            "carbon_intensity",
            Entry::Number(Some((
                task.intensity.into(),
                INTENSITY,
                quantity(task.intensity, INTENSITY),
            ))),
        ),
        (
            "carbon_intensity_market",
            Entry::Number(
                (task.intensity_market)
                    .map(|value| (value.into(), INTENSITY, quantity(value, INTENSITY))),
            ),
        ),
        (
            "%cpu",
            Entry::Number(Some((task.cpu.into(), "%", percent(task.cpu)))),
        ),
        (
            "memory",
            Entry::Number(Some((
                Json::from(task.memory.round() as i64),
                "B",
                quantity(task.memory, "B"),
            ))),
        ),
        ("realtime", Entry::Duration(task.realtime)),
        ("cpus", Entry::Number(Some((cpus, "", plain(task.cpus))))),
        (
            "pue",
            Entry::Number(Some((task.pue.into(), "", plain(task.pue)))),
        ),
        (
            "powerdraw_cpu",
            Entry::Number(Some((
                task.powerdraw_cpu.into(),
                "W",
                quantity(task.powerdraw_cpu, "W"),
            ))),
        ),
        (
            "powerdraw_memory",
            Entry::Number(Some((
                task.powerdraw_memory.into(),
                "W",
                quantity(task.powerdraw_memory, "W"),
            ))),
        ),
        ("cpu_power_model", Entry::Text(None)),
        ("cpu_model", Entry::Text(words(&task.cpu_model))),
        (
            "raw_energy_processor",
            Entry::Number(wh(task.raw_processor)),
        ),
        ("raw_energy_memory", Entry::Number(wh(task.raw_memory))),
    ];
    for (key, entry) in metrics {
        if let Some(value) = entry.json(nulls) {
            json.insert(key, value);
        }
    }
    if !metrics_only {
        for (key, value) in &task.record {
            if json.get(key).is_none() && (nulls || !value.is_null()) {
                let readable = value
                    .as_str()
                    .map_or_else(|| value.to_string(), str::to_owned);
                let entry = Entry::Text(Some((Json::from(value), readable)));
                if let Some(value) = entry.json(nulls) {
                    json.insert(key, value);
                }
            }
        }
    }
    if !node.children.is_empty() {
        let children = node
            .children
            .iter()
            .map(|child| self::node(child, metrics_only, nulls));
        json.insert("hasPart", Json::list(children));
    }
    json
}

/// A metric of a node: a text, a number with its unit, or a duration in milliseconds.
enum Entry {
    Text(Option<(Json, String)>),
    Number(Option<(Json, &'static str, String)>),
    Duration(f64),
}

impl Entry {
    fn json(self, nulls: bool) -> Option<Json> {
        let readable = |text: String| Json::object([("readable", Json::from(text))]);
        match self {
            Entry::Text(Some((value, text))) => Some(Json::object([
                ("@type", Json::from("schema:PropertyValue")),
                ("value", value),
                ("additionalProperty", readable(text)),
            ])),
            Entry::Number(Some((value, unit, text))) => Some(Json::object([
                ("@type", Json::from("schema:QuantitativeValue")),
                ("value", value),
                ("unitText", unit.into()),
                ("additionalProperty", readable(text)),
            ])),
            Entry::Duration(millis) => Some(Json::object([
                ("@type", Json::from("schema:Duration")),
                ("value", iso_duration(millis).into()),
                ("unitText", "ms".into()),
                ("additionalProperty", readable(duration(millis, usize::MAX))),
            ])),
            Entry::Text(None) | Entry::Number(None) if nulls => Some(Json::object([
                ("@type", Json::from("schema:PropertyValue")),
                ("value", Json::Null),
            ])),
            Entry::Text(None) | Entry::Number(None) => None,
        }
    }
}
