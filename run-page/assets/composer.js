// The combiner of the studio: the state with undo, the live check of the graph, the toolbar,
// wiring and suggestions, and the dialogs of import, export and run. It joins the canvas of
// `canvas.js`, the explorer of `explorer.js` and the inspector of `inspector.js`.
// Copyright (c) 2026 The uwe Contributors
// SPDX-License-Identifier: MIT or Apache-2.0
"use strict";

// The combiner of this page, kept while the user visits other views.
let composer = null;

async function showComposer() {
  leave();
  trail.replaceChildren();
  document.title = "uwe: combiner";
  const here = { kind: "compose", close() { closePopover(); } };
  view = here;
  await studioReady;
  if (view !== here) return;
  if (!studio.studio) {
    main.replaceChildren(el("div", { class: "card" }, el("h2", {}, "This page only shows runs"),
      el("p", { class: "quiet" }, "`uwe serve`, or `uwe page` with `--tools`, combines tools on the page.")));
    return;
  }
  const first = !composer;
  if (first) composer = startComposer();
  main.replaceChildren(composer.node);
  composer.redraw();
  composer.search();
  composer.check();
  // A workflow kept from an earlier visit shows whole on the first visit of this page.
  if (first) composer.fit();
}

function startComposer() {
  const state = {
    model: loadModel(),
    undo: [],
    redo: [],
    selected: null,
    wiring: null,
    report: null,
    // Whether a check of a newer model is still to answer; the run waits for it.
    pending: false,
    labels: {},
    definitions: new Map(),
    query: { q: "", kind: "all", input_format: "", output_format: "" },
  };
  const canvas = makeCanvas({
    select: (kind, id) => select(kind, id),
    moved: (card, origin) => moved(card, origin),
    pick: (end) => pick(end),
    suggest: (end, anchor) => suggest(end, anchor),
    output: (end) => toggleOutput(end),
  });
  const parts = {
    explorer: el("aside", { class: "explorer" }),
    toolbar: el("div", { class: "toolbar" }),
    detail: el("div", { class: "detail" }),
    findings: el("div", { class: "validation" }),
  };
  const node = el("div", { class: "composer" }, parts.explorer,
    el("section", { class: "stage-pane" }, parts.toolbar, canvas.node),
    el("aside", { class: "inspector" }, parts.detail, parts.findings));
  // What the explorer and the inspector share with the rest of the combiner.
  const context = {
    state, parts, change, select, redraw, addTool,
    // The short names of the common formats fit the cards; other terms take their EDAM label.
    label: (term) => (FORMATS.some(([id]) => id === term) ? labelOf(term) : state.labels[term] || labelOf(term)),
    edgeCheck,
  };
  const explorer = makeExplorer(context);
  const inspector = makeInspector(context);
  let checking = null;
  // The number of the latest check; an answer to an earlier one is dropped.
  let checks = 0;

  // ---- the model and its history ----

  // Changes the model with undo; `refresh` draws the detail panel again, which a form that
  // changed the model itself does not want.
  function change(work, refresh = true) {
    state.undo.push(clone(state.model));
    state.redo = [];
    work(state.model);
    saveModel(state.model);
    redraw(refresh);
    scheduleCheck();
  }

  function restore(from, to) {
    if (!from.length) return;
    to.push(clone(state.model));
    state.model = from.pop();
    state.selected = null;
    saveModel(state.model);
    redraw();
    scheduleCheck();
  }

  // A card that the canvas moved already: only the history learns where it was.
  function moved(card, origin) {
    const before = clone(state.model);
    const found = [...before.nodes, ...before.inputs].find((item) => item.id === card.id);
    if (found) Object.assign(found, origin);
    state.undo.push(before);
    state.redo = [];
    saveModel(state.model);
  }

  function scheduleCheck() {
    clearTimeout(checking);
    state.pending = true;
    drawToolbar();
    checking = setTimeout(check, 350);
  }

  async function check() {
    const asked = ++checks;
    if (!state.model.nodes.length) {
      state.report = null;
      state.pending = false;
      redraw();
      return;
    }
    state.pending = true;
    drawToolbar();
    let report;
    try {
      report = await send("/api/composer/check", { workflow: toWorkflow(state.model) });
    } catch (error) {
      report = { error: error.message };
    }
    if (asked !== checks) return;
    state.report = report;
    state.pending = false;
    redraw(Boolean(state.selected && state.selected.kind === "edge"));
  }

  // What the check says of a wire, or null before a check.
  function edgeCheck(edge) {
    const report = state.report && state.report.report;
    if (!report) return null;
    const at = state.report.edges.findIndex((item) => sameWire(item, edge));
    return at < 0 ? null : report.edges[at];
  }

  // ---- drawing ----

  function look() {
    const report = state.report && state.report.report;
    return {
      selected: state.selected,
      wiring: state.wiring,
      label: context.label,
      nodeReport: (id) => report && report.nodes.find((item) => item.id === id),
      edgeFit: (edge) => {
        const found = edgeCheck(edge);
        return found ? found.fit : "none";
      },
    };
  }

  function redraw(refresh = true) {
    canvas.draw(state.model, look());
    drawToolbar();
    inspector.draw(refresh);
  }

  function drawToolbar() {
    const model = state.model;
    const name = el("input", { type: "text", class: "mono", value: model.name, "aria-label": "name of the workflow",
      onchange: (event) => change((next) => { next.name = event.target.value.trim() || "workflow"; }) });
    const report = state.report && state.report.report;
    const misfit = Boolean(report) && report.fit === "does_not_fit";
    const clear = () => {
      if (confirm("Start a new workflow? The canvas is cleared.")) change((next) => Object.assign(next, blankModel()));
    };
    parts.toolbar.replaceChildren(
      name,
      el("button", { onclick: () => restore(state.undo, state.redo), disabled: !state.undo.length, title: "undo" }, "Undo"),
      el("button", { onclick: () => restore(state.redo, state.undo), disabled: !state.redo.length, title: "redo" }, "Redo"),
      el("button", { onclick: () => { change(layout); canvas.fit(); } }, "Auto-layout"),
      el("button", { onclick: addInput }, "+ Input"),
      el("button", { onclick: openImport }, "Import"),
      el("button", { onclick: openExport, disabled: !model.nodes.length }, "Export"),
      el("button", { onclick: clear }, "New"),
      el("span", { class: "grow" }),
      el("button", { class: "primary", onclick: openRun, disabled: !model.nodes.length || misfit || state.pending,
        title: misfit ? "the graph does not fit; the check says why"
          : state.pending ? "the graph is being checked" : "run the workflow" }, "Run workflow"));
  }

  // ---- adding and wiring ----

  function freeId(stem) {
    const plain = String(stem).replace(/[^A-Za-z0-9_.-]/g, "_") || "step";
    const taken = new Set([...state.model.nodes.map((item) => item.id), ...state.model.inputs.map((item) => item.name)]);
    return freeName(taken, plain);
  }

  // Adds a tool, at `place` or in the middle of the view, and gives back its node.
  function addTool(tool, place) {
    const where = place || canvas.middle();
    const added = { id: freeId(tool.id), tool: tool.id, version: tool.version, x: where.x, y: where.y,
      parameters: {}, summary: summaryOf(tool) };
    change((model) => model.nodes.push(added));
    return added;
  }

  function addInput() {
    const where = canvas.middle();
    const name = freeId("input");
    change((model) => model.inputs.push({ kind: "input", id: `input:${name}`, name, type: "file", list: false,
      format: "", description: "", optional: false, x: where.x - CARD_WIDTH - 60, y: where.y }));
    select("input", `input:${name}`);
  }

  function select(kind, id) {
    state.selected = kind ? { kind, id } : null;
    if (!kind && state.wiring) state.wiring = null;
    redraw();
  }

  // A click on a port: an output starts a wire, an input then ends it.
  function pick(end) {
    if (end.side === "out") {
      state.wiring = { end: { node: end.node, port: end.port }, port: end.info };
      redraw();
      return;
    }
    if (!state.wiring) return;
    const from = state.wiring.end;
    state.wiring = null;
    connect(from, { node: end.node, port: end.port });
  }

  // Wires an output to an input; the input loses the wire it had, as it takes one source.
  function connect(from, to) {
    if (from.node === to.node) {
      redraw();
      return;
    }
    change((model) => {
      model.edges = model.edges.filter((edge) => !(edge.to.node === to.node && edge.to.port === to.port));
      model.edges.push({ from, to, asserts: [] });
    });
  }

  function toggleOutput(end) {
    change((model) => {
      const at = model.outputs.findIndex((item) => item.from.node === end.node && item.from.port === end.port);
      if (at >= 0) model.outputs.splice(at, 1);
      else {
        const taken = new Set(model.outputs.map((item) => item.name));
        model.outputs.push({ name: freeName(taken, `${end.node}_${end.port}`), from: end });
      }
    });
  }

  // The tools that can follow an output or make an input, ranked by the server; a choice adds
  // the tool beside the card and wires it at once.
  async function suggest(end, anchor) {
    const card = [...state.model.nodes, ...state.model.inputs].find((item) => item.id === end.node);
    if (!card) return;
    const popover = openPopover(anchor, el("p", { class: "quiet" }, "Looking for tools..."));
    let answer;
    try {
      answer = await send("/api/tools/suggest", suggestion(card, end));
    } catch (error) {
      popover.replaceChildren(el("p", { class: "error" }, error.message));
      return;
    }
    const choose = (item) => {
      closePopover();
      if (!item.summary) return;
      const beside = end.side === "out" ? CARD_WIDTH + 80 : -CARD_WIDTH - 80;
      const added = addTool(item.summary, { x: card.x + beside, y: card.y });
      if (end.side === "out") connect({ node: card.id, port: end.port }, { node: added.id, port: item.port });
      else connect({ node: added.id, port: item.port }, { node: card.id, port: end.port });
    };
    const items = answer.suggestions.map((item) => suggestionButton(item, () => choose(item)));
    popover.replaceChildren(el("h3", {}, end.side === "out" ? `Tools that read ${end.port}` : `Tools that make ${end.port}`),
      items.length ? el("div", { class: "suggestions" }, items) : el("p", { class: "quiet" }, "No tool of the library fits."));
  }

  // ---- import, export and run ----

  function openImport() {
    const text = el("textarea", { rows: "16", class: "mono", placeholder: "uwe_version: \"1\"\nname: ...\nsteps: ..." });
    const file = el("input", { type: "file", accept: ".yaml,.yml,.json", onchange: async (event) => {
      const chosen = event.target.files[0];
      if (chosen) text.value = await chosen.text();
    } });
    const said = el("div", {});
    const dialog = openDialog("Import a composed workflow", [file, text, said], [
      el("button", { class: "primary", onclick: async () => {
        try {
          const answer = await send("/api/composer/import", { text: text.value });
          const model = await fromWorkflow(answer.workflow);
          change((next) => Object.assign(next, model));
          canvas.fit();
          explorer.search();
          dialog.close();
        } catch (error) {
          said.replaceChildren(el("pre", { class: "error" }, error.message));
        }
      } }, "Import")]);
  }

  function openExport() {
    const shown = el("pre", { class: "export-text" }, "");
    const formats = el("div", { class: "switch" });
    let current = { text: "", file: "workflow.uwe.yaml" };
    // Only the latest export answers; the buttons wait for it, so they never take an older one.
    let exports = 0;
    const copy = (event) => navigator.clipboard.writeText(current.text).then(
      () => { event.target.textContent = "Copied"; }, () => { event.target.textContent = "Select the text to copy it"; });
    const buttons = [el("button", { onclick: copy }, "Copy"),
      el("button", { class: "primary", onclick: () => download(current) }, "Download")];
    const show = async (format) => {
      const asked = ++exports;
      for (const button of formats.children) button.setAttribute("aria-pressed", String(button.dataset.format === format));
      for (const button of buttons) button.disabled = true;
      shown.textContent = "Writing...";
      try {
        const answer = await send("/api/composer/export", { workflow: toWorkflow(state.model), format });
        if (asked !== exports) return;
        current = answer;
        shown.textContent = current.text;
        for (const button of buttons) button.disabled = false;
      } catch (error) {
        if (asked !== exports) return;
        shown.textContent = error.message;
      }
    };
    formats.append(...[["yaml", ".uwe.yaml"], ["json", "JSON"], ["wir", "wir"]].map(([format, label]) =>
      el("button", { "data-format": format, onclick: () => show(format) }, label)));
    openDialog("Export", [formats, shown], buttons);
    show("yaml");
  }

  function openRun() {
    const fields = state.model.inputs.map(runField);
    const executor = el("select", { "aria-label": "executor" }, studio.executors.map((name) => el("option", { value: name }, name)));
    executor.value = studio.executors.includes("local") ? "local" : studio.executors[0];
    const offline = el("input", { type: "checkbox" });
    const fresh = el("input", { type: "checkbox" });
    const image = el("input", { type: "text", class: "mono", placeholder: "default image", "aria-label": "default image" });
    const output = el("input", { type: "text", class: "mono", placeholder: "output folder", "aria-label": "output folder" });
    const said = el("div", {});
    const start = async (event) => {
      const inputs = {};
      for (const item of fields) {
        const value = item.read();
        if (value !== undefined) inputs[item.input.name] = value;
      }
      const request = { workflow: toWorkflow(state.model), inputs, executor: executor.value };
      if (offline.checked) request.no_network = true;
      if (fresh.checked) request.no_cache = true;
      if (image.value.trim()) request.default_image = image.value.trim();
      if (output.value.trim()) request.output_dir = output.value.trim();
      event.target.disabled = true;
      said.replaceChildren(el("p", { class: "quiet" }, "Lowering and starting the run..."));
      try {
        const answer = await send("/api/composer/run", request);
        dialog.close();
        go(`/runs/${encodeURIComponent(answer.run_id)}`);
      } catch (error) {
        event.target.disabled = false;
        said.replaceChildren(el("pre", { class: "error" }, error.message));
      }
    };
    const dialog = openDialog("Run the workflow", [
      ...(fields.length ? fields.map((item) => item.node) : [el("p", { class: "quiet" }, "The workflow has no inputs.")]),
      el("div", { class: "tools" }, el("span", { class: "quiet" }, "Executor"), executor,
        el("label", { class: "check" }, offline, "no network"), el("label", { class: "check" }, fresh, "no cache")),
      el("div", { class: "tools" }, image, output),
      said,
    ], [el("button", { class: "primary", onclick: start }, "Run workflow")]);
  }

  document.addEventListener("keydown", (event) => {
    if (!view || view.kind !== "compose") return;
    if (event.key === "Escape") {
      closePopover();
      if (state.wiring) { state.wiring = null; redraw(); }
      return;
    }
    const editing = event.target.closest && event.target.closest("input, textarea, select");
    if (editing || !(event.ctrlKey || event.metaKey)) return;
    if (event.key === "z" && !event.shiftKey) { event.preventDefault(); restore(state.undo, state.redo); }
    if (event.key === "y" || (event.key === "z" && event.shiftKey)) { event.preventDefault(); restore(state.redo, state.undo); }
  });

  return { node, redraw, search: explorer.search, check, fit: () => canvas.fit() };
}

