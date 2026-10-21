// Where the time of a run went, on the page of iunctus: the numbers of the run, the critical
// path (the chain of tasks that set the length of the run) as one bar and as a table, and the
// steps that ran the longest or used the most CPU time, memory and I/O. A switch shows the path
// in the graph view. It uses the helpers of `page.js`, the view of `run.js` and the graph of
// `graph.js`.
// Copyright (c) 2026 The uwe Contributors
// SPDX-License-Identifier: MIT or Apache-2.0
"use strict";

// The kinds of time on the path, in the fixed order of their colours.
const PARTS = [
  { key: "ran", label: "ran" },
  { key: "waited", label: "waited to start" },
  { key: "between", label: "loading, planning and storing" },
];
// The orders of the table of steps, and the value each sorts by.
const STEP_ORDERS = [
  ["ran", "ran the longest", (step) => step.ran],
  ["cpu", "used the most CPU time", (step) => step.cpu_usec],
  ["memory", "used the most memory", (step) => step.memory_peak],
  ["io", "read and wrote the most", (step) =>
    step.read_bytes === null && step.write_bytes === null
      ? null : (step.read_bytes || 0) + (step.write_bytes || 0)],
];
// How many steps the table shows until the user asks for all.
const STEP_ROWS = 10;
// While a run goes on, the numbers are loaded again at most this often.
const TIME_PAUSE = 5000;
// A bar of more parts than this has no gaps between them, which would take all its room.
const GAPS_MOST = 80;

// Builds the card and loads the numbers.
function drawTime() {
  view.time = { data: null, order: "ran", all: false, path: false, loaded: 0, timer: null };
  view.parts.time.replaceChildren(el("h2", {}, "Where the time went"),
    el("p", { class: "quiet" }, "Reading..."));
  loadTime();
}

async function loadTime() {
  const time = view.time;
  const id = view.id;
  time.loaded = Date.now();
  let data;
  try {
    data = await json(`/api/runs/${encodeURIComponent(id)}/time`);
  } catch (error) {
    if (view && view.time === time) {
      view.parts.time.replaceChildren(el("h2", {}, "Where the time went"),
        el("p", { class: "error" }, error.message));
    }
    return;
  }
  if (!view || view.id !== id || view.time !== time) return;
  time.data = data;
  renderTime();
  markPath();
}

// A live update came: load the numbers again soon, but not more often than every few seconds.
function timeChanged() {
  const time = view && view.time;
  if (!time || time.timer) return;
  const wait = Math.max(0, TIME_PAUSE - (Date.now() - time.loaded));
  time.timer = setTimeout(() => {
    time.timer = null;
    if (view && view.time === time) loadTime();
  }, wait);
}

function renderTime() {
  const time = view.time;
  const data = time.data;
  const length = data.ended - data.started;
  const tiles = [
    ["Length of the run", duration(length)],
    ["Time of all tasks together", duration(data.ran)],
    ["Tasks at a time, on average", length > 0 ? (data.ran / length).toFixed(1) : "0"],
    ["Time tasks waited to start", duration(data.waited)],
    ["Time no task ran", duration(Math.max(0, length - data.busy))],
  ];
  const parts = [
    el("div", { class: "tools" },
      el("h2", { style: "margin: 0; flex: 1" }, "Where the time went"),
      el("button", { "aria-pressed": String(time.path), onclick: () => showPath(!time.path) },
        "Show the critical path in the graph")),
    el("div", { class: "tiles" }, tiles.map(([label, value]) =>
      el("div", { class: "tile" }, el("div", { class: "tile-label" }, label),
        el("div", { class: "tile-value" }, value)))),
  ];
  if (view.run.simulated) {
    parts.push(el("p", { class: "quiet" },
      "The run was simulated: these are the times of the simulator, not of the tools."));
  }
  parts.push(pathPart(data, length), stepsPart(data.steps));
  view.parts.time.replaceChildren(...parts);
  fitLabels();
}

// Keeps the name of a task in its part of the bar only where the name fits whole.
function fitLabels() {
  for (const label of view.parts.time.querySelectorAll(".timebar .part-label")) {
    if (label.scrollWidth > label.parentNode.clientWidth) label.remove();
  }
}

// ---- the critical path ----

