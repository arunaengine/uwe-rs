// The boxes of the graph view on the page of iunctus: their tooltips, their states while a run
// goes on, the chosen box, and the legend of the states the picture shows.
// Copyright (c) 2026 The iunctus Contributors
// SPDX-License-Identifier: MIT or Apache-2.0
"use strict";

const SVG_SPACE = "http://www.w3.org/2000/svg";
// The states of the boxes in the order of `State::ALL` in `workflow-graph`, which the counts
// of a step follow.
const BOX_STATES = ["simulated", "succeeded", "reused", "failed", "running", "waiting", "stopped"];
// The width of a character of the counts in a box of a step, as `svg.rs` measures it.
const SMALL_CHARACTER = 6.6;

// ---- the tooltip ----

function hovered(event) {
  const graph = view.graph;
  if (graph.press) return;
  const node = event.target.closest(".node");
  if (!node) {
    hideTip();
    return;
  }
  graph.tip.replaceChildren(...tipOf(node));
  graph.tip.hidden = false;
  placeTip(event);
}

function placeTip(event) {
  const tip = view.graph.tip;
  const x = Math.min(event.clientX + 14, window.innerWidth - tip.offsetWidth - 8);
  const y = event.clientY + 16 + tip.offsetHeight > window.innerHeight
    ? event.clientY - tip.offsetHeight - 10 : event.clientY + 16;
  tip.style.left = `${Math.max(4, x)}px`;
  tip.style.top = `${Math.max(4, y)}px`;
}

function hideTip() {
  if (view && view.graph) view.graph.tip.hidden = true;
}

// What the tooltip of a box says, from what the page knows now.
function tipOf(node) {
  if (node.dataset.task !== undefined) {
    const row = view.tasks.get(Number(node.dataset.task));
    if (!row) return [node.dataset.tip || ""];
    const markers = view.markers.filter((marker) => marker.task === row.id);
    const facts = [
      row.started ? `time ${duration((row.ended || Date.now()) - row.started)}` : "",
      row.exit_code !== null && row.exit_code !== undefined ? `exit code ${row.exit_code}` : "",
      row.reused ? `result from ${row.reused === "cache" ? "the cache" : `run ${row.reused}`}` : "",
      row.attempts > 1 ? `${row.attempts} attempts` : "",
      markers.length ? `${markers.length} ${markers.length === 1 ? "marker" : "markers"}` : "",
    ].filter(Boolean);
    return [el("div", {}, el("strong", {}, row.name), " ", badge(boxState(row))),
      el("div", { class: "quiet mono" }, row.step),
      facts.length ? el("div", {}, facts.join(", ")) : null].filter(Boolean);
  }
  if (node.dataset.step !== undefined) {
    const name = node.dataset.step;
    const tally = stepTallies(new Set([name])).get(name) || tallyOf([]);
    const hint = view.graph.view === "tasks" ? "Click to show its tasks." : "Click to show the step.";
    return [el("div", {}, el("strong", { class: "mono" }, name)),
      el("div", {}, tallyText(tally, true)), el("div", { class: "quiet" }, hint)];
  }
  const [first, ...rest] = (node.dataset.tip || "").split("\n");
  return [el("div", {}, el("strong", {}, first)), rest.length ? el("ul", {}, rest.map((line) => el("li", {}, line))) : null]
    .filter(Boolean);
}

// ---- live states ----

// The state that the box of a task shows.
function boxState(row) {
  if (row.state === "succeeded" && row.reused) return "reused";
  if (row.state === "succeeded" && view.run.simulated) return "simulated";
  return row.state;
}

function tallyOf(rows) {
  const tally = Object.fromEntries(BOX_STATES.map((state) => [state, 0]));
  for (const row of rows) tally[boxState(row)] += 1;
  return tally;
}

// The tally of the tasks of each of these steps.
function stepTallies(names) {
  const found = new Map([...names].map((name) => [name, tallyOf([])]));
  for (const row of view.tasks.values()) {
    const tally = found.get(row.step);
    if (tally) tally[boxState(row)] += 1;
  }
  return found;
}

// The state of a box of several tasks: a failure first, then work that goes on, then how the
// tasks ended, as `Tally::state` of `workflow-graph` says.
function tallyState(tally) {
  const order = ["failed", "running", "waiting", "stopped", "succeeded", "simulated"];
  return order.find((state) => tally[state] > 0) || "reused";
}