// ---- helpers of the combiner ----

// `stem`, or `stem_2`, `stem_3` and so on, whichever `taken` does not hold yet.
function freeName(taken, stem) {
  if (!taken.has(stem)) return stem;
  for (let number = 2; ; number += 1) if (!taken.has(`${stem}_${number}`)) return `${stem}_${number}`;
}

// The request of the tools that fit a port of a card.
function suggestion(card, end) {
  if (end.side !== "out") return { mode: "producers", tool: card.tool, version: card.version, port: end.port };
  if (card.kind === "input") return { mode: "followers", supply: supplyOf(card) };
  return { mode: "followers", tool: card.tool, version: card.version, port: end.port };
}

// One suggested tool: how well its port fits, which a click takes.
function suggestionButton(item, choose) {
  const format = item.check.format;
  const [state, text] = format === "exact" ? ["succeeded", "exact"]
    : format === "general" ? ["running", "more general"] : ["reused", "warning"];
  return el("button", { class: "suggestion", onclick: choose },
    el("span", { class: "grow" }, el("strong", {}, item.title || item.tool), " ", el("span", { class: "mono quiet" }, item.port)),
    el("span", { class: `chip eco ${item.ecosystem || ""}` }, item.ecosystem || ""),
    badge(state, text),
    item.check.scatter ? el("span", { class: "chip scatter" }, "scatter") : null);
}

