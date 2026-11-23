// The script of the page of uwe: the shared helpers, the navigation with its tabs, and the
// list of runs. `run.js` shows one run, `launch.js` and `composer.js` the studio views.
// Copyright (c) 2026 The uwe Contributors
// SPDX-License-Identifier: MIT or Apache-2.0
"use strict";

// The states of a task as the page shows them; `reused` is a task whose result came from the
// cache or from an earlier run.
const STATES = ["succeeded", "reused", "failed", "running", "waiting", "stopped"];
const LABELS = {
  succeeded: "succeeded", reused: "reused", failed: "failed",
  running: "running", waiting: "waiting", stopped: "stopped",
};
// How many task rows the table shows at first, and adds on each "show more".
const ROWS = 300;
// The graph is laid out again at most this often while tasks come.
const GRAPH_PAUSE = 2000;

// A report is a file of one run, without a server (`report.js`): its views go by the part of
// the address after `#`.
const REPORT = document.getElementById("report") !== null;

const main = document.getElementById("main");
const trail = document.getElementById("trail");
const liveMark = document.getElementById("live");
const tabs = document.getElementById("tabs");

// The run on the page, a view of the studio, or null on the list of runs.
let view = null;
// Counts the views that the page left; a view whose data comes after the next one began is
// dropped, so that it never replaces that view or opens its streams.
let shownViews = 0;
// What the server lets the page do: `{studio, executors, languages}`; a report shows runs only.
let studio = { studio: false, executors: [], languages: [] };
// The words that filter the list of runs, kept while the list refreshes.
let runsFilter = "";
// Settles once `studio` is known; the views of the studio wait for it.
let studioReady = Promise.resolve();

// ---- small helpers ----

// Builds an element. Text always goes in as text, never as HTML.
function el(tag, attributes, ...children) {
  const node = document.createElement(tag);
  for (const [name, value] of Object.entries(attributes || {})) {
    if (value === undefined || value === null || value === false) continue;
    if (name.startsWith("on")) node.addEventListener(name.slice(2), value);
    else node.setAttribute(name, value === true ? "" : value);
  }
  for (const child of children.flat()) {
    if (child === undefined || child === null || child === false) continue;
    node.append(child instanceof Node ? child : String(child));
  }
  return node;
}

// The nodes that are there: `replaceChildren` would show a missing one as the text `null`.
function present(...nodes) {
  return nodes.filter((node) => node !== null && node !== undefined && node !== false);
}

async function json(url) {
  const response = await fetch(url);
  if (!response.ok) throw new Error((await response.text()) || response.statusText);
  return response.json();
}

// Posts a JSON body and reads the JSON answer; a refusal throws its message.
async function send(url, body) {
  const response = await fetch(url, {
    method: "POST",
    headers: { "content-type": "application/json" },
    body: JSON.stringify(body === undefined ? {} : body),
  });
  if (!response.ok) throw new Error((await response.text()) || response.statusText);
  return response.json();
}

function when(milliseconds) {
  return milliseconds ? new Date(milliseconds).toLocaleString() : "";
}

function duration(milliseconds) {
  if (milliseconds === null || milliseconds === undefined || milliseconds < 0) return "";
  if (milliseconds < 1000) return `${milliseconds} ms`;
  const seconds = milliseconds / 1000;
  if (seconds < 60) return `${seconds.toFixed(1)} s`;
  const minutes = Math.floor(seconds / 60);
  if (minutes < 60) return `${minutes} min ${Math.round(seconds % 60)} s`;
  return `${Math.floor(minutes / 60)} h ${minutes % 60} min`;
}

function bytes(size) {
  if (size === null || size === undefined) return "";
  const units = ["B", "KB", "MB", "GB", "TB"];
  let value = size;
  let unit = 0;
  while (value >= 1000 && unit < units.length - 1) { value /= 1000; unit += 1; }
  return unit === 0 ? `${value} B` : `${value.toFixed(1)} ${units[unit]}`;
}