// `24 tasks`, or `24 tasks: 20 succeeded, 4 running`, as `workflow-graph` writes it.
function tallyText(tally, spelled) {
  const total = BOX_STATES.reduce((sum, state) => sum + tally[state], 0);
  const tasks = total === 1 ? "1 task" : `${total} tasks`;
  if (!spelled && tally[tallyState(tally)] === total) return tasks;
  const states = BOX_STATES.filter((state) => tally[state]).map((state) => `${tally[state]} ${state}`);
  return `${tasks}: ${states.join(", ")}`;
}

// Gives a box a state: its class and its icon.
function setState(node, state) {
  if (node.classList.contains(state)) return;
  const kind = node.classList.contains("step") ? "step" : "task";
  const chosen = node.classList.contains("chosen") ? " chosen" : "";
  node.setAttribute("class", `node ${kind} ${state}${chosen}`);
  const icon = node.querySelector(".icon");
  if (icon) icon.setAttribute("href", `#icon-${state}`);
}

// Writes the counts of a step into its box, cut to the room that the box has.
function setCounts(node, text) {
  const detail = node.querySelector(".detail");
  const box = node.querySelector(".box");
  if (!detail || !box) return;
  const room = Math.floor((Number(box.getAttribute("width")) -
    (Number(detail.getAttribute("x")) - Number(box.getAttribute("x"))) - 8) / SMALL_CHARACTER);
  const shown = text.length > room ? `${text.slice(0, Math.max(0, room - 1))}…` : text;
  if (detail.textContent !== shown) detail.textContent = shown;
}

// Gives the boxes the states that the page knows now: of the tasks in `rows`, or of all.
function paintStates(rows) {
  const graph = view && view.graph;
  if (!graph || !graph.content) return;
  const changed = rows || [...view.tasks.values()];
  const steps = new Set();
  for (const row of changed) {
    const node = document.getElementById(`t${row.id}`);
    if (node) setState(node, boxState(row));
    if (graph.steps.has(row.step)) steps.add(row.step);
  }
  for (const [name, tally] of stepTallies(steps)) {
    const node = graph.steps.get(name);
    setState(node, tallyState(tally));
    setCounts(node, tallyText(tally, false));
  }
  drawGraphLegend();
}

// Marks the box of the chosen task, or of its step when that is one box, or the chosen step.
function markChosen() {
  const graph = view && view.graph;
  if (!graph || !graph.content) return null;
  for (const node of graph.content.querySelectorAll(".node.chosen")) node.classList.remove("chosen");
  let node = null;
  if (view.chosen !== null && view.chosen !== undefined) {
    node = document.getElementById(`t${view.chosen}`);
    const row = view.tasks.get(view.chosen);
    if (!node && row) node = graph.steps.get(row.step) || null;
  } else if (view.chosenStep) {
    node = graph.steps.get(view.chosenStep) || null;
  }
  if (node) node.classList.add("chosen");
  return node;
}

// ---- the legend ----

// A small icon of a state, from the symbols of the picture.
function stateIcon(name) {
  const icon = document.createElementNS(SVG_SPACE, "svg");
  icon.setAttribute("class", "state-icon");
  icon.setAttribute("viewBox", "0 0 20 20");
  icon.setAttribute("aria-hidden", "true");
  const use = document.createElementNS(SVG_SPACE, "use");
  use.setAttribute("href", `#icon-${name}`);
  icon.append(use);
  return icon;
}

// The states that the picture shows, what a dashed box means, and how to use the view.
function drawGraphLegend() {
  const graph = view.graph;
  const content = graph.content;
  const shown = BOX_STATES.filter((state) => content.querySelector(`.node.${state}`));
  const entries = shown.map((state) => el("span", {}, stateIcon(state), state));
  if (content.querySelector(".node.open-point")) {
    entries.push(el("span", {}, stateIcon("open"), "more tasks may follow from real data"));
  }
  if (content.querySelector(".stack")) {
    entries.push(el("span", {}, el("i", { class: "stack-sign" }), "a step with several tasks"));
  }
  const hint = graph.view === "tasks" && graph.steps.size
    ? "Drag to move, Ctrl + wheel to zoom (or click the graph first); click a step to show its tasks."
    : "Drag to move, Ctrl + wheel to zoom (or click the graph first).";
  graph.legend.replaceChildren(...entries, el("span", { class: "quiet hint" }, hint));
}
