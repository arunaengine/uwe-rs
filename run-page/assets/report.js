// The report of one run, a single HTML file that `iunctus report` and `iunctus run -report`
// write: the page of the run without a server. The data that the page would ask the server for
// is in the file; this script answers the requests of the other scripts from it, and adds the
// parts that only the report has: the timeline of the tasks, what each task used, and where the
// results went. It uses the helpers of `page.js` and the view of `run.js`.
// Copyright (c) 2026 The uwe Contributors
// SPDX-License-Identifier: MIT or Apache-2.0
"use strict";

const REPORT_DATA = JSON.parse(document.getElementById("report").textContent);
// How many tasks the timeline draws, and how many rows the table of resources shows at first.
const TIMELINE_ROWS = 1000;
const RESOURCE_ROWS = 300;

// ---- the answers of the server, from the file ----

// The scripts of the page ask the server with `fetch`; the report answers from its data, and
// never goes to the network.
window.fetch = async (address) => {
  const path = decodeURIComponent(String(address));
  if (Object.hasOwn(REPORT_DATA.answers, path)) {
    return new Response(JSON.stringify(REPORT_DATA.answers[path]),
      { headers: { "content-type": "application/json" } });
  }
  if (Object.hasOwn(REPORT_DATA.pictures, path)) {
    return new Response(REPORT_DATA.pictures[path], { headers: { "content-type": "image/svg+xml" } });
  }
  return new Response(`This report holds only run ${REPORT_DATA.run}, and not this view of it. ` +
    `\`iunctus page ${REPORT_DATA.run}\` shows everything.`, { status: 404 });
};

// The view that the report shows first.
function reportStart() {
  return `/runs/${encodeURIComponent(REPORT_DATA.run)}`;
}

function reportTask(id) {
  return REPORT_DATA.answers[`/api/runs/${REPORT_DATA.run}/tasks/${id}`] || null;
}

document.getElementById("written").textContent =
  `report of ${when(REPORT_DATA.written)}, ${REPORT_DATA.program}`;

// The run view of the page, with the parts of the report after the time it took.
const showPageRun = showRun;
showRun = async function (id, task) {
  await showPageRun(id, task);
  if (!view || view.id !== id) return;
  view.parts.time.after(timelinePart(), resourcesPart(), resultsPart());
};

// ---- the timeline ----

// One bar per task, as Nextflow's timeline draws them: light while it waited to start, then in
// the colour of its state while it ran. A task taken from the cache is a thin mark.
function timelinePart() {
  const run = view.run;
  const tasks = [...view.tasks.values()].filter((task) => task.started)
    .sort((a, b) => a.started - b.started || a.id - b.id);
  const card = el("section", { class: "card" }, el("h2", {}, "Timeline"));
  if (!tasks.length) {
    card.append(el("p", { class: "quiet" }, "No task started."));
    return card;
  }
  const start = Math.min(run.started, ...tasks.map((task) => task.requested));
  const end = Math.max(run.ended || 0, ...tasks.map((task) => task.ended || task.started));
  const length = Math.max(1, end - start);
  const at = (time) => `${(100 * (time - start)) / length}%`;
  const width = (from, to) => `max(2px, ${(100 * Math.max(0, to - from)) / length}%)`;
  const ticks = [0, 0.25, 0.5, 0.75, 1].map((share) =>
    el("span", { style: `left: ${100 * share}%` }, duration(Math.round(share * length))));
  const rows = tasks.slice(0, TIMELINE_ROWS).map((task) => {
    const state = shownState(task);
    const ended = task.ended || end;
    const said = `${task.name}: ${LABELS[state]}, waited ${duration(task.started - task.requested)}, ` +
      `ran ${duration(ended - task.started)}`;
    const bars = [];
    if (task.started > task.requested) {
      bars.push(el("i", { class: "waited", style: `left: ${at(task.requested)}; width: ${width(task.requested, task.started)}` }));
    }
    bars.push(el("i", { class: fill(state), style: `left: ${at(task.started)}; width: ${width(task.started, ended)}` }));
    return el("div", { class: "lane pick", title: said, onclick: () => go(taskPath(task.id)) },
      el("span", { class: "lane-name" }, task.name),
      el("span", { class: "lane-bars" }, bars),
      el("span", { class: "lane-time" }, task.reused ? "cached" : duration(ended - task.started)));
  });
  card.append(
    el("div", { class: "legend" }, el("span", {}, el("i", { class: "waited" }), "waited to start"),
      ["succeeded", "reused", "failed", "stopped"].map((state) =>
        el("span", {}, el("i", { class: fill(state) }), label(state)))),
    el("div", { class: "timeline" },
      el("div", { class: "lane" }, el("span", {}), el("span", { class: "lane-ticks" }, ticks), el("span", {})),
      rows));
  if (tasks.length > TIMELINE_ROWS) {
    card.append(el("p", { class: "quiet" }, `The first ${TIMELINE_ROWS} of ${tasks.length} tasks.`));
  }
  return card;
}

