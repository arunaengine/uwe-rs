// The canvas of the combiner: tool and input cards with their ports, the edges between them as
// curves, moving and zooming, dragging cards, and wiring an output to an input by two clicks.
// Copyright (c) 2026 The uwe Contributors
// SPDX-License-Identifier: MIT or Apache-2.0
"use strict";

// The sizes of a card in pixels at zoom 1; the ports sit in rows below the head.
const CARD_WIDTH = 320;
const CARD_HEAD = 56;
const PORT_ROW = 28;
const SVG_NS = "http://www.w3.org/2000/svg";

// The canvas calls `actions` for what the user does: `select(kind, id)`, `moved(id, x, y)`,
// `pick(end)` for a click on a port, `suggest(end, anchor)` and `output(end)`.
function makeCanvas(actions) {
  const world = el("div", { class: "world" });
  const wires = document.createElementNS(SVG_NS, "svg");
  wires.setAttribute("class", "wires");
  world.append(wires);
  const zoom = (factor) => () => zoomAt(factor, board.clientWidth / 2, board.clientHeight / 2);
  const board = el("div", { class: "board", tabindex: "0", "aria-label": "the workflow" }, world,
    el("div", { class: "zoom" },
      el("button", { title: "zoom in", onclick: zoom(1.2) }, "+"),
      el("button", { title: "zoom out", onclick: zoom(1 / 1.2) }, "−"),
      el("button", { title: "show the whole workflow", onclick: () => fit() }, "fit")));
  const camera = { x: 40, y: 40, scale: 1 };
  let model = null;
  let state = null;

  const place = () => {
    world.style.transform = `translate(${camera.x}px, ${camera.y}px) scale(${camera.scale})`;
  };

  function zoomAt(factor, x, y) {
    const scale = Math.min(2, Math.max(0.25, camera.scale * factor));
    camera.x = x - ((x - camera.x) * scale) / camera.scale;
    camera.y = y - ((y - camera.y) * scale) / camera.scale;
    camera.scale = scale;
    place();
  }

  // Shows every card: the camera moves so that they all fit the board.
  function fit() {
    const cards = model ? [...model.inputs, ...model.nodes] : [];
    if (!cards.length) return;
    const left = Math.min(...cards.map((card) => card.x));
    const top = Math.min(...cards.map((card) => card.y));
    const right = Math.max(...cards.map((card) => card.x + CARD_WIDTH));
    const bottom = Math.max(...cards.map((card) => card.y + cardHeight(card)));
    // The margin on the right leaves the zoom buttons free.
    const scale = Math.min(1.2, (board.clientWidth - 120) / (right - left || 1), (board.clientHeight - 80) / (bottom - top || 1));
    camera.scale = Math.max(0.25, scale);
    camera.x = 40 - left * camera.scale;
    camera.y = 40 - top * camera.scale;
    place();
  }

  board.addEventListener("wheel", (event) => {
    event.preventDefault();
    const box = board.getBoundingClientRect();
    zoomAt(Math.exp(-event.deltaY * 0.0015), event.clientX - box.left, event.clientY - box.top);
  }, { passive: false });

  // Dragging the board moves the camera; dragging the head of a card moves the card.
  board.addEventListener("pointerdown", (event) => {
    if (event.button !== 0 || event.target.closest("button, input, select, .zoom")) return;
    const head = event.target.closest(".card-head");
    const card = head && head.closest("[data-card]");
    const start = { x: event.clientX, y: event.clientY };
    if (card) {
      const found = cardOf(card.dataset.card);
      if (!found) return;
      const origin = { x: found.x, y: found.y };
      let moved = false;
      head.setPointerCapture(event.pointerId);
      const move = (next) => {
        moved = true;
        found.x = Math.round(origin.x + (next.clientX - start.x) / camera.scale);
        found.y = Math.round(origin.y + (next.clientY - start.y) / camera.scale);
        card.style.left = `${found.x}px`;
        card.style.top = `${found.y}px`;
        drawWires();
      };
      const end = () => {
        head.removeEventListener("pointermove", move);
        head.removeEventListener("pointerup", end);
        if (moved) actions.moved(found, origin);
        else actions.select(found.kind || "node", found.id);
      };
      head.addEventListener("pointermove", move);
      head.addEventListener("pointerup", end);
      return;
    }
    if (event.target.closest("[data-card], path")) return;
    const origin = { x: camera.x, y: camera.y };
    let moved = false;
    board.setPointerCapture(event.pointerId);
    board.classList.add("dragging");
    const move = (next) => {
      moved = true;
      camera.x = origin.x + next.clientX - start.x;
      camera.y = origin.y + next.clientY - start.y;
      place();
    };
    const end = () => {
      board.classList.remove("dragging");
      board.removeEventListener("pointermove", move);
      board.removeEventListener("pointerup", end);
      if (!moved) actions.select(null, null);
    };
    board.addEventListener("pointermove", move);
    board.addEventListener("pointerup", end);
  });

  function cardOf(id) {
    return model.nodes.find((node) => node.id === id) || model.inputs.find((input) => input.id === id);
  }

  // Where a port sits in the world: inputs on the left edge, outputs on the right one.
  function anchor(end, side) {
    const card = cardOf(end.node);
    if (!card) return null;
    const ports = side === "in" ? portsOf(card).inputs : portsOf(card).outputs;
    const row = Math.max(0, ports.findIndex((port) => port.name === end.port));
    return {
      x: card.x + (side === "in" ? 0 : CARD_WIDTH),
      y: card.y + CARD_HEAD + row * PORT_ROW + PORT_ROW / 2,
    };
  }

  function drawWires() {
    const paths = [];
    model.edges.forEach((edge, index) => {
      const from = anchor(edge.from, "out");
      const to = anchor(edge.to, "in");
      if (!from || !to) return;
      const bend = Math.max(48, Math.abs(to.x - from.x) / 2);
      const line = `M ${from.x} ${from.y} C ${from.x + bend} ${from.y}, ${to.x - bend} ${to.y}, ${to.x} ${to.y}`;
      const fit = state.edgeFit(edge);
      const chosen = state.selected && state.selected.kind === "edge" && state.selected.id === index;
      const hit = document.createElementNS(SVG_NS, "path");
      hit.setAttribute("d", line);
      hit.setAttribute("class", "wire-hit");
      hit.addEventListener("click", (event) => { event.stopPropagation(); actions.select("edge", index); });
      const drawn = document.createElementNS(SVG_NS, "path");
      drawn.setAttribute("d", line);
      drawn.setAttribute("class", `wire fit-${fit}${chosen ? " chosen" : ""}${edge.asserts && edge.asserts.length ? " asserted" : ""}`);
      paths.push(drawn, hit);
    });
    wires.replaceChildren(...paths);
  }

  function portButton(card, port, side) {
    const end = { node: card.id, port: port.name };
    const wiring = state.wiring;
    const connected = side === "in"
      ? model.edges.some((edge) => edge.to.node === card.id && edge.to.port === port.name)
      : model.edges.some((edge) => edge.from.node === card.id && edge.from.port === port.name);
    let fitClass = "";
    if (wiring && side === "in") fitClass = ` ${compatibility(wiring.port, port)}`;
    if (wiring && side === "out" && wiring.end.node === card.id && wiring.end.port === port.name) fitClass = " wiring";
    const formats = (port.formats || []).slice(0, 1).map((format) => el("span", { class: "chip format", title: format }, state.label(format)));
    const output = side === "out" && model.outputs.some((item) => item.from.node === card.id && item.from.port === port.name);
    const handle = el("button", {
      class: `port-handle ${side}${connected ? " connected" : ""}${fitClass}`,
      title: side === "in" ? `input ${port.name}` : `output ${port.name}: click, then click an input`,
      onclick: (event) => { event.stopPropagation(); actions.pick({ ...end, side, info: port }); },
    });
    // Every output and each input that nothing feeds yet can ask for the tools that fit it.
    const plus = side === "out" || !connected
      ? el("button", { class: "port-add",
        title: side === "out" ? "suggest tools that read this output" : "suggest tools that make this input",
        onclick: (event) => { event.stopPropagation(); actions.suggest({ ...end, side, info: port }, event.target); } }, "+")
      : null;
    const mark = side === "out" && card.kind !== "input"
      ? el("button", { class: `port-output${output ? " on" : ""}`, title: output ? "a workflow output; click to drop it" : "make this a workflow output",
        onclick: (event) => { event.stopPropagation(); actions.output(end); } }, "out")
      : null;
    const name = el("span", { class: "port-name mono", title: port.name }, port.name, port.required === false ? "?" : "");
    return el("div", { class: `port ${side}` },
      side === "in" ? [handle, name, ...formats, plus] : [plus, mark, ...formats, name, handle]);
  }

  function drawCard(card) {
    const ports = portsOf(card);
    const rows = Math.max(ports.inputs.length, ports.outputs.length, 1);
    const chosen = state.selected && state.selected.id === card.id;
    const verdict = state.nodeReport(card.id);
    const head = card.kind === "input"
      ? el("div", { class: "card-head" },
        el("div", { class: "card-title" }, el("span", { class: "chip" }, "input"), el("strong", { class: "mono" }, card.name)),
        el("div", { class: "quiet card-sub" }, `${card.list ? "list of " : ""}${card.type}${card.format ? `, ${state.label(card.format)}` : ""}`))
      : el("div", { class: "card-head" },
        el("div", { class: "card-title" },
          el("strong", { title: card.summary.title }, card.summary.title || card.tool),
          el("span", { class: `chip eco ${card.summary.ecosystem || ""}` }, card.summary.ecosystem || "tool")),
        el("div", { class: "card-sub" },
          el("span", { class: "mono quiet" }, card.id),
          verdict ? badge(fitState(verdict.fit), fitText(verdict.fit)) : null,
          verdict && verdict.scatter ? el("span", { class: "chip scatter", title: "maps over a list: one run per item" }, "scatter") : null));
    const body = [];
    for (let row = 0; row < rows; row += 1) {
      const input = ports.inputs[row];
      const output = ports.outputs[row];
      body.push(el("div", { class: "port-row" },
        input ? portButton(card, input, "in") : el("span", {}),
        output ? portButton(card, output, "out") : el("span", {})));
    }
    const node = el("div", {
      class: `flow-card${card.kind === "input" ? " input-card" : ""}${chosen ? " chosen" : ""}${verdict ? ` fit-${fitState(verdict.fit)}` : ""}`,
      "data-card": card.id,
      style: `left: ${card.x}px; top: ${card.y}px; width: ${CARD_WIDTH}px`,
    }, head, el("div", { class: "card-ports" }, body));
    return node;
  }

  return {
    node: board,
    fit,
    // The point of the board that the middle of the view shows, for a new card.
    middle() {
      return {
        x: Math.round((board.clientWidth / 2 - camera.x) / camera.scale - CARD_WIDTH / 2),
        y: Math.round((board.clientHeight / 2 - camera.y) / camera.scale - CARD_HEAD),
      };
    },
    draw(nextModel, nextState) {
      model = nextModel;
      state = nextState;
      const cards = [...model.inputs, ...model.nodes].map(drawCard);
      world.replaceChildren(wires, ...cards);
      board.classList.toggle("wiring", Boolean(state.wiring));
      drawWires();
      place();
    },
  };
}

