// The graph view of a run on the page of iunctus. The server draws the picture with the layout
// of iunctus, the same SVG as `iunctus graph -format svg`; this script shows it in a card with
// switches for the view and the direction, and opens a step that is folded into one box with a
// click. While a run goes on, a new picture takes the place of the old one when tasks come, and
// the boxes glide to their new places. `camera.js` moves and zooms the picture, `boxes.js` keeps
// the states of its boxes; both use the helpers of `page.js` and the view of `run.js`.
// Copyright (c) 2026 The iunctus Contributors
// SPDX-License-Identifier: MIT or Apache-2.0
"use strict";

// How long boxes glide to their new places, in milliseconds.
const GLIDE = 450;
// Pictures with more boxes than this change without gliding, so that large runs stay fast.
const GLIDE_MOST = 2500;
// Pictures with more boxes than this hide what lies outside the view, so that moving them
// stays fast.
const HIDE_LEAST = 1500;

// ---- the frame of the view ----

// Builds the card of the graph and loads the first picture.
function drawGraph() {
  const graph = {
    view: "tasks",
    direction: "down",
    open: new Set(),
    camera: { x: 0, y: 0, scale: 1 },
    // How the camera follows new pictures: "start" or "whole"; null once the user moved it.
    framing: "start",
    picture: null,
    content: null,
    size: { width: 0, height: 0 },
    steps: new Map(),
    // The boxes and lines of a large picture with their extents, and whether each is shown.
    items: null,
    hiding: false,
    loading: 0,
    timer: null,
    drawn: 0,
    press: null,
    waiting: false,
    // Whether the wheel zooms without Ctrl: after a click into the graph, until the pointer
    // leaves it.
    active: false,
    zoomHint: null,
    zoomHintTimer: null,
  };
  view.graph = graph;
  graph.tools = el("div", {});
  graph.tip = el("div", { class: "graph-tip", hidden: true });
  const zoom = el("div", { class: "zoom" },
    el("button", { title: "Zoom in", "aria-label": "zoom in", onclick: () => zoomBy(ZOOM_STEP) }, "+"),
    el("button", { title: "Zoom out", "aria-label": "zoom out", onclick: () => zoomBy(1 / ZOOM_STEP) }, "−"),
    el("button", { title: "Fit to screen", onclick: () => fitGraph(true) }, "Fit"));
  graph.message = el("p", { class: "quiet stage-message" }, "Drawing...");
  graph.zoomHint = el("p", { class: "stage-hint", hidden: true }, "Use Ctrl + scroll to zoom the graph");
  graph.stage = el("div", { class: "stage", tabindex: "0",
    "aria-label": "The graph of the run. Drag to move it, use Ctrl and the wheel to zoom." },
  graph.message, graph.zoomHint, zoom);
  graph.legend = el("div", { class: "legend graph-legend" });
  const stage = graph.stage;
  stage.addEventListener("pointerdown", pressed);
  stage.addEventListener("pointermove", moved);
  stage.addEventListener("pointerup", released);
  stage.addEventListener("pointercancel", released);
  stage.addEventListener("pointerover", hovered);
  stage.addEventListener("pointerleave", () => { graph.active = false; hideTip(); });
  stage.addEventListener("blur", () => { graph.active = false; });
  stage.addEventListener("wheel", wheeled, { passive: false });
  stage.addEventListener("keydown", keyed);
  graph.observer = new ResizeObserver(() => { if (view && view.graph === graph) reframe(false); });
  graph.observer.observe(stage);
  view.parts.graph.replaceChildren(graph.tools, stage, graph.legend, graph.tip);
  graphTools();
  loadGraph({});
}

// The title, the switches of the view and the direction, and the steps that are open.
function graphTools() {
  const graph = view.graph;
  const switches = (choices, current, choose) => el("span", { class: "switch" },
    choices.map(([name, text]) => el("button", { "aria-pressed": String(name === current),
      onclick: () => { if (name !== current) choose(name); } }, text)));
  const views = switches([["tasks", "Tasks"], ["steps", "Steps"]], graph.view, (name) => {
    graph.view = name;
    graph.framing = "start";
    graphTools();
    loadGraph({});
  });
  const directions = switches([["down", "Top down"], ["right", "Left to right"]], graph.direction,
    (name) => {
      graph.direction = name;
      graph.framing = "start";
      graphTools();
      loadGraph({});
    });
  const path = Boolean(view.time && view.time.path);
  const critical = el("button", { "aria-pressed": String(path),
    title: "The chain of tasks that set the length of the run", onclick: () => showPath(!path) },
  "Critical path");
  const rows = [el("div", { class: "tools" },
    el("h2", { style: "margin: 0; flex: 1" }, "Graph"), views, directions, critical)];
  if (graph.view === "tasks" && graph.open.size) {
    rows.push(el("div", { class: "tools" }, el("span", { class: "quiet" }, "Open steps:"),
      [...graph.open].map((name) => el("span", { class: "chip mono", title: name },
        name.split(":").slice(-2).join(":"),
        el("button", { title: "Fold its tasks into one box", onclick: () => foldStep(name) }, "fold"))),
      graph.open.size > 1 ? el("button", { onclick: foldAll }, "fold all") : null));
  }
  graph.tools.replaceChildren(...rows);
}

