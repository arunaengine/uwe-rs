// The view of one run on the page of iunctus: its summary, graph, steps, tasks, and one task
// or step with its details. A running run updates live through Server-Sent Events. It uses
// the helpers of `page.js` and the graph view of `graph.js`.
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
    filter: { text: "", state: "", step: "", order: "asked" },
    shown: ROWS,
    chosen: null,
    chosenStep: null,
    events: null,
    graph: null,
  };
  document.title = `iunctus: ${base(data.run.workflow)} ${id}`;
  buildRun();
  drawAll();
  drawGraph();
  choose(task);
  listen();
}

// The frame of the run view: each part is filled by its own draw function. The graph comes
// first, next to the panel of the chosen task or step.
function buildRun() {
  view.parts = {
    summary: el("div", {}),
    progress: el("div", { class: "summary-progress" }),
    graph: el("section", { class: "card" }),
    steps: el("section", { class: "card" }),
    tasks: el("section", { class: "card" }),
    detail: el("aside", { class: "card side", hidden: true }),
  };
  const parts = view.parts;
  view.columns = el("div", { class: "columns" },
    el("div", {}, parts.graph, parts.steps, parts.tasks),
    parts.detail);
  main.replaceChildren(el("section", { class: "card" }, parts.summary, parts.progress),
    view.columns);
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
    facts.map(([name, value]) => el("div", {}, el("dt", {}, name),
      el("dd", name === "Time" ? { "data-time": "" } : {}, value)))));
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
  view.clock = view.parts.summary.querySelector("[data-time]");
  // The parameters and the command line side by side, until one opens.
  const folded = parts.filter((part) => part.tagName === "DETAILS");
  view.parts.summary.append(el("div", { class: "tools summary-more" }, folded));
}

// Keeps the time of a running run ticking between the updates of the stream.
function tickTime() {
  if (!view || !view.clock || view.run.state !== "running") return;
  view.clock.textContent = duration(Date.now() - view.run.started);
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
      el("span", { class: fill(state), style: `width: ${(100 * found[state]) / total}%` })));
  const legend = el("div", { class: "legend" },
    STATES.filter((state) => found[state]).map((state) =>
      el("span", {}, el("i", { class: fill(state) }), `${found[state]} ${label(state)}`)));
  legend.prepend(el("strong", {}, `${tasks.length} tasks`));
  const parts = [bar, legend];
  // A simulated run is a preview: it tells how much of the work a real run would take from
  // the cache, before any compute is spent.
  if (view.run.simulated && found.reused > 0) {
    const left = tasks.length - found.reused;
    parts.push(el("p", { class: "preview" }, left === 0
      ? `A real run would take all ${tasks.length} tasks from the cache; none would run.`
      : `A real run would take ${found.reused} of ${tasks.length} tasks from the cache; ` +
        `${left} would run.`));
  }
  view.parts.progress.replaceChildren(...parts);
}

// The colour of a state in the bar and the legend; the tasks of a simulated run that succeeded
// have the colour of simulated tasks, as in the graph.
function fill(state) {
  return view.run.simulated && state === "succeeded" ? "simulated-fill" : `${state}-fill`;
}

