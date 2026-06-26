// The view of one run on the page of iunctus: its summary, graph, steps, tasks, and one task
// with its logs and files. A running run updates live through Server-Sent Events. It uses the
// helpers of `page.js`, which the page loads first.
// Copyright (c) 2026 The iunctus Contributors
// SPDX-License-Identifier: MIT or Apache-2.0
"use strict";

// ---- one run ----

async function showRun(id, task) {
  leave();
  trail.textContent = `runs / ${id}`;
  let data;
  try {
    data = await json(`/api/runs/${encodeURIComponent(id)}`);
  } catch (error) {
    main.replaceChildren(el("p", { class: "error" }, error.message));
    return;
  }
  view = {
    id,
    run: data.run,
    tasks: new Map(data.tasks.map((row) => [row.id, row])),
    markers: data.markers,
    sequence: data.sequence,
    filter: { text: "", state: "", step: "" },
    shown: ROWS,
    chosen: null,
    events: null,
    graphView: data.tasks.length > LARGE ? "steps" : "tasks",
    graphTimer: null,
    graphDrawn: 0,
  };
  document.title = `iunctus: ${base(data.run.workflow)} ${id}`;
  buildRun();
  drawAll();
  drawGraph();
  choose(task);
  listen();
}

// The frame of the run view: each part is filled by its own draw function.
function buildRun() {
  view.parts = {
    summary: el("section", { class: "card" }),
    progress: el("section", { class: "card" }),
    graph: el("section", { class: "card" }),
    steps: el("section", { class: "card" }),
    tasks: el("section", { class: "card" }),
    detail: el("aside", { class: "card side", hidden: true }),
  };
  const parts = view.parts;
  view.columns = el("div", { class: "columns" },
    el("div", {}, parts.progress, parts.graph, parts.steps, parts.tasks),
    parts.detail);
  main.replaceChildren(parts.summary, view.columns);
}

function drawAll() {
  // The summary changes only with the state of the run; drawing it again would close its
  // open sections.
  const shown = [view.run.state, view.run.ended, view.run.error].join(" ");
  if (view.summaryShown !== shown) {
    view.summaryShown = shown;
    drawSummary();
  }
  drawProgress();
  drawSteps();
  drawTasks();
}

function drawSummary() {
  const run = view.run;
  const ended = run.ended || (run.state === "running" ? Date.now() : null);
  const facts = [
    ["Run", run.id],
    ["Language", run.language],
    ["Executor", run.executor],
    ["Started", when(run.started)],
    ["Ended", run.ended ? when(run.ended) : ""],
    ["Time", ended ? duration(ended - run.started) : ""],
    ["Revision", run.revision || ""],
    ["Store", run.store || ""],
    ["Resumes", run.resumes || ""],
  ].filter(([, value]) => value !== "");
  const parts = [
    el("h1", {}, base(run.workflow), " ", badge(run.state)),
    el("div", { class: "quiet mono" }, run.workflow),
  ];
  if (run.simulated) {
    parts.push(el("div", { class: "notice" },
      el("strong", {}, "Simulated run. "),
      "No task ran: the simulator made up the output files, so files, logs and times are not " +
      "results of the tools. Points where real data can add or remove tasks are marked."));
  }
  if (run.error) parts.push(el("pre", { class: "error" }, run.error));
  parts.push(el("dl", { class: "facts" },
    facts.map(([name, value]) => el("div", {}, el("dt", {}, name), el("dd", {}, value)))));
  if (run.parameters.length) {
    parts.push(el("details", {},
      el("summary", {}, `${run.parameters.length} parameters`),
      el("table", {}, el("tbody", {}, run.parameters.map(([name, value]) =>
        el("tr", {}, el("td", { class: "mono" }, name), el("td", { class: "mono" }, value)))))));
  }
  parts.push(el("details", {},
    el("summary", {}, "command line"),
    el("pre", {}, ["iunctus", ...run.arguments].join(" "))));
  view.parts.summary.replaceChildren(...parts);
}

// The number of tasks in each shown state.
function counts(tasks) {
  const found = Object.fromEntries(STATES.map((state) => [state, 0]));
  for (const task of tasks) found[shownState(task)] = (found[shownState(task)] || 0) + 1;
  return found;
}