// The control of one workflow input in the run dialog, and how it reads its value.
function runField(input) {
  const integer = input.type === "integer";
  const typed = (text) => (integer ? (wholeNumber(text) ? Number(text) : text)
    : input.type === "number" ? Number(text) : text);
  const messages = el("div", { class: "field-messages", hidden: true });
  let control;
  if (input.type === "boolean") control = el("input", { type: "checkbox" });
  else if (input.list) control = el("textarea", { rows: "3", class: "mono", placeholder: "one value per line" });
  else {
    control = el("input", { type: input.type === "number" ? "number" : "text",
      inputmode: integer ? "numeric" : undefined, step: input.type === "number" ? "any" : undefined,
      class: "mono", oninput: integer ? () => flagInteger(control, messages) : undefined,
      placeholder: input.type === "file" ? "path or address of a file" : input.type === "directory" ? "path of a folder" : "" });
  }
  const read = () => {
    if (input.type === "boolean") return control.checked;
    if (input.list) {
      const lines = control.value.split("\n").map((line) => line.trim()).filter(Boolean);
      return lines.length ? lines.map(typed) : undefined;
    }
    return control.value.trim() === "" ? undefined : typed(control.value.trim());
  };
  const node = el("label", { class: "field" },
    el("span", { class: "field-label" }, el("span", { class: "mono" }, input.name),
      input.format ? el("span", { class: "chip format", title: input.format }, labelOf(input.format)) : null,
      input.optional ? null : el("span", { class: "chip required" }, "required")),
    control,
    input.description ? el("span", { class: "quiet field-help" }, input.description) : null,
    messages);
  return { input, node, read };
}