function shortDigest(digest) {
  return digest ? digest.replace("sha256:", "").slice(0, 12) : "";
}

function badge(state, text) {
  return el("span", { class: `state ${state}` }, text || LABELS[state] || state);
}

// The state a task shows: a result taken from elsewhere counts as reused.
function shownState(task) {
  return task.state === "succeeded" && task.reused ? "reused" : task.state;
}

function link(path, text) {
  return el("a", { href: REPORT ? `#${path}` : path, "data-route": true }, text);
}

function base(path) {
  const parts = String(path).split("/").filter(Boolean);
  return parts.length ? parts[parts.length - 1] : String(path);
}

// ---- navigation ----

document.addEventListener("click", (event) => {
  const anchor = event.target.closest("a[data-route]");
  if (!anchor || event.metaKey || event.ctrlKey || event.shiftKey || event.button !== 0) return;
  event.preventDefault();
  go(anchor.getAttribute("href").replace(/^#/, ""));
});
window.addEventListener(REPORT ? "hashchange" : "popstate", route);

function go(path) {
  if (REPORT && location.hash !== `#${path}`) {
    // The change of the address shows the view.
    location.hash = path;
    return;
  }
  if (!REPORT && path !== location.pathname) history.pushState(null, "", path);
  route();
}

// Changes the address to a view that is shown already.
function remember(path) {
  history.pushState(null, "", REPORT ? `#${path}` : path);
}

// The address of the view: its path, or in a report what follows `#`.
function address() {
  return REPORT ? location.hash.slice(1) || reportStart() : location.pathname;
}

function route() {
  const parts = address().split("/").filter(Boolean).map(decodeURIComponent);
  markTab(parts[0] || "");
  if (!REPORT && parts[0] === "launch" && typeof showLaunch === "function") {
    showLaunch();
    return;
  }
  if (!REPORT && parts[0] === "compose" && typeof showComposer === "function") {
    showComposer();
    return;
  }
  if (parts[0] === "runs" && parts[1]) {
    const task = parts[2] === "tasks" && parts[3] ? Number(parts[3]) : null;
    if (view && view.id === parts[1]) {
      choose(task);
    } else {
      showRun(parts[1], task);
    }
  } else if (parts[0] === "compare" && parts[1] && parts[2]) {
    showCompare(parts[1], parts[2]);
  } else {
    showRuns();
  }
}

function leave() {
  shownViews += 1;
  if (view && view.close) view.close();
  if (view && view.stream) view.stream.close();
  if (view && view.events) view.events.close();
  if (view && view.ticker) clearInterval(view.ticker);
  if (view && view.time) clearTimeout(view.time.timer);
  if (view && view.graph) {
    clearTimeout(view.graph.timer);
    view.graph.observer.disconnect();
  }
  view = null;
  liveMark.hidden = true;
}

// ---- the tabs of the studio ----

// Asks what the server lets the page do, and shows the tabs of the launcher and the combiner
// when it starts runs. A report and a page that only shows runs keep the list alone.
async function loadStudio() {
  if (REPORT || !tabs) return;
  try {
    studio = await json("/api/capabilities");
  } catch (_) {
    studio = { studio: false, executors: [], languages: [] };
  }
  const entries = [["", "/", "Runs"]];
  if (studio.studio) entries.push(["launch", "/launch", "Run workflow"], ["compose", "/compose", "Combiner"]);
  tabs.replaceChildren(...entries.map(([name, path, text]) =>
    el("a", { href: path, "data-route": true, "data-tab": name, class: "tab" }, text)));
  tabs.hidden = entries.length < 2;
  markTab(address().split("/").filter(Boolean)[0] || "");
  // The list and the run get their buttons once the page knows that it may start runs.
  if (studio.studio && !view && address() === "/") showRuns();
  if (studio.studio && view && view.run && typeof drawAll === "function") {
    view.summaryShown = null;
    drawAll();
  }
}

function markTab(first) {
  if (!tabs) return;
  const shown = first === "launch" || first === "compose" ? first : "";
  for (const tab of tabs.querySelectorAll("a[data-tab]")) {
    if (tab.dataset.tab === shown) tab.setAttribute("aria-current", "page");
    else tab.removeAttribute("aria-current");
  }
}

// Asks a run to stop, or starts it again; it says what happened next to the button.
async function runAction(button, run, action) {
  button.disabled = true;
  try {
    const answer = await send(`/api/runs/${encodeURIComponent(run)}/${action}`);
    if (action === "repeat" && answer.run_id) {
      go(`/runs/${encodeURIComponent(answer.run_id)}`);
      return;
    }
    button.textContent = action === "cancel" ? "stopping" : "started";
  } catch (error) {
    button.disabled = false;
    button.title = error.message;
    button.textContent = "refused";
  }
}

// ---- the list of runs ----

async function showRuns() {
  leave();
  // The tab names the list once the tabs show; a page without them says it here.
  trail.textContent = tabs && !tabs.hidden ? "" : "runs";
  document.title = "uwe: runs";
  let runs;
  try {
    runs = await json("/api/runs");
  } catch (error) {
    main.replaceChildren(el("p", { class: "error" }, error.message));
    return;
  }
  if (view) return;
  if (!runs.length) {
    main.replaceChildren(el("div", { class: "card" },
      el("h2", {}, "No runs yet"),
      el("p", { class: "quiet" }, "Every `uwe run` records itself here."),
      studio.studio ? el("p", {}, link("/launch", "Run a workflow"), " or ",
        link("/compose", "combine tools into one"), ".") : null));
    return;
  }
  const actions = studio.studio && !REPORT;
  const rows = runs.map((run) => {
    const total = Object.values(run.tasks).reduce((sum, count) => sum + count, 0);
    const row = el("tr", {},
      el("td", { class: "mono" }, link(`/runs/${encodeURIComponent(run.id)}`, run.id)),
      el("td", {}, when(run.started)),
      el("td", {}, badge(run.state)),
      el("td", {}, run.simulated ? "simulated" : run.executor),
      el("td", { class: "number" }, total),
      el("td", { class: "number" }, run.tasks.failed || ""),
      el("td", {}, run.workflow),
      actions ? el("td", {}, run.state === "running"
        ? el("button", { onclick: (event) => runAction(event.target, run.id, "cancel") }, "Cancel")
        : el("button", { onclick: (event) => runAction(event.target, run.id, "repeat") }, "Run again")) : null);
    row.dataset.words = [run.id, run.state, run.executor, run.workflow, run.language].join(" ").toLowerCase();
    return row;
  });
  const body = el("tbody", {}, rows);
  const filter = () => {
    const words = runsFilter.toLowerCase().split(/\s+/).filter(Boolean);
    for (const row of rows) row.hidden = !words.every((word) => row.dataset.words.includes(word));
  };
  const search = el("input", { type: "search", placeholder: "Filter by run, state, executor or workflow",
    "aria-label": "filter the runs", oninput: (event) => { runsFilter = event.target.value; filter(); } });
  search.value = runsFilter;
  filter();
  main.replaceChildren(el("div", { class: "card" },
    el("h2", {}, "Runs"),
    el("div", { class: "tools" }, search),
    el("table", {},
      el("thead", {}, el("tr", {},
        el("th", {}, "Run"), el("th", {}, "Started"), el("th", {}, "State"),
        el("th", {}, "Executor"), el("th", { class: "number" }, "Tasks"),
        el("th", { class: "number" }, "Failed"), el("th", {}, "Workflow"),
        actions ? el("th", {}, "") : null)),
      body)));
  if (runsFilter) search.focus();
  // A running run changes the list; look again in a while.
  if (runs.some((run) => run.state === "running")) {
    setTimeout(() => { if (!view && location.pathname === "/") showRuns(); }, 5000);
  }
}

// Shows the view of the address once the scripts are loaded, then the tabs of the studio.
document.addEventListener("DOMContentLoaded", () => {
  studioReady = loadStudio();
  route();
});
