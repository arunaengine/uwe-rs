// The camera of the graph view on the page of iunctus: drag to move the picture, zoom with the
// wheel, the buttons or the keys, and fit it to the view. The first view shows the start of the
// graph, and large pictures hide what lies outside the view, so that moving stays fast. A click
// that does not move the picture goes to the box under the pointer.
// Copyright (c) 2026 The iunctus Contributors
// SPDX-License-Identifier: MIT or Apache-2.0
"use strict";

// The smallest and largest zoom, and how much a button press zooms.
const ZOOM_LEAST = 0.02;
const ZOOM_MOST = 4;
const ZOOM_STEP = 1.25;
// The first view shows the whole picture when it fits at this zoom or more, else its start at
// the readable zoom.
const OVERVIEW = 0.5;
const READABLE = 0.85;
// Below this zoom the names in the boxes are hidden, which keeps large graphs fast.
const FAR = 0.3;
// How far the pointer moves before a press becomes a drag, in pixels.
const DRAG = 4;
// The space kept around a picture that fits the view.
const FIT_MARGIN = 24;

// ---- the camera ----

function clamp(value, least, most) {
  return Math.min(most, Math.max(least, value));
}

function applyCamera(smooth) {
  const graph = view.graph;
  if (!graph.content) return;
  const { x, y, scale } = graph.camera;
  graph.content.classList.toggle("moving", smooth);
  graph.content.style.transform = `translate(${x}px, ${y}px) scale(${scale})`;
  graph.stage.classList.toggle("far", scale < FAR);
  const dots = clamp(20 * scale, 8, 80);
  graph.stage.style.backgroundSize = `${dots}px ${dots}px`;
  graph.stage.style.backgroundPosition = `${x}px ${y}px`;
  if (graph.items && !graph.hiding) {
    graph.hiding = true;
    requestAnimationFrame(hideFarItems);
  }
}

// The middle of the view, in the coordinates of the picture.
function viewCentre() {
  const graph = view.graph;
  const box = graph.stage.getBoundingClientRect();
  const { x, y, scale } = graph.camera;
  return { x: (box.width / 2 - x) / scale, y: (box.height / 2 - y) / scale };
}

// The zoom at which the whole picture fits the view.
function fitScale() {
  const graph = view.graph;
  const box = graph.stage.getBoundingClientRect();
  const { width, height } = graph.size;
  if (!width || !height || !box.width || !box.height) return 1;
  const scale = Math.min((box.width - 2 * FIT_MARGIN) / width, (box.height - 2 * FIT_MARGIN) / height);
  return clamp(scale, ZOOM_LEAST, 1.2);
}

// Shows the whole picture; new pictures are fitted too, until the user moves the view.
function fitGraph(smooth) {
  const graph = view.graph;
  const box = graph.stage.getBoundingClientRect();
  const scale = fitScale();
  graph.camera = { scale, x: (box.width - graph.size.width * scale) / 2,
    y: (box.height - graph.size.height * scale) / 2 };
  graph.framing = "whole";
  applyCamera(smooth);
}

// Shows the picture from its start, the top or the left: all of it when it fits well enough,
// else its first tasks at a readable zoom, around the first box nearest the middle of all.
function frameStart(smooth) {
  const graph = view.graph;
  const box = graph.stage.getBoundingClientRect();
  const across = graph.direction === "down" ? "x" : "y";
  const along = graph.direction === "down" ? "y" : "x";
  const room = { x: box.width, y: box.height };
  const size = { x: graph.size.width, y: graph.size.height };
  const fit = fitScale();
  const scale = fit >= OVERVIEW ? fit : READABLE;
  let middle = size[across] / 2;
  const points = [...graph.content.querySelectorAll(".node")].map(place);
  if (fit < OVERVIEW && points.length) {
    const all = centroid(points)[across];
    const first = Math.min(...points.map((point) => point[along]));
    const layer = points.filter((point) => point[along] - first < 30);
    const distance = (point) => Math.abs(point[across] - all);
    middle = layer.reduce((best, point) => (distance(point) < distance(best) ? point : best))[across];
  }
  const camera = { scale, x: 0, y: 0 };
  camera[across] = room[across] / 2 - middle * scale;
  camera[along] = FIT_MARGIN / 2;
  graph.camera = camera;
  graph.framing = "start";
  applyCamera(smooth);
}

// Frames the picture again as the user last asked, unless they moved the view since.
function reframe(smooth) {
  const graph = view.graph;
  if (!graph.content) return;
  if (graph.framing === "whole") fitGraph(smooth);
  else if (graph.framing === "start") frameStart(smooth);
}

function zoomAt(factor, x, y, smooth) {
  const camera = view.graph.camera;
  const scale = clamp(camera.scale * factor, ZOOM_LEAST, ZOOM_MOST);
  camera.x = x - ((x - camera.x) * scale) / camera.scale;
  camera.y = y - ((y - camera.y) * scale) / camera.scale;
  camera.scale = scale;
  view.graph.framing = null;
  applyCamera(smooth);
}

// Zooms around the middle of the view.
function zoomBy(factor) {
  const box = view.graph.stage.getBoundingClientRect();
  zoomAt(factor, box.width / 2, box.height / 2, true);
}

// Moves the view so that a box is in it, when it is not.
function reveal(node) {
  const graph = view.graph;
  const box = graph.stage.getBoundingClientRect();
  const at = place(node);
  const { x, y, scale } = graph.camera;
  const seen = { x: at.x * scale + x, y: at.y * scale + y };
  if (seen.x > 40 && seen.x < box.width - 40 && seen.y > 40 && seen.y < box.height - 40) return;
  graph.camera.x = box.width / 2 - at.x * scale;
  graph.camera.y = box.height / 2 - at.y * scale;
  graph.framing = null;
  applyCamera(true);
}