// The ports of a card as the canvas draws them: a tool's, or the one output of an input.
function portsOf(card) {
  if (card.kind === "input") {
    return {
      inputs: [],
      outputs: [{ name: card.name, kind: card.type === "file" || card.type === "directory" ? card.type : { value: card.type },
        cardinality: card.list ? "list" : "one", formats: card.format ? [card.format] : [] }],
    };
  }
  return { inputs: card.summary.inputs || [], outputs: card.summary.outputs || [] };
}

function cardHeight(card) {
  const ports = portsOf(card);
  return CARD_HEAD + Math.max(ports.inputs.length, ports.outputs.length, 1) * PORT_ROW + 8;
}

// How well an output fits an input before the server checks it: kinds that differ do not fit;
// formats that share no term may still fit through EDAM, which the check decides.
function compatibility(output, input) {
  const kind = (port) => (typeof port.kind === "string" ? port.kind : "value");
  if (kind(output) !== kind(input)) return "misfit";
  const given = output.formats || [];
  const taken = input.formats || [];
  if (!given.length || !taken.length || given.some((format) => taken.includes(format))) return "fitting";
  return "maybe";
}

function fitState(fit) {
  return fit === "fits" ? "succeeded" : fit === "unknown" ? "reused" : "failed";
}

function fitText(fit) {
  return fit === "fits" ? "fits" : fit === "unknown" ? "unknown" : "does not fit";
}
