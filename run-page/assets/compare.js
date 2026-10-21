// Comparing two runs on the page of uwe: what differs between the runs themselves, and task
// by task what each does, which outputs have other content, and which came from the cache or an
// earlier run in one run and ran in the other. The run view offers it for the runs of the same
// workflow. It uses the helpers of `page.js`.
// Copyright (c) 2026 The uwe Contributors
// SPDX-License-Identifier: MIT or Apache-2.0
"use strict";

// The kinds of pairs, in the order the page shows them, with their titles.
const PAIRINGS = [
  ["changed", "Changed", "The task does something else: another image, command, script, environment or input."],
  ["outputs", "The same task, other outputs", "The task does the same, but outputs have other content."],
  ["first", "Only in the first run", ""],
  ["second", "Only in the second run", ""],
  ["same", "The same task, ended otherwise or came from elsewhere", ""],
];
// How many tasks each part shows until the user asks for all.
const PAIR_ROWS = 100;

async function showCompare(first, second) {
  leave();
  trail.replaceChildren(link("/", "runs"), " / compare");
  document.title = "uwe: compare two runs";
  main.replaceChildren(el("p", { class: "quiet" }, "Comparing..."));
  let data;
  try {
    data = await json(`/api/compare/${encodeURIComponent(first)}/${encodeURIComponent(second)}`);
  } catch (error) {
    main.replaceChildren(el("p", { class: "error" }, error.message));
    return;
  }
  if (view) return;
  const count = (pairing) => data.tasks.filter((pair) => pair.pairing === pairing).length;
  const elsewhere = data.tasks.filter((pair) =>
    pair.first && pair.second && Boolean(pair.first.reused) !== Boolean(pair.second.reused)).length;
  const tiles = [
    ["The same", count("same")],
    ["Changed", count("changed")],
    ["Other outputs", count("outputs")],
    ["Only in the first run", count("first")],
    ["Only in the second run", count("second")],
    ["Ran in one run only, came from elsewhere in the other", elsewhere],
  ];
  const cards = [el("section", { class: "card" },
    el("h1", {}, "Comparing two runs"),
    el("div", { class: "pair-of-runs" }, runCard("First run", data.first), runCard("Second run", data.second)),
    data.changes.length
      ? el("div", {}, el("h3", {}, "What differs between the runs"),
        el("ul", {}, data.changes.map((change) => el("li", {}, sentence(change)))))
      : el("p", { class: "quiet" }, "The runs have the same workflow, revision, parameters and inputs."),
    el("div", { class: "tiles" }, tiles.map(([label, value]) =>
      el("div", { class: "tile" }, el("div", { class: "tile-label" }, label),
        el("div", { class: "tile-value" }, value)))))];
  for (const [pairing, title, note] of PAIRINGS) {
    const pairs = data.tasks.filter((pair) => pair.pairing === pairing &&
      (pairing !== "same" || pair.results.length));
    if (pairs.length) cards.push(pairCard(title, note, pairs, data));
  }
  main.replaceChildren(...cards);
}

// A sentence of the database, with the names and values that it quotes in `...` as code.
function sentence(text) {
  return text.split("`").map((part, index) => (index % 2 ? el("code", {}, part) : part));
}

// One run of the two: a link to it and a few facts.
function runCard(title, run) {
  const tasks = Object.values(run.tasks).reduce((sum, count) => sum + count, 0);
  return el("div", { class: "tile" },
    el("div", { class: "tile-label" }, title),
    el("div", {}, link(`/runs/${encodeURIComponent(run.id)}`, run.id), " ", badge(run.state)),
    el("div", { class: "quiet mono" }, run.workflow),
    el("div", { class: "quiet" }, `${when(run.started)}, ${run.simulated ? "simulated" : run.executor}, ` +
      `${tasks} tasks${run.revision ? `, revision ${run.revision.slice(0, 12)}` : ""}`));
}

// A part of the comparison: its tasks with what differs.
function pairCard(title, note, pairs, data) {
  const table = el("tbody", {});
  const card = el("section", { class: "card" }, el("h2", {}, `${title} (${pairs.length})`),
    note ? el("p", { class: "quiet" }, note) : null,
    el("table", {}, el("thead", {}, el("tr", {}, el("th", {}, "Task"), el("th", {}, "Step"),
      el("th", {}, "First run"), el("th", {}, "Second run"), el("th", {}, "What differs"))), table));
  const side = (run, found) => (found
    ? el("span", {}, link(`/runs/${encodeURIComponent(run)}/tasks/${found.task}`, `task ${found.task}`), " ",
      badge(found.reused ? "reused" : found.state))
    : el("span", { class: "quiet" }, "none"));
  const fill = (shown) => {
    table.replaceChildren(...pairs.slice(0, shown).map((pair) => {
      const named = pair.first || pair.second;
      const lines = [...pair.changes, ...pair.outputs, ...pair.results];
      return el("tr", {},
        el("td", {}, named.name),
        el("td", { class: "mono" }, pair.step),
        el("td", {}, side(data.first.id, pair.first)),
        el("td", {}, side(data.second.id, pair.second)),
        el("td", {}, lines.length ? el("ul", { class: "differences" }, lines.map((line) => el("li", {}, sentence(line)))) : ""));
    }));
    const more = card.querySelector(":scope > .more");
    if (more) more.remove();
    if (pairs.length > shown) {
      card.append(el("button", { class: "more", onclick: () => fill(shown + PAIR_ROWS) },
        `show more (${pairs.length - shown} left)`));
    }
  };
  fill(PAIR_ROWS);
  return card;
}

// The part of the run view that offers to compare the run with another run of its workflow.
async function comparePart(run) {
  const part = el("div", { class: "tools compare-with" });
  let runs;
  try {
    runs = await json("/api/runs");
  } catch {
    return part;
  }
  const others = runs.filter((other) => other.workflow === run.workflow && other.id !== run.id);
  if (!others.length) return part;
  // The run before this one first, as `uwe compare <run>` takes it.
  const before = others.find((other) => other.started <= run.started) || others[0];
  const choice = el("select", {}, others.map((other) => el("option", {
    value: other.id, selected: other.id === before.id,
  }, `${other.id} (${when(other.started)}, ${other.state})`)));
  part.append(el("span", { class: "quiet" }, "Compare with"), choice,
    el("button", { onclick: () => {
      const other = runs.find((found) => found.id === choice.value);
      const [first, second] = other.started <= run.started ? [other.id, run.id] : [run.id, other.id];
      go(`/compare/${encodeURIComponent(first)}/${encodeURIComponent(second)}`);
    } }, "Compare"));
  return part;
}