// Shows the tasks of a folded step in the graph.
function openStep(name) {
  view.graph.open.add(name);
  graphTools();
  loadGraph({ focus: name });
}

function foldStep(name) {
  view.graph.open.delete(name);
  graphTools();
  loadGraph({ focus: name });
  if (view.chosenStep === name) drawStepDetail();
}

function foldAll() {
  view.graph.open.clear();
  graphTools();
  loadGraph({});
  if (view.chosenStep) drawStepDetail();
}

// ---- pictures ----

// Asks the server for the picture of the graph as the view wants it, and shows it.
async function loadGraph(how) {
  const graph = view.graph;
  const id = view.id;
  if (graph.timer) clearTimeout(graph.timer);
  graph.timer = null;
  graph.drawn = Date.now();
  graph.loading += 1;
  const number = graph.loading;
  const query = new URLSearchParams({ view: graph.view, direction: graph.direction });
  if (graph.open.size) query.set("open", [...graph.open].join("\n"));
  let text;
  try {
    const response = await fetch(`/api/runs/${encodeURIComponent(id)}/graph.svg?${query}`);
    if (!response.ok) throw new Error((await response.text()) || response.statusText);
    text = await response.text();
  } catch (error) {
    if (view && view.graph === graph) {
      graph.message.textContent = `The graph cannot be drawn: ${error.message}`;
    }
    return;
  }
  if (!view || view.graph !== graph || number !== graph.loading) return;
  const root = new DOMParser().parseFromString(text, "image/svg+xml").documentElement;
  if (root.nodeName !== "svg") {
    graph.message.textContent = "The graph cannot be drawn.";
    return;
  }
  mountPicture(document.importNode(root, true), how);
}

// Puts a new picture in the place of the old one. The page keeps only the graph itself: the
// title and legend of the picture are on the page already.
function mountPicture(picture, how) {
  const graph = view.graph;
  const content = picture.querySelector(":scope > .graph");
  if (!content) return;
  for (const part of picture.querySelectorAll(":scope > .background, :scope > .heading, :scope > .legend")) {
    part.remove();
  }
  for (const attribute of ["width", "height", "viewBox"]) picture.removeAttribute(attribute);
  content.removeAttribute("transform");
  // The page shows its own tooltip with what it knows now; open points keep their reasons.
  for (const title of content.querySelectorAll(".node > title")) {
    title.parentNode.dataset.tip = title.textContent;
    title.remove();
  }
  const before = graph.picture ? places(graph.content) : null;
  const centre = graph.picture ? viewCentre() : null;
  graph.steps = new Map();
  for (const node of content.querySelectorAll(".node.step")) graph.steps.set(node.dataset.step, node);
  graph.size = { width: Number(content.dataset.width) || 0, height: Number(content.dataset.height) || 0 };
  graph.message.hidden = true;
  if (graph.picture) graph.picture.replaceWith(picture);
  else graph.stage.prepend(picture);
  graph.picture = picture;
  graph.content = content;
  graph.items = content.querySelectorAll(".node").length > HIDE_LEAST ? extents(content) : null;
  paintStates(null);
  markChosen();
  markPath();
  drawGraphLegend();
  if (!before || graph.framing) {
    reframe(Boolean(before));
    glide(before, { x: 0, y: 0 });
    return;
  }
  const shift = anchorShift(before, how, centre);
  graph.camera.x -= shift.x * graph.camera.scale;
  graph.camera.y -= shift.y * graph.camera.scale;
  applyCamera(false);
  glide(before, shift);
}

// The place of a box, from the translation that the server gave it.
function place(node) {
  const found = /translate\(([-\d.]+)[ ,]+([-\d.]+)\)/.exec(node.getAttribute("transform") || "");
  return found ? { x: Number(found[1]), y: Number(found[2]) } : { x: 0, y: 0 };
}

