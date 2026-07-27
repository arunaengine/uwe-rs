// The script of the page of iunctus: the helpers that both views share, the navigation, and
// the list of runs. `run.js` shows one run.
// Copyright (c) 2026 The iunctus Contributors
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

const main = document.getElementById("main");
const trail = document.getElementById("trail");
const liveMark = document.getElementById("live");

// The run on the page, or null on the list of runs.
let view = null;

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

async function json(url) {
  const response = await fetch(url);
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
  return el("a", { href: path, "data-route": true }, text);
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
  go(anchor.getAttribute("href"));
});
window.addEventListener("popstate", route);

function go(path) {
  if (path !== location.pathname) history.pushState(null, "", path);
  route();
}

function route() {
  const parts = location.pathname.split("/").filter(Boolean).map(decodeURIComponent);
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

// ---- the list of runs ----

async function showRuns() {
  leave();
  trail.textContent = "runs";
  document.title = "iunctus: runs";
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
      el("p", { class: "quiet" }, "Every `iunctus run` records itself here.")));
    return;
  }
  const rows = runs.map((run) => {
    const total = Object.values(run.tasks).reduce((sum, count) => sum + count, 0);
    return el("tr", {},
      el("td", { class: "mono" }, link(`/runs/${encodeURIComponent(run.id)}`, run.id)),
      el("td", {}, when(run.started)),
      el("td", {}, badge(run.state)),
      el("td", {}, run.simulated ? "simulated" : run.executor),
      el("td", { class: "number" }, total),
      el("td", { class: "number" }, run.tasks.failed || ""),
      el("td", {}, run.workflow));
  });
  main.replaceChildren(el("div", { class: "card" },
    el("h2", {}, "Runs"),
    el("table", {},
      el("thead", {}, el("tr", {},
        el("th", {}, "Run"), el("th", {}, "Started"), el("th", {}, "State"),
        el("th", {}, "Executor"), el("th", { class: "number" }, "Tasks"),
        el("th", { class: "number" }, "Failed"), el("th", {}, "Workflow"))),
      el("tbody", {}, rows))));
  // A running run changes the list; look again in a while.
  if (runs.some((run) => run.state === "running")) {
    setTimeout(() => { if (!view && location.pathname === "/") showRuns(); }, 5000);
  }
}

// Shows the view of the address once both scripts are loaded.
document.addEventListener("DOMContentLoaded", route);