function drawProgress() {
  const tasks = [...view.tasks.values()];
  const found = counts(tasks);
  const total = tasks.length || 1;
  const bar = el("div", { class: "progress", role: "img",
    "aria-label": STATES.map((state) => `${found[state]} ${state}`).join(", ") },
    STATES.filter((state) => found[state]).map((state) =>
      el("span", { class: `${state}-fill`, style: `width: ${(100 * found[state]) / total}%` })));
  const legend = el("div", { class: "legend" },
    STATES.filter((state) => found[state]).map((state) =>
      el("span", {}, el("i", { class: `${state}-fill` }), `${found[state]} ${LABELS[state]}`)));
  view.parts.progress.replaceChildren(el("h2", {}, `${tasks.length} tasks`), bar, legend);
}

// ---- graph ----

function drawGraph() {
  if (!view) return;
  view.graphTimer = null;
  view.graphDrawn = Date.now();
  const graphView = view.graphView;
  const toggle = ["steps", "tasks"].map((name) =>
    el("button", { "aria-pressed": String(name === graphView), onclick: () => {
      view.graphView = name;
      drawGraph();
    } }, name === "steps" ? "Steps" : "Tasks"));
  const box = el("div", { class: "graph" }, el("p", { class: "quiet" }, "Drawing..."));
  view.parts.graph.replaceChildren(
    el("div", { class: "tools" }, el("h2", { style: "margin: 0; flex: 1" }, "Graph"), toggle),
    box);
  const id = view.id;
  fetch(`/api/runs/${encodeURIComponent(id)}/graph.svg?view=${graphView}`)
    .then(async (response) => {
      if (!view || view.id !== id) return;
      if (!response.ok) {
        box.replaceChildren(el("p", { class: "quiet" }, "The graph cannot be drawn: ",
          (await response.text()) || response.statusText));
        return;
      }
      const svg = new DOMParser().parseFromString(await response.text(), "image/svg+xml");
      const root = svg.documentElement;
      if (root.nodeName !== "svg") {
        box.replaceChildren(el("p", { class: "quiet" }, "The graph cannot be drawn."));
        return;
      }
      box.replaceChildren(document.importNode(root, true));
    })
    .catch((error) => box.replaceChildren(el("p", { class: "error" }, error.message)));
}

// Draws the graph again soon, but not more often than every few seconds.
function graphChanged() {
  if (!view || view.graphTimer) return;
  const wait = Math.max(0, GRAPH_PAUSE - (Date.now() - view.graphDrawn));
  view.graphTimer = setTimeout(drawGraph, wait);
}

// ---- steps ----

function drawSteps() {
  const steps = new Map();
  for (const task of view.tasks.values()) {
    let step = steps.get(task.step);
    if (!step) {
      step = { name: task.step, tasks: [], time: 0 };
      steps.set(task.step, step);
    }
    step.tasks.push(task);
    if (task.started && task.ended) step.time += task.ended - task.started;
  }
  const rows = [...steps.values()].map((step) => {
    const found = counts(step.tasks);
    const chosen = view.filter.step === step.name;
    return el("tr", { class: chosen ? "pick chosen" : "pick", onclick: () => {
      view.filter.step = chosen ? "" : step.name;
      view.shown = ROWS;
      drawSteps();
      drawTasks();
    } },
      el("td", { class: "mono" }, step.name),
      el("td", { class: "number" }, step.tasks.length),
      STATES.map((state) => el("td", { class: "number" }, found[state] || "")),
      el("td", { class: "number" }, duration(step.time)));
  });
  view.parts.steps.replaceChildren(
    el("h2", {}, `${steps.size} steps`),
    el("table", {},
      el("thead", {}, el("tr", {}, el("th", {}, "Step"), el("th", { class: "number" }, "Tasks"),
        STATES.map((state) => el("th", { class: "number" }, LABELS[state])),
        el("th", { class: "number" }, "Task time"))),
      el("tbody", {}, rows)));
}

// ---- tasks ----

function matching() {
  const filter = view.filter;
  const text = filter.text.toLowerCase();
  const found = [];
  for (const task of view.tasks.values()) {
    if (filter.step && task.step !== filter.step) continue;
    if (filter.state && shownState(task) !== filter.state) continue;
    if (text && !task.name.toLowerCase().includes(text) && String(task.id) !== text) continue;
    found.push(task);
  }
  return found;
}

function drawTasks() {
  const part = view.parts.tasks;
  // Keep the search box and its focus while the run updates the table.
  if (!view.taskTools) {
    const search = el("input", { type: "search", placeholder: "Find a task by name or number",
      oninput: (event) => { view.filter.text = event.target.value; view.shown = ROWS; drawRows(); } });
    const state = el("select", { onchange: (event) => {
      view.filter.state = event.target.value;
      view.shown = ROWS;
      drawRows();
    } }, el("option", { value: "" }, "every state"),
      STATES.map((name) => el("option", { value: name }, LABELS[name])));
    view.stepChip = el("span", {});
    view.taskTools = el("div", { class: "tools" }, search, state, view.stepChip);
    view.taskTable = el("div", {});
    part.replaceChildren(el("h2", {}, "Tasks"), view.taskTools, view.taskTable);
  }
  drawRows();
}