// The name of a state in the legend. In a simulated run, a task that succeeded was simulated,
// and a reused one is what a real run would take from the cache.
function label(state) {
  if (view.run.simulated && state === "succeeded") return "simulated";
  if (view.run.simulated && state === "reused") return "from the cache";
  return LABELS[state];
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
  if (filter.order === "longest") {
    // Tasks that have not started come last.
    const time = (task) => (task.started ? (task.ended || Date.now()) - task.started : -1);
    found.sort((a, b) => time(b) - time(a));
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
    const order = el("select", { onchange: (event) => {
      view.filter.order = event.target.value;
      view.shown = ROWS;
      drawRows();
    } }, el("option", { value: "asked" }, "in the order asked"),
      el("option", { value: "longest" }, "longest first"));
    view.stepChip = el("span", {});
    view.taskTools = el("div", { class: "tools" }, search, state, order, view.stepChip);
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
  view.chosenStep = null;
  drawRows();
  const detail = view.parts.detail;
  if (task === null || task === undefined) {
    detail.hidden = true;
    view.columns.classList.remove("open");
    markChosen();
    return;
  }
  detail.hidden = false;
  view.columns.classList.add("open");
  drawDetail(task);
  const node = markChosen();
  if (node) reveal(node);
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
      el("button", { onclick: closeDetail }, "close")),
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
  if (!row.reused && row.attempts > 0) parts.push(whyPart(id, task));
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

// Why the task ran instead of taking the result of an earlier run, filled when it is known.
function whyPart(id, task) {
  const part = el("div", {});
  json(`/api/runs/${encodeURIComponent(id)}/tasks/${task}/why`)
    .then((why) => {
      if (!why) return;
      const earlier = link(`/runs/${encodeURIComponent(why.run)}/tasks/${why.task}`,
        `task ${why.task} of run ${why.run}`);
      part.replaceChildren(el("h3", {}, "Why it ran"),
        el("p", { class: "quiet" }, "Compared with ", earlier, ":"),
        el("ul", {}, why.changes.map((change) => el("li", {}, change))));
    })
    .catch(() => {});
  return part;
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

// ---- one step ----

// Shows a step in the panel: its tasks by state, its markers, and how to see its tasks.
function chooseStep(name) {
  if (!view) return;
  if (view.chosen !== null && view.chosen !== undefined) {
    history.pushState(null, "", `/runs/${encodeURIComponent(view.id)}`);
    view.chosen = null;
    drawRows();
  }
  view.chosenStep = name;
  view.parts.detail.hidden = false;
  view.columns.classList.add("open");
  drawStepDetail();
  markChosen();
}

function closeDetail() {
  if (view.chosen !== null && view.chosen !== undefined) {
    go(`/runs/${encodeURIComponent(view.id)}`);
    return;
  }
  view.chosenStep = null;
  view.parts.detail.hidden = true;
  view.columns.classList.remove("open");
  markChosen();
}

function drawStepDetail() {
  const name = view.chosenStep;
  if (!name) return;
  const tasks = [...view.tasks.values()].filter((task) => task.step === name);
  const found = counts(tasks);
  const time = tasks.reduce((sum, task) =>
    sum + (task.started && task.ended ? task.ended - task.started : 0), 0);
  const ids = new Set(tasks.map((task) => task.id));
  const kinds = new Map();
  for (const marker of view.markers) {
    if (ids.has(marker.task) && !kinds.has(marker.kind)) kinds.set(marker.kind, marker);
  }
  const graph = view.graph;
  // A step is one box when the picture has its box: the server folds a step with more than
  // five tasks, and more steps when the graph is wide.
  const opened = Boolean(graph && graph.open.has(name));
  const folded = Boolean(graph && graph.view === "tasks" && graph.steps.has(name)) && !opened;
  const foldable = folded || (opened && graph.view === "tasks");
  const state = tallyState(tallyOf(tasks));
  const parts = [
    el("div", { class: "tools" },
      el("h2", { class: "mono", style: "margin: 0; flex: 1" }, name, " ", badge(state)),
      el("button", { onclick: closeDetail }, "close")),
    el("dl", { class: "facts" },
      el("div", {}, el("dt", {}, "Tasks"), el("dd", {}, tasks.length)),
      el("div", {}, el("dt", {}, "Task time"), el("dd", {}, duration(time)))),
    el("div", { class: "legend" }, STATES.filter((shown) => found[shown]).map((shown) =>
      el("span", {}, el("i", { class: `${shown}-fill` }), `${found[shown]} ${LABELS[shown]}`))),
  ];
  for (const marker of kinds.values()) {
    parts.push(el("p", { class: "marker" }, el("strong", {}, `${marker.kind}: `), marker.reason));
  }
  const actions = [el("button", { onclick: () => {
    view.filter.step = name;
    view.shown = ROWS;
    drawSteps();
    drawTasks();
    view.parts.tasks.scrollIntoView({ behavior: "smooth" });
  } }, "Show them in the table")];
  if (foldable) {
    actions.unshift(el("button", { onclick: () => {
      if (folded) openStep(name);
      else foldStep(name);
      drawStepDetail();
    } }, folded ? "Show its tasks in the graph" : "Fold its tasks into one box"));
  }
  parts.push(el("div", { class: "tools" }, actions));
  const first = tasks.slice(0, 50);
  parts.push(el("h3", {}, "Tasks"), el("table", {}, el("tbody", {}, first.map((task) =>
    el("tr", { class: "pick", onclick: () => go(taskPath(task.id)) },
      el("td", { class: "number" }, task.id), el("td", {}, task.name),
      el("td", {}, badge(shownState(task))))))));
  if (tasks.length > first.length) {
    parts.push(el("p", { class: "quiet" }, `and ${tasks.length - first.length} more in the table`));
  }
  view.parts.detail.replaceChildren(...parts);
}

// ---- live updates ----

function listen() {
  if (!view || view.run.state !== "running") return;
  const id = view.id;
  const events = new EventSource(`/api/runs/${encodeURIComponent(id)}/events?after=${view.sequence}`);
  view.events = events;
  liveMark.hidden = false;
  view.ticker = setInterval(tickTime, 1000);
  events.addEventListener("update", (message) => {
    if (!view || view.id !== id) return;
    const update = JSON.parse(message.data);
    view.sequence = update.sequence;
    view.run = update.run;
    for (const task of update.tasks) view.tasks.set(task.id, task);
    if (update.markers) view.markers = update.markers;
    drawAll();
    // States change in place; new tasks and markers need a new picture.
    paintStates(update.tasks);
    if (update.graph) graphChanged();
    if (view.chosen !== null && update.tasks.some((task) => task.id === view.chosen)) {
      drawDetail(view.chosen);
    }
    if (view.chosenStep && update.tasks.some((task) => task.step === view.chosenStep)) {
      drawStepDetail();
    }
    if (update.run.state !== "running") {
      events.close();
      clearInterval(view.ticker);
      liveMark.hidden = true;
      // The run is over: draw its whole graph now, without waiting.
      loadGraph({});
    }
  });
}