// ---- the resources ----

// The orders of the table of resources, and the value each sorts by.
const RESOURCE_ORDERS = [
  ["asked", "in the order asked", null],
  ["time", "longest first", (row) => row.time],
  ["cpu", "most CPU time first", (row) => row.cpu],
  ["memory", "most memory first", (row) => row.memory],
  ["io", "most I/O first", (row) => (row.read ?? 0) + (row.written ?? 0)],
];

// What each task used, as the executor measured it in its last attempt: the time, the CPU time
// and the share of one CPU, the peak of memory, and the bytes read and written.
function resourcesPart() {
  const rows = [...view.tasks.values()].map((task) => {
    const detail = reportTask(task.id);
    const last = detail && detail.attempts.length ? detail.attempts[detail.attempts.length - 1] : null;
    const time = task.started && task.ended && !task.reused ? task.ended - task.started : null;
    const cpu = last && last.cpu_usec !== null ? last.cpu_usec / 1000 : null;
    return { task, time, cpu, memory: last ? last.memory_peak : null,
      read: last ? last.read_bytes : null, written: last ? last.write_bytes : null,
      exit: task.exit_code };
  });
  const card = el("section", { class: "card" }, el("h2", {}, "Resources"));
  if (!rows.some((row) => row.cpu !== null || row.memory !== null)) {
    card.append(el("p", { class: "quiet" }, view.run.simulated
      ? "A simulated run uses no resources."
      : "The executor measured no resources of these tasks."));
  }
  let order = "asked";
  let shown = RESOURCE_ROWS;
  const table = el("div", {});
  const draw = () => {
    const sort = RESOURCE_ORDERS.find(([name]) => name === order)[2];
    const sorted = sort ? [...rows].sort((a, b) => (sort(b) ?? -1) - (sort(a) ?? -1)) : rows;
    const share = (row) => (row.cpu !== null && row.time ? `${Math.round((100 * row.cpu) / row.time)}%` : "");
    const more = sorted.length > shown
      ? el("button", { onclick: () => { shown += RESOURCE_ROWS; draw(); } },
        `show more (${sorted.length - shown} left)`)
      : null;
    table.replaceChildren(el("table", {},
      el("thead", {}, el("tr", {}, el("th", { class: "number" }, "#"), el("th", {}, "Task"),
        el("th", {}, "State"), el("th", { class: "number" }, "Exit"),
        el("th", { class: "number" }, "Time"), el("th", { class: "number" }, "CPU time"),
        el("th", { class: "number" }, "CPU"), el("th", { class: "number" }, "Memory peak"),
        el("th", { class: "number" }, "Read"), el("th", { class: "number" }, "Written"))),
      el("tbody", {}, sorted.slice(0, shown).map((row) =>
        el("tr", { class: "pick", onclick: () => go(taskPath(row.task.id)) },
          el("td", { class: "number" }, row.task.id), el("td", {}, row.task.name),
          el("td", {}, badge(shownState(row.task))), el("td", { class: "number" }, row.exit ?? ""),
          el("td", { class: "number" }, duration(row.time)),
          el("td", { class: "number" }, row.cpu === null ? "" : duration(Math.round(row.cpu))),
          el("td", { class: "number" }, share(row)), el("td", { class: "number" }, bytes(row.memory)),
          el("td", { class: "number" }, bytes(row.read)), el("td", { class: "number" }, bytes(row.written)))))),
    ...(more ? [more] : []));
  };
  card.append(el("div", { class: "tools" }, el("select", { onchange: (event) => {
    order = event.target.value;
    shown = RESOURCE_ROWS;
    draw();
  } }, RESOURCE_ORDERS.map(([name, text]) => el("option", { value: name }, text)))), table);
  draw();
  return card;
}

// ---- the results ----

// Where the results of the run went, and where the files of every task are.
function resultsPart() {
  const card = el("section", { class: "card" }, el("h2", {}, "Results"));
  const delivered = REPORT_DATA.deliveries;
  if (delivered.length) {
    card.append(el("table", {},
      el("thead", {}, el("tr", {}, el("th", {}, "Where it went"), el("th", {}, "Made by"))),
      el("tbody", {}, delivered.map((result) => el("tr", {},
        el("td", { class: "mono" }, result.target),
        el("td", {}, result.task === null ? "" : link(taskPath(result.task),
          (view.tasks.get(result.task) || { name: `task ${result.task}` }).name)))))));
  } else {
    card.append(el("p", { class: "quiet" }, "The run sent no results to a place of their own."));
  }
  if (view.run.store) {
    card.append(el("p", {}, "The files of every task are in the store ",
      el("span", { class: "mono" }, view.run.store), "; the panel of a task names them."));
  }
  return card;
}