function drawRows() {
  const filter = view.filter;
  view.stepChip.replaceChildren(filter.step ? el("span", { class: "chip" }, `step ${filter.step}`,
    el("button", { "aria-label": "every step", onclick: () => {
      filter.step = "";
      drawSteps();
      drawTasks();
    } }, "x")) : "");
  const found = matching();
  const rows = found.slice(0, view.shown).map((task) => {
    const state = shownState(task);
    const time = task.started ? duration((task.ended || Date.now()) - task.started) : "";
    return el("tr", {
      class: task.id === view.chosen ? "pick chosen" : "pick",
      onclick: () => go(taskPath(task.id)),
    },
      el("td", { class: "number" }, task.id),
      el("td", {}, task.name),
      el("td", { class: "mono" }, task.step),
      el("td", {}, badge(state)),
      el("td", { class: "number" }, task.exit_code ?? ""),
      el("td", { class: "number" }, task.attempts > 1 ? task.attempts : ""),
      el("td", { class: "number" }, time));
  });
  const more = found.length > view.shown
    ? el("button", { onclick: () => { view.shown += ROWS; drawRows(); } },
      `show more (${found.length - view.shown} left)`)
    : null;
  const tail = found.length ? more : el("p", { class: "quiet" }, "No task matches.");
  view.taskTable.replaceChildren(
    el("table", {},
      el("thead", {}, el("tr", {}, el("th", { class: "number" }, "#"), el("th", {}, "Task"),
        el("th", {}, "Step"), el("th", {}, "State"), el("th", { class: "number" }, "Exit"),
        el("th", { class: "number" }, "Attempts"), el("th", { class: "number" }, "Time"))),
      el("tbody", {}, rows)),
    ...(tail ? [tail] : []));
}

function taskPath(task) {
  return `/runs/${encodeURIComponent(view.id)}/tasks/${task}`;
}

// ---- one task ----

function choose(task) {
  if (!view) return;
  view.chosen = task;
  drawRows();
  const detail = view.parts.detail;
  if (task === null || task === undefined) {
    detail.hidden = true;
    view.columns.classList.remove("open");
    return;
  }
  detail.hidden = false;
  view.columns.classList.add("open");
  drawDetail(task);
}

async function drawDetail(task) {
  const id = view.id;
  const detail = view.parts.detail;
  let data;
  try {
    data = await json(`/api/runs/${encodeURIComponent(id)}/tasks/${task}`);
  } catch (error) {
    detail.replaceChildren(el("p", { class: "error" }, error.message));
    return;
  }
  if (!view || view.id !== id || view.chosen !== task) return;
  const row = data.row;
  const description = data.description;
  const last = data.attempts[data.attempts.length - 1];
  const markers = view.markers.filter((marker) => marker.task === task);
  const facts = [
    ["Step", row.step],
    ["Image", (last && last.image) || description.image || ""],
    ["Started", when(row.started)],
    ["Time", row.started ? duration((row.ended || Date.now()) - row.started) : ""],
    ["Exit code", row.exit_code ?? ""],
    ["Result", row.reused ? `taken from ${row.reused}` : ""],
    ["Work directory", description.workdir || ""],
  ].filter(([, value]) => value !== "");
  const parts = [
    el("div", { class: "tools" },
      el("h2", { style: "margin: 0; flex: 1" }, row.name, " ", badge(shownState(row))),
      el("button", { onclick: () => go(`/runs/${encodeURIComponent(id)}`) }, "close")),
    el("dl", { class: "facts" },
      facts.map(([name, value]) => el("div", {}, el("dt", {}, name), el("dd", {}, value)))),
  ];
  for (const marker of markers) {
    parts.push(el("p", { class: "marker" }, el("strong", {}, `${marker.kind}: `), marker.reason));
  }
  if (description.command.length) {
    parts.push(el("h3", {}, "Command"), el("pre", {}, description.command.join(" ")));
  }
  if (description.script) parts.push(el("h3", {}, "Script"), el("pre", {}, description.script));
  parts.push(logPart("Standard output", data.stdout, id, task, last, "stdout"));
  parts.push(logPart("Standard error", data.stderr, id, task, last, "stderr"));
  parts.push(el("h3", {}, "Inputs"), filesTable(data.inputs, "inputs", id, task, data.producers));
  parts.push(el("h3", {}, "Outputs"), filesTable(data.outputs, "outputs", id, task, {}));
  const environment = Object.entries(description.environment || {});
  if (environment.length) {
    parts.push(el("details", {}, el("summary", {}, `${environment.length} environment variables`),
      el("table", {}, el("tbody", {}, environment.map(([name, value]) =>
        el("tr", {}, el("td", { class: "mono" }, name), el("td", { class: "mono" }, value)))))));
  }
  if (data.attempts.length > 1) {
    parts.push(el("h3", {}, "Attempts"), el("table", {}, el("tbody", {},
      data.attempts.map((attempt) => el("tr", {},
        el("td", { class: "number" }, attempt.number),
        el("td", {}, when(attempt.started)),
        el("td", { class: "number" }, attempt.ended ? duration(attempt.ended - attempt.started) : ""),
        el("td", { class: "number" }, attempt.exit_code ?? ""),
        el("td", {}, attempt.retried ? "retried after a failure of the machine" : ""))))));
  }
  parts.push(el("p", { class: "quiet mono" }, `fingerprint ${data.fingerprint}`));
  detail.replaceChildren(...parts);
}

