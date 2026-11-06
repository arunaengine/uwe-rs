//! The HTML report of nf-co2footprint: the totals of the run, the emissions of each process,
//! a table of the tasks and the options, with its data as the plugin's page globals.
// Copyright (c) 2026 The uwe Contributors
// SPDX-License-Identifier: MIT or Apache-2.0

use std::fmt::Write as _;

use super::calculation::Metrics;
use super::files::{INTENSITY, Node};
use super::readable::{duration, percent, plain, quantity, scientific};
use crate::report_format::{Json, offset_time};

/// What the report shows of the run besides the estimate.
pub(super) struct Header<'a> {
    pub(super) run_name: &'a str,
    pub(super) command_line: &'a str,
    pub(super) project: &'a str,
    pub(super) start: i64,
    pub(super) end: i64,
}

/// The page, from the session node and its workflow node.
pub(super) fn render(
    header: &Header,
    session: &Node,
    tasks: &[&Metrics],
    options: &[(&str, String)],
    intensities: &[(i64, f64)],
) -> String {
    let total = &session.metrics;
    let processes: Vec<&Node> = (session.children.iter())
        .filter(|node| node.level == "workflow")
        .flat_map(|workflow| workflow.children.iter())
        .collect();
    let mut summary = Json::Object(Vec::new());
    for process in &processes {
        let values = |value: fn(&Metrics) -> f64| {
            Json::list(process.children.iter().map(|task| value(&task.metrics)))
        };
        summary.insert(
            &process.name,
            Json::object([
                ("CO2e", values(|task| task.co2)),
                ("energy_consumption", values(|task| task.energy * 1000.0)),
                ("CO2e_non_cached", values(|task| task.co2_fresh)),
                (
                    "energy_consumption_non_cached",
                    values(|task| task.energy_fresh * 1000.0),
                ),
                ("memory", values(|task| task.memory)),
                (
                    "peak_rss",
                    values(|task| task.record["peak_rss"].as_f64().unwrap_or_default()),
                ),
            ]),
        );
    }
    let data = Json::object([
        ("trace", Json::list(tasks.iter().map(|task| row(task)))),
        ("summary", summary),
    ]);
    let option_list = Json::list(options.iter().map(|(name, value)| {
        let value = (value != "null").then(|| Json::from(value.as_str()));
        Json::object([("option", Json::from(*name)), ("value", value.into())])
    }));
    let records = Json::object(
        (intensities.iter())
            .map(|(time, value)| (crate::report_format::instant(*time), Json::from(*value))),
    );
    let mut page = String::new();
    let title = format!(
        "[{}] Nextflow CO\u{2082}e Footprint Report",
        escape(header.run_name)
    );
    let _ = write!(
        page,
        "<!DOCTYPE html>\n<html lang=\"en\">\n<head>\n<meta charset=\"utf-8\">\n<meta \
         name=\"viewport\" content=\"width=device-width, initial-scale=1\">\n<title>{title}</title>\n\
         <style>body{{font-family:sans-serif;margin:2rem;color:#222}}table{{border-collapse:\
         collapse;margin:1rem 0}}td,th{{border:1px solid #ccc;padding:.3rem .6rem;text-align:\
         left}}th{{background:#f3f3f3}}</style>\n</head>\n<body>\n<h1>{title}</h1>\n\
         <h2>Workflow</h2>\n<table>\n<tr><th>Command</th><td><code>{}</code></td></tr>\n\
         <tr><th>Started</th><td>{}</td></tr>\n<tr><th>Completed</th><td>{}</td></tr>\n\
         <tr><th>Project</th><td>{}</td></tr>\n</table>\n",
        escape(header.command_line),
        offset_time(header.start),
        offset_time(header.end),
        escape(header.project),
    );
    let car = scientific(total.co2 / 175.0);
    let flight = scientific(total.co2 * 100.0 / 50_000.0);
    let tree = duration(total.co2 / 917.0 * 4.35 * 7.0 * 86_400_000.0, 2);
    let market = total
        .co2_market
        .map_or("-".to_owned(), |market| quantity(market, "g"));
    let _ = write!(
        page,
        "<h2>CO\u{2082}e Footprint Metrics</h2>\n<table>\n<tr><th>CO\u{2082}e emissions</th>\
         <td>{}</td></tr>\n<tr><th>Energy consumption</th><td>{}</td></tr>\n<tr><th>CO\u{2082}e \
         emissions of tasks that were not cached</th><td>{}</td></tr>\n<tr><th>Energy of tasks \
         that were not cached</th><td>{}</td></tr>\n<tr><th>CO\u{2082}e emissions (market)</th>\
         <td>{market}</td></tr>\n<tr><th>Car</th><td>{car} km</td></tr>\n<tr><th>Tree</th><td>\
         {tree}</td></tr>\n<tr><th>Flight from Paris to London</th><td>{flight} %</td></tr>\n\
         </table>\n<h2>Per process emissions</h2>\n<table>\n<tr><th>Process</th><th>Tasks</th>\
         <th>CO\u{2082}e</th><th>Energy</th></tr>\n",
        quantity(total.co2, "g"),
        quantity(total.energy * 1000.0, "Wh"),
        quantity(total.co2_fresh, "g"),
        quantity(total.energy_fresh * 1000.0, "Wh"),
    );
    for process in &processes {
        let _ = writeln!(
            page,
            "<tr><td>{}</td><td>{}</td><td>{}</td><td>{}</td></tr>",
            escape(&process.name),
            process.children.len(),
            quantity(process.metrics.co2, "g"),
            quantity(process.metrics.energy * 1000.0, "Wh"),
        );
    }
    page.push_str(
        "</table>\n<h2>Tasks</h2>\n<table>\n<tr><th>Task</th><th>Status</th><th>Name</th>\
         <th>Energy</th><th>CO\u{2082}e</th><th>Carbon intensity</th><th>%CPU</th><th>Memory</th>\
         <th>Run time</th><th>CPUs</th><th>CPU power</th></tr>\n",
    );
    for task in tasks {
        let _ = writeln!(
            page,
            "<tr><td>{}</td><td>{}</td><td>{}</td><td>{}</td><td>{}</td><td>{}</td><td>{}</td>\
             <td>{}</td><td>{}</td><td>{}</td><td>{}</td></tr>",
            task.task_id.unwrap_or_default(),
            escape(task.status.as_deref().unwrap_or("-")),
            escape(task.name.as_deref().unwrap_or("-")),
            quantity(task.energy * 1000.0, "Wh"),
            quantity(task.co2, "g"),
            quantity(task.intensity, INTENSITY),
            percent(task.cpu),
            quantity(task.memory, "B"),
            duration(task.realtime, usize::MAX),
            plain(task.cpus),
            quantity(task.powerdraw_cpu, "W"),
        );
    }
    page.push_str("</table>\n<h2>Options</h2>\n<table>\n");
    for (name, value) in options {
        let _ = writeln!(page, "<tr><th>{name}</th><td>{}</td></tr>", escape(value));
    }
    let script = |name: &str, value: &Json| {
        format!(
            "window.{name} = {};\n",
            value.compact().replace("</", "<\\/")
        )
    };
    let _ = write!(
        page,
        "</table>\n<h2>Data Attribution &amp; References</h2>\n<p>The estimate follows the Green \
         Algorithms method: Lannelongue, L., Grealey, J., Inouye, M., Green Algorithms: \
         Quantifying the Carbon Footprint of Computation. Adv. Sci. 2021, 2100707. \
         <a href=\"https://doi.org/10.1002/advs.202100707\">doi:10.1002/advs.202100707</a>. \
         Live carbon intensity, when asked for, comes from Electricity Maps.</p>\n<script>\n{}{}{}\
         </script>\n</body>\n</html>\n",
        script("data", &data),
        script("options", &option_list),
        script("ciRecords", &records),
    );
    page
}