// The parts of the path in order: before the first task, then per task the time between it and
// the task before, its wait and its run, then after the last task.
function pathSegments(data) {
  const segments = [{ key: "between", time: data.before, what: "before the first task" }];
  for (const task of data.critical_path) {
    segments.push({ key: "between", time: task.gap, what: "between the task before and this one", task });
    segments.push({ key: "waited", time: task.waited, what: "waited to start", task });
    segments.push({ key: "ran", time: task.ran, what: task.reused ? "came from elsewhere" : "ran", task });
  }
  if (data.critical_path.length) {
    segments.push({ key: "between", time: data.after, what: "after the last task" });
  }
  return segments.filter((segment) => segment.time > 0);
}

function pathPart(data, length) {
  const segments = pathSegments(data);
  const totals = Object.fromEntries(PARTS.map((part) => [part.key, 0]));
  for (const segment of segments) totals[segment.key] += segment.time;
  const tip = el("div", { class: "graph-tip", hidden: true });
  const bar = el("div", {
    class: segments.length > GAPS_MOST ? "timebar dense" : "timebar",
    role: "img",
    "aria-label": `The critical path: ${PARTS.map((part) =>
      `${duration(totals[part.key])} ${part.label}`).join(", ")}. The table below lists its parts.`,
  }, segments.map((segment) => {
    // The name of a task that ran labels its part, where it fits (see `fitLabels`).
    const label = segment.key === "ran" && segment.task && !segment.task.reused
      ? el("span", { class: "part-label" }, segment.task.name) : null;
    const part = el("span", {
      class: `part ${segment.key}`,
      style: `flex: ${segment.time} 1 0px`,
      onpointermove: (event) => {
        const who = segment.task ? [el("div", {}, segment.task.name), el("div", { class: "quiet mono" }, segment.task.step)] : [];
        tip.replaceChildren(el("div", {}, el("strong", {}, duration(segment.time)), ` ${segment.what}`), ...who);
        tip.hidden = false;
        tip.style.left = `${Math.min(event.clientX + 14, window.innerWidth - tip.offsetWidth - 8)}px`;
        tip.style.top = `${event.clientY + 16}px`;
      },
      onpointerleave: () => { tip.hidden = true; },
    }, label);
    if (segment.task) part.addEventListener("click", () => go(taskPath(segment.task.task)));
    return part;
  }));
  const legend = el("div", { class: "legend" }, PARTS.map((part) =>
    el("span", {}, el("i", { class: `part ${part.key}` }), `${part.label} ${duration(totals[part.key])}`)));
  const scale = el("div", { class: "timescale quiet" }, el("span", {}, "0"), el("span", {}, duration(length)));
  const rows = [];
  const row = (cells, task) => el("tr", task === undefined ? {} : { class: "pick", onclick: () => go(taskPath(task)) }, cells);
  rows.push(row([el("td", {}), el("td", { class: "quiet" }, "before the first task"), el("td", {}),
    el("td", { class: "number" }, duration(data.before)), el("td", {}), el("td", {}), el("td", {})]));
  for (const task of data.critical_path) {
    const ran = task.reused ? `from ${task.reused === "cache" ? "the cache" : `run ${task.reused}`}` : duration(task.ran);
    rows.push(row([el("td", { class: "number" }, task.task), el("td", {}, task.name),
      el("td", { class: "mono" }, task.step), el("td", { class: "number" }, duration(task.gap)),
      el("td", { class: "number" }, duration(task.waited)), el("td", { class: "number" }, ran),
      el("td", { class: "number" }, task.attempts > 1 ? task.attempts : "")], task.task));
  }
  if (data.critical_path.length) {
    rows.push(row([el("td", {}), el("td", { class: "quiet" }, "after the last task"), el("td", {}),
      el("td", { class: "number" }, duration(data.after)), el("td", {}), el("td", {}), el("td", {})]));
  }
  const table = el("table", {},
    el("thead", {}, el("tr", {}, el("th", { class: "number" }, "#"), el("th", {}, "Task"),
      el("th", {}, "Step"), el("th", { class: "number" }, "Before it"),
      el("th", { class: "number" }, "Waited"), el("th", { class: "number" }, "Ran"),
      el("th", { class: "number" }, "Attempts"))),
    el("tbody", {}, rows));
  const count = data.critical_path.length;
  return el("div", { class: "timepath" },
    el("h3", {}, "The critical path"),
    el("p", { class: "quiet" }, count
      ? `The chain of ${count === 1 ? "1 task" : `${count} tasks`} that set the length of the run: ` +
        "each waited for the one before it. Click a part to open its task."
      : "No task ran, so the run spent its time before any task."),
    count ? bar : null, count ? scale : null, count ? legend : null, tip,
    count ? el("details", {}, el("summary", {}, "the parts as a table"), table) : null);
}