function logPart(title, log, id, task, attempt, stream) {
  if (!log) return el("details", {}, el("summary", {}, `${title}: empty`));
  const whole = attempt
    ? el("a", { href: `/api/runs/${encodeURIComponent(id)}/tasks/${task}/logs/${attempt.number}/${stream}`,
      target: "_blank" }, "whole text")
    : null;
  const note = log.whole ? "" : ` (the last ${bytes(log.text.length)} of ${bytes(log.size)})`;
  return el("div", {}, el("h3", {}, title, " ", el("small", { class: "quiet" }, note), " ",
    log.whole ? null : whole), el("pre", {}, log.text));
}

function filesTable(files, role, id, task, producers) {
  if (!files.length) return el("p", { class: "quiet" }, "none");
  const rows = files.map((file, index) => {
    const url = `/api/runs/${encodeURIComponent(id)}/tasks/${task}/${role}/${index}`;
    const producer = producers[file.location];
    let from = "";
    if (producer !== undefined) from = link(taskPath(producer), `task ${producer}`);
    else if (!file.location) from = "written by the workflow";
    else from = el("span", { class: "mono" }, file.location);
    let get = "";
    if (file.location && !file.location.includes("://")) {
      get = file.directory
        ? el("button", { onclick: (event) => listFolder(event.target, url) }, "files")
        : el("a", { href: url }, "download");
    }
    return el("tr", {},
      el("td", { class: "mono" }, file.path, file.directory ? "/" : ""),
      role === "inputs" ? el("td", {}, from) : null,
      el("td", { class: "number" }, bytes(file.size)),
      el("td", { class: "mono", title: file.digest || "" }, shortDigest(file.digest)),
      el("td", {}, get));
  });
  return el("table", {},
    el("thead", {}, el("tr", {}, el("th", {}, "Path"), role === "inputs" ? el("th", {}, "From") : null,
      el("th", { class: "number" }, "Size"), el("th", {}, "SHA-256"), el("th", {}, ""))),
    el("tbody", {}, rows));
}

async function listFolder(button, url) {
  let entries;
  try {
    entries = await json(url);
  } catch (error) {
    button.replaceWith(el("span", { class: "error" }, error.message));
    return;
  }
  button.replaceWith(el("ul", {}, entries.map((entry) => el("li", {},
    el("a", { href: `${url}?path=${encodeURIComponent(entry.path)}` }, entry.path),
    ` ${bytes(entry.size)}`))));
}

// ---- live updates ----

function listen() {
  if (!view || view.run.state !== "running") return;
  const id = view.id;
  const events = new EventSource(`/api/runs/${encodeURIComponent(id)}/events?after=${view.sequence}`);
  view.events = events;
  liveMark.hidden = false;
  events.addEventListener("update", (message) => {
    if (!view || view.id !== id) return;
    const update = JSON.parse(message.data);
    view.sequence = update.sequence;
    view.run = update.run;
    for (const task of update.tasks) view.tasks.set(task.id, task);
    if (update.markers) view.markers = update.markers;
    drawAll();
    if (update.graph) graphChanged();
    if (view.chosen !== null && update.tasks.some((task) => task.id === view.chosen)) {
      drawDetail(view.chosen);
    }
    if (update.run.state !== "running") {
      events.close();
      liveMark.hidden = true;
      graphChanged();
    }
  });
}