// Saves a text as a file of the browser.
function download(exported) {
  const url = URL.createObjectURL(new Blob([exported.text], { type: "text/plain" }));
  const anchor = el("a", { href: url, download: exported.file });
  document.body.append(anchor);
  anchor.click();
  anchor.remove();
  URL.revokeObjectURL(url);
}

// ---- dialogs and popovers ----

function openDialog(title, body, buttons) {
  const dialog = el("dialog", { class: "studio-dialog" },
    el("div", { class: "tools" }, el("h2", { class: "grow" }, title),
      el("button", { onclick: () => dialog.close(), "aria-label": "close" }, "Close")),
    el("div", { class: "dialog-body" }, body),
    el("div", { class: "tools dialog-buttons" }, buttons));
  dialog.addEventListener("close", () => dialog.remove());
  document.body.append(dialog);
  dialog.showModal();
  return dialog;
}

let popover = null;
function openPopover(anchor, content) {
  closePopover();
  const box = anchor.getBoundingClientRect();
  popover = el("div", { class: "popover", role: "dialog" }, content);
  popover.style.left = `${Math.min(window.innerWidth - 380, Math.max(8, box.left))}px`;
  popover.style.top = `${Math.min(window.innerHeight - 120, box.bottom + 6)}px`;
  document.body.append(popover);
  setTimeout(() => document.addEventListener("pointerdown", outside), 0);
  return popover;
}

function outside(event) {
  if (popover && !popover.contains(event.target)) closePopover();
}

function closePopover() {
  document.removeEventListener("pointerdown", outside);
  if (popover) popover.remove();
  popover = null;
}