// The extent of each box and line of a picture, for hiding what lies outside the view.
function extents(content) {
  const items = [];
  for (const node of content.querySelectorAll(".node")) {
    const at = place(node);
    const box = node.querySelector(".box");
    const left = at.x + Number(box.getAttribute("x"));
    const top = at.y + Number(box.getAttribute("y"));
    // The second box of a stack lies a little further.
    const width = Number(box.getAttribute("width")) + 4;
    const height = Number(box.getAttribute("height")) + 4;
    items.push({ element: node, left, top, right: left + width, bottom: top + height, shown: true });
  }
  for (const edge of content.querySelectorAll(".edge")) {
    const numbers = (edge.getAttribute("d").match(/-?[\d.]+/g) || []).map(Number);
    const xs = numbers.filter((_, index) => index % 2 === 0);
    const ys = numbers.filter((_, index) => index % 2 === 1);
    if (!xs.length) continue;
    items.push({ element: edge, left: Math.min(...xs), top: Math.min(...ys), right: Math.max(...xs),
      bottom: Math.max(...ys), shown: true });
  }
  return items;
}

// Shows the boxes and lines of a large picture that are in or near the view, and hides the rest.
function hideFarItems() {
  const graph = view && view.graph;
  if (!graph) return;
  graph.hiding = false;
  if (!graph.items) return;
  const box = graph.stage.getBoundingClientRect();
  const { x, y, scale } = graph.camera;
  const margin = 300 / scale;
  const left = -x / scale - margin;
  const top = -y / scale - margin;
  const right = (box.width - x) / scale + margin;
  const bottom = (box.height - y) / scale + margin;
  for (const item of graph.items) {
    const shown = item.right >= left && item.left <= right && item.bottom >= top && item.top <= bottom;
    if (shown === item.shown) continue;
    item.shown = shown;
    item.element.style.display = shown ? "" : "none";
  }
}

// ---- the pointer and the keys ----

function pressed(event) {
  if (event.button !== 0 || event.target.closest(".zoom")) return;
  const graph = view.graph;
  graph.active = true;
  graph.press = { x: event.clientX, y: event.clientY, camera: { ...graph.camera }, moved: false,
    target: event.target };
  graph.stage.setPointerCapture(event.pointerId);
}

function moved(event) {
  const graph = view.graph;
  const press = graph.press;
  if (!press) {
    if (!graph.tip.hidden) placeTip(event);
    return;
  }
  const x = event.clientX - press.x;
  const y = event.clientY - press.y;
  if (!press.moved && Math.hypot(x, y) < DRAG) return;
  press.moved = true;
  hideTip();
  graph.stage.classList.add("dragging");
  graph.camera.x = press.camera.x + x;
  graph.camera.y = press.camera.y + y;
  graph.framing = null;
  applyCamera(false);
}

function released() {
  const graph = view.graph;
  const press = graph.press;
  graph.press = null;
  graph.stage.classList.remove("dragging");
  if (press && !press.moved) clicked(press.target);
  if (graph.waiting) {
    graph.waiting = false;
    graphChanged();
  }
}

// The wheel zooms with Ctrl (or Cmd), and after a click into the graph; else it scrolls the
// page, as it does over a map on a page, and a hint says how to zoom.
function wheeled(event) {
  const graph = view.graph;
  if (!(event.ctrlKey || event.metaKey || graph.active)) {
    showZoomHint();
    return;
  }
  event.preventDefault();
  const box = view.graph.stage.getBoundingClientRect();
  const speed = event.deltaMode === 1 ? 0.05 : 0.0015;
  zoomAt(Math.exp(-event.deltaY * speed), event.clientX - box.left, event.clientY - box.top, false);
}

// Says for a moment how to zoom, when the wheel was used without Ctrl.
function showZoomHint() {
  const graph = view.graph;
  graph.zoomHint.hidden = false;
  clearTimeout(graph.zoomHintTimer);
  graph.zoomHintTimer = setTimeout(() => { graph.zoomHint.hidden = true; }, 1500);
}

function keyed(event) {
  const graph = view.graph;
  const steps = { ArrowLeft: [80, 0], ArrowRight: [-80, 0], ArrowUp: [0, 80], ArrowDown: [0, -80] };
  if (event.key === "+" || event.key === "=") zoomBy(ZOOM_STEP);
  else if (event.key === "-") zoomBy(1 / ZOOM_STEP);
  else if (event.key === "0") fitGraph(true);
  else if (steps[event.key]) {
    graph.camera.x += steps[event.key][0];
    graph.camera.y += steps[event.key][1];
    graph.framing = null;
    applyCamera(true);
  } else return;
  event.preventDefault();
}

// A click on a box: a task opens its panel; a step that is one box opens in the graph and
// shows its panel; an open point stands for the box before it.
function clicked(target) {
  const node = target.closest ? target.closest(".node") : null;
  if (!node) return;
  if (node.dataset.task !== undefined) {
    go(taskPath(Number(node.dataset.task)));
  } else if (node.dataset.step !== undefined) {
    const name = node.dataset.step;
    if (view.graph.view === "tasks") openStep(name);
    chooseStep(name);
  } else if (node.dataset.host) {
    const host = document.getElementById(node.dataset.host);
    if (host) clicked(host);
  }
}