// ---- steps ----

function stepsPart(steps) {
  const time = view.time;
  const [, , value] = STEP_ORDERS.find(([key]) => key === time.order);
  const sorted = steps.filter((step) => value(step) !== null && value(step) > 0)
    .sort((a, b) => value(b) - value(a));
  const most = sorted.length ? value(sorted[0]) : 0;
  const shown = time.all ? sorted : sorted.slice(0, STEP_ROWS);
  const measured = steps.some((step) => step.cpu_usec !== null || step.memory_peak !== null);
  const order = el("select", { onchange: (event) => {
    time.order = event.target.value;
    renderTime();
  } }, STEP_ORDERS.map(([key, label]) => el("option", { value: key, selected: key === time.order }, label)));
  const cell = (key, text, amount) => {
    if (key !== time.order) return el("td", { class: "number" }, text);
    const share = most > 0 ? (100 * amount) / most : 0;
    return el("td", { class: "number sorted" }, text,
      el("div", { class: "databar" }, el("span", { style: `width: ${share}%` })));
  };
  const amount = (value, show) => (value === null || value === undefined ? "" : show(value));
  const rows = shown.map((step) => el("tr", { class: "pick", onclick: () => chooseStep(step.step) },
    el("td", { class: "mono" }, step.step),
    el("td", { class: "number" }, step.tasks),
    cell("ran", duration(step.ran), step.ran),
    el("td", { class: "number" }, duration(step.waited)),
    cell("cpu", amount(step.cpu_usec, (usec) => duration(Math.round(usec / 1000))), step.cpu_usec),
    cell("memory", amount(step.memory_peak, bytes), step.memory_peak),
    cell("io", amount(step.read_bytes, bytes), (step.read_bytes || 0) + (step.write_bytes || 0)),
    el("td", { class: "number" }, amount(step.write_bytes, bytes))));
  const parts = [el("div", { class: "tools" }, el("h3", { style: "margin: 0" }, "Steps that"), order)];
  if (!sorted.length) {
    parts.push(el("p", { class: "quiet" }, measured || time.order === "ran"
      ? "No step has a value to show here."
      : "No task has a measure of CPU time, memory or I/O: the local executor with Docker or " +
        "Podman measures them."));
    return el("div", {}, parts);
  }
  parts.push(el("table", {},
    el("thead", {}, el("tr", {}, el("th", {}, "Step"), el("th", { class: "number" }, "Tasks"),
      el("th", { class: "number" }, "Ran, together"), el("th", { class: "number" }, "Waited"),
      el("th", { class: "number" }, "CPU time"), el("th", { class: "number" }, "Peak memory"),
      el("th", { class: "number" }, "Read"), el("th", { class: "number" }, "Written"))),
    el("tbody", {}, rows)));
  if (sorted.length > STEP_ROWS) {
    parts.push(el("button", { onclick: () => { time.all = !time.all; renderTime(); } },
      time.all ? "show fewer" : `show all ${sorted.length}`));
  }
  return el("div", {}, parts);
}

// ---- the path in the graph ----

function showPath(on) {
  if (!view || !view.time) return;
  view.time.path = on;
  if (view.time.data) renderTime();
  graphTools();
  markPath();
}

// Marks the boxes of the path in the graph, or the boxes of their steps where those are folded,
// and the lines between them; the rest of the graph steps back.
function markPath() {
  const graph = view && view.graph;
  if (!graph || !graph.content) return;
  const content = graph.content;
  for (const found of content.querySelectorAll(".on-path")) found.classList.remove("on-path");
  const time = view.time;
  const on = Boolean(time && time.path && time.data);
  if (graph.stage.classList.contains("showing-path") !== on) {
    graph.stage.classList.toggle("showing-path", on);
    drawGraphLegend();
  }
  if (!on) return;
  const boxes = [];
  for (const task of time.data.critical_path) {
    const node = document.getElementById(`t${task.task}`) || graph.steps.get(task.step);
    if (!node) continue;
    node.classList.add("on-path");
    if (boxes[boxes.length - 1] !== node.id) boxes.push(node.id);
  }
  for (let index = 1; index < boxes.length; index += 1) {
    const from = CSS.escape(boxes[index - 1]);
    const to = CSS.escape(boxes[index]);
    for (const edge of content.querySelectorAll(`.edge[data-from="${from}"][data-to="${to}"]`)) {
      edge.classList.add("on-path");
    }
  }
}