/// A task of the page's data: each field with its raw value and its text.
fn row(task: &Metrics) -> Json {
    let field = |value: Json, kind: &str, readable: String| {
        Json::object([
            (
                "raw",
                Json::object([("value", value), ("type", kind.into())]),
            ),
            ("readable", readable.into()),
        ])
    };
    let mut row = Json::object([
        (
            "task_id",
            field(
                task.task_id.into(),
                "Integer",
                task.task_id.unwrap_or_default().to_string(),
            ),
        ),
        (
            "status",
            field(
                task.status.clone().into(),
                "String",
                task.status.clone().unwrap_or_default(),
            ),
        ),
        (
            "name",
            field(
                task.name.clone().into(),
                "String",
                task.name.clone().unwrap_or_default(),
            ),
        ),
        (
            "energy_consumption",
            field(
                (task.energy * 1000.0).into(),
                "Number",
                quantity(task.energy * 1000.0, "Wh"),
            ),
        ),
        (
            "CO2e",
            field(task.co2.into(), "Number", quantity(task.co2, "g")),
        ),
        (
            "carbon_intensity",
            field(
                task.intensity.into(),
                "Number",
                quantity(task.intensity, INTENSITY),
            ),
        ),
        ("%cpu", field(task.cpu.into(), "Number", percent(task.cpu))),
        (
            "memory",
            field(task.memory.into(), "Number", quantity(task.memory, "B")),
        ),
        (
            "realtime",
            field(
                task.realtime.into(),
                "Duration",
                duration(task.realtime, usize::MAX),
            ),
        ),
        ("cpus", field(task.cpus.into(), "Number", plain(task.cpus))),
        (
            "powerdraw_cpu",
            field(
                task.powerdraw_cpu.into(),
                "Number",
                quantity(task.powerdraw_cpu, "W"),
            ),
        ),
        (
            "cpu_model",
            field(
                task.cpu_model.clone().into(),
                "String",
                task.cpu_model.clone().unwrap_or_default(),
            ),
        ),
    ]);
    for (key, value) in &task.record {
        if row.get(key).is_none() && !value.is_null() {
            let readable = value
                .as_str()
                .map_or_else(|| value.to_string(), str::to_owned);
            row.insert(key, field(Json::from(value), "String", readable));
        }
    }
    row
}

fn escape(text: &str) -> String {
    text.replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('"', "&quot;")
}