function centroid(points) {
  const sum = points.reduce((total, point) => ({ x: total.x + point.x, y: total.y + point.y }),
    { x: 0, y: 0 });
  return { x: sum.x / points.length, y: sum.y / points.length };
}

// The places of the boxes of a picture: by id, the boxes of steps by name, and the boxes of
// tasks by the name of their step.
function places(content) {
  const found = { boxes: new Map(), steps: new Map(), tasks: new Map() };
  for (const node of content.querySelectorAll(".node")) {
    const at = place(node);
    found.boxes.set(node.id, at);
    if (node.dataset.step !== undefined) found.steps.set(node.dataset.step, at);
    const row = node.dataset.task !== undefined ? view.tasks.get(Number(node.dataset.task)) : null;
    if (row) {
      if (!found.tasks.has(row.step)) found.tasks.set(row.step, []);
      found.tasks.get(row.step).push(at);
    }
  }
  return found;
}

// Where a box of the new picture was before: at its own place; for a task of a step that was
// one box, at that box; for a step that is one box now, amid its tasks.
function origin(before, node) {
  const own = before.boxes.get(node.id);
  if (own) return own;
  if (node.dataset.step !== undefined) {
    const tasks = before.tasks.get(node.dataset.step);
    return tasks ? centroid(tasks) : null;
  }
  const row = node.dataset.task !== undefined ? view.tasks.get(Number(node.dataset.task)) : null;
  return row && before.steps.has(row.step) ? before.steps.get(row.step) : null;
}

// How far the new picture must move so that what the user looks at stays in place: a step
// that opened or folded, else the box nearest to the middle of the view that is in both.
function anchorShift(before, how, centre) {
  const now = places(view.graph.content);
  if (how.focus !== undefined) {
    const old = before.steps.get(how.focus) ||
      (before.tasks.has(how.focus) ? centroid(before.tasks.get(how.focus)) : null);
    const found = now.steps.get(how.focus) ||
      (now.tasks.has(how.focus) ? centroid(now.tasks.get(how.focus)) : null);
    if (old && found) return { x: found.x - old.x, y: found.y - old.y };
  }
  let best = null;
  for (const [id, old] of before.boxes) {
    const found = now.boxes.get(id);
    if (!found) continue;
    const distance = Math.hypot(old.x - centre.x, old.y - centre.y);
    if (!best || distance < best.distance) best = { distance, shift: { x: found.x - old.x, y: found.y - old.y } };
  }
  return best ? best.shift : { x: 0, y: 0 };
}

// Lets the boxes of the new picture glide from where they were. `shift` is how far the camera
// moved the picture, so that a box starts where it was on the screen.
function glide(before, shift) {
  const graph = view.graph;
  const nodes = graph.content.querySelectorAll(".node");
  if (!before || nodes.length > GLIDE_MOST) return;
  const moving = [];
  for (const node of nodes) {
    const from = origin(before, node);
    const to = place(node);
    if (!from) {
      node.classList.add("arriving");
      moving.push([node, null]);
      continue;
    }
    const start = { x: from.x + shift.x, y: from.y + shift.y };
    if (Math.abs(start.x - to.x) < 0.5 && Math.abs(start.y - to.y) < 0.5) continue;
    node.style.transform = `translate(${start.x}px, ${start.y}px)`;
    moving.push([node, to]);
  }
  const edges = graph.content.querySelector(".edges");
  if (edges) edges.classList.add("arriving");
  // The browser draws the start first, then the boxes glide.
  requestAnimationFrame(() => requestAnimationFrame(() => {
    graph.stage.classList.add("gliding");
    for (const [node, to] of moving) {
      if (to) node.style.transform = `translate(${to.x}px, ${to.y}px)`;
      node.classList.remove("arriving");
    }
    if (edges) edges.classList.remove("arriving");
    setTimeout(() => {
      graph.stage.classList.remove("gliding");
      for (const [node] of moving) node.style.transform = "";
    }, GLIDE + 100);
  }));
}

// Draws the graph again soon, but not more often than every few seconds, and not while the
// user drags it.
function graphChanged() {
  const graph = view && view.graph;
  if (!graph || graph.timer) return;
  if (graph.press) {
    graph.waiting = true;
    return;
  }
  const wait = Math.max(0, GRAPH_PAUSE - (Date.now() - graph.drawn));
  graph.timer = setTimeout(() => loadGraph({}), wait);
}
