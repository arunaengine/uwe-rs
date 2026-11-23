// The inspector of the combiner: the detail of the selected step, wire, input or tool, the
// form of the parameters of a step, and every finding of the live check of the graph.
// Copyright (c) 2026 The uwe Contributors
// SPDX-License-Identifier: MIT or Apache-2.0
"use strict";

// The conditions that a user can assert of the data on a wire.
const ASSERTIONS = [[{ sorted: "coordinate" }, "Sorted by coordinate"], [{ sorted: "name" }, "Sorted by name"],
  ["indexed", "Indexed"]];
// The kinds of value that a workflow input takes.
const INPUT_TYPES = ["file", "directory", "string", "integer", "number", "boolean"];

// The inspector draws into `context.parts.detail` and `context.parts.findings`. It returns
// `draw(refresh)`, which draws the detail again only when the selection changed or `refresh`
// asks, so that a form keeps its focus while the check runs.
function makeInspector(context) {
  const { state, parts, change, select } = context;
  let shown = null;

  function draw(refresh) {
    const chosen = state.selected;
    const key = chosen ? `${chosen.kind} ${chosen.id}` : "";
    if (refresh || key !== shown) {
      shown = key;
      parts.detail.replaceChildren(detail(chosen));
    }
    parts.findings.replaceChildren(...findings());
  }

  function detail(chosen) {
    switch (chosen && chosen.kind) {
      case "node": return nodePart(chosen.id);
      case "edge": return edgePart(chosen.id);
      case "input": return inputPart(chosen.id);
      case "tool": return toolPart(chosen.id);
      default: return workflowPart();
    }
  }

  function workflowPart() {
    const model = state.model;
    const description = el("textarea", { rows: "2", placeholder: "What the workflow does", "aria-label": "description",
      onchange: (event) => change((next) => { next.description = event.target.value.trim(); }) });
    description.value = model.description || "";
    return el("div", {},
      el("h2", {}, "Workflow"),
      description,
      el("p", { class: "quiet" }, `${model.nodes.length} steps, ${model.inputs.length} inputs, ${model.edges.length} wires, `
        + `${model.outputs.length} outputs. Click a card, a port or a wire.`),
      state.wiring ? el("p", { class: "notice" }, "Click an input to wire it; Esc stops.") : null);
  }

  // The whole definition of a tool version, asked once per page.
  async function definitionOf(tool, version) {
    const key = `${tool}\u0000${version}`;
    if (!state.definitions.has(key)) {
      const answer = await json(`/api/tools/${encodeURIComponent(tool)}/${encodeURIComponent(version)}`);
      Object.assign(state.labels, answer.labels || {});
      state.definitions.set(key, answer);
    }
    return state.definitions.get(key);
  }

  function nodePart(id) {
    const node = state.model.nodes.find((item) => item.id === id);
    if (!node) return el("p", { class: "quiet" }, "The step is gone.");
    const part = el("div", {});
    const rename = el("input", { type: "text", class: "mono", value: node.id, "aria-label": "id of the step",
      onchange: (event) => renameNode(node.id, event.target.value.trim()) });
    const head = [
      el("div", { class: "tools" }, el("h2", { class: "grow" }, node.summary.title || node.tool),
        el("button", { class: "danger", onclick: () => removeNode(node.id) }, "Remove")),
      el("label", { class: "field" }, el("span", { class: "field-label" }, "Step id"), rename),
      el("dl", { class: "facts" },
        fact("Tool", `${node.tool} ${node.version}`, "mono"),
        fact("Kind", node.summary.ecosystem || ""),
        node.summary.image ? fact("Image", node.summary.image, "mono") : null),
    ];
    const given = state.model.edges.filter((edge) => edge.to.node === node.id && edge.from.node.startsWith("input:"));
    if (given.length) {
      head.push(el("p", { class: "quiet" }, "From the workflow inputs: ",
        given.map((edge) => `${edge.to.port} ← ${edge.from.port}`).join(", ")));
    }
    part.append(...head, el("p", { class: "quiet" }, "Loading the parameters..."));
    definitionOf(node.tool, node.version)
      .then((answer) => part.replaceChildren(...head, ...parameters(node, answer.definition, part)))
      .catch((error) => part.replaceChildren(...head, el("p", { class: "error" }, error.message)));
    return part;
  }

  // The source of a step's tool and the form of its parameters, which change the step a
  // moment after the last edit.
  function parameters(node, definition, part) {
    const schema = definition.parameters && Object.keys(definition.parameters).length ? definition.parameters : null;
    const shown = [];
    if (definition.source) shown.push(el("p", { class: "quiet mono" }, `${definition.source.kind}: ${definition.source.address}`));
    if (!schema) return [...shown, el("p", { class: "quiet" }, "The tool has no parameters.")];
    const form = schemaForm(schema, node.parameters || {}, {
      onChange: () => {
        clearTimeout(part.timer);
        part.timer = setTimeout(() => change((model) => {
          const found = model.nodes.find((item) => item.id === node.id);
          if (found) found.parameters = form.value() || {};
        }, false), 500);
      },
    });
    return [...shown, el("h3", {}, "Parameters"), form.node];
  }

  function edgePart(index) {
    const edge = state.model.edges[index];
    if (!edge) return el("p", { class: "quiet" }, "The wire is gone.");
    const found = context.edgeCheck(edge);
    const fromInput = edge.from.node.startsWith("input:");
    const toggles = ASSERTIONS.map(([condition, label]) => {
      const same = (item) => JSON.stringify(item) === JSON.stringify(condition);
      const box = el("input", { type: "checkbox", onchange: (event) => change((model) => {
        const target = model.edges[index];
        target.asserts = (target.asserts || []).filter((item) => !same(item));
        if (event.target.checked) target.asserts.push(condition);
      }) });
      box.checked = (edge.asserts || []).some(same);
      return el("label", { class: "check" }, box, label);
    });
    return el("div", {},
      el("div", { class: "tools" }, el("h2", { class: "grow" }, "Wire"),
        el("button", { class: "danger", onclick: () => change((model) => { model.edges.splice(index, 1); state.selected = null; }) }, "Remove")),
      el("p", { class: "mono" }, `${edge.from.node}/${edge.from.port} → ${edge.to.node}/${edge.to.port}`),
      found ? verdict(found)
        : el("p", { class: "quiet" }, fromInput ? "A workflow input; the step's check covers it." : "Not checked yet."),
      fromInput ? null : el("h3", {}, "The data is"),
      fromInput ? null : el("div", { class: "tools" }, toggles));
  }

  // What the check says of one wire: its fit, how the formats match, and why.
  function verdict(found) {
    return el("div", { class: "verdict" },
      el("div", { class: "tags" },
        badge(fitState(found.fit), fitText(found.fit)),
        el("span", { class: "chip" }, `format ${found.format}`),
        found.scatter ? el("span", { class: "chip scatter" }, "scatter") : null),
      el("ul", { class: "findings" }, found.findings.map((finding) =>
        el("li", { class: `finding fit-${finding.fit}` }, `${finding.aspect}: ${finding.reason}`))));
  }

  function inputPart(id) {
    const input = state.model.inputs.find((item) => item.id === id);
    if (!input) return el("p", { class: "quiet" }, "The input is gone.");
    const edit = (work) => change((model) => {
      const found = model.inputs.find((item) => item.id === id);
      if (found) work(found, model);
    });
    const name = el("input", { type: "text", class: "mono", value: input.name, onchange: (event) => {
      const next = event.target.value.trim().replace(/[^A-Za-z0-9_.-]/g, "_");
      if (!next || next === input.name) return;
      edit((found, model) => {
        const old = found.id;
        found.name = next;
        found.id = `input:${next}`;
        for (const edge of model.edges) if (edge.from.node === old) edge.from = { node: found.id, port: next };
        state.selected = { kind: "input", id: found.id };
      });
    } });
    const type = el("select", { onchange: (event) => edit((found) => { found.type = event.target.value; }) },
      INPUT_TYPES.map((kind) => el("option", { value: kind }, kind)));
    type.value = input.type;
    const format = el("select", { onchange: (event) => edit((found) => { found.format = event.target.value; }) },
      FORMATS.map(([term, label]) => el("option", { value: term }, term ? `${label} (${term})` : label)));
    format.value = input.format || "";
    const flag = (key, label) => {
      const box = el("input", { type: "checkbox", onchange: (event) => edit((found) => { found[key] = event.target.checked; }) });
      box.checked = Boolean(input[key]);
      return el("label", { class: "check" }, box, label);
    };
    const description = el("textarea", { rows: "2", onchange: (event) => edit((found) => { found.description = event.target.value.trim(); }) });
    description.value = input.description || "";
    const remove = () => change((model) => {
      model.inputs = model.inputs.filter((item) => item.id !== id);
      model.edges = model.edges.filter((edge) => edge.from.node !== id);
      state.selected = null;
    });
    return el("div", {},
      el("div", { class: "tools" }, el("h2", { class: "grow" }, "Workflow input"),
        el("button", { class: "danger", onclick: remove }, "Remove")),
      field("Name", name),
      field("Kind", type),
      input.type === "file" ? field("EDAM format", format) : null,
      el("div", { class: "tools" }, flag("list", "a list"), flag("optional", "optional")),
      field("Description", description),
      el("p", { class: "quiet" }, "Click its port, then an input of a step, to wire it."));
  }

  function toolPart(key) {
    const [tool, version] = key.split("\u0000");
    const part = el("div", {}, el("h2", {}, tool), el("p", { class: "quiet" }, "Loading the tool..."));
    definitionOf(tool, version).then((answer) => {
      const definition = answer.definition;
      const port = (item, formats) => el("li", {}, el("span", { class: "mono" }, item.name), " ",
        formats.map((format) => el("span", { class: "chip format", title: format }, context.label(format))),
        item.required === false ? el("span", { class: "quiet" }, " optional") : null);
      const outputs = (definition.outputs || []).map((item) => port(item, item.format && item.format.terms ? item.format.terms : []));
      const properties = Object.keys((definition.parameters && definition.parameters.properties) || {});
      const add = () => {
        const summary = (state.tools || []).find((item) => item.id === tool && item.version === version);
        context.addTool(summary || summaryFromDefinition(definition, answer.ecosystem));
      };
      const citations = (definition.citations || []).map((item) => item.doi || item.text);
      part.replaceChildren(...present(
        el("div", { class: "tools" }, el("h2", { class: "grow" }, definition.title || definition.id),
          el("button", { class: "primary", onclick: add }, "+ Add")),
        definition.description ? el("p", {}, definition.description) : null,
        el("dl", { class: "facts" },
          fact("Version", definition.version, "mono"),
          fact("Kind", answer.ecosystem),
          definition.image ? fact("Image", definition.image.name, "mono") : null,
          answer.origin ? fact("File", answer.origin, "mono") : null),
        el("h3", {}, "Inputs"), el("ul", { class: "ports" }, (definition.inputs || []).map((item) => port(item, item.formats || []))),
        el("h3", {}, "Outputs"), el("ul", { class: "ports" }, outputs),
        properties.length ? el("p", { class: "quiet" }, `Parameters: ${properties.join(", ")}`) : null,
        citations.length ? el("p", { class: "quiet" }, `Cite: ${citations.join("; ")}`) : null));
    }).catch((error) => part.append(el("p", { class: "error" }, error.message)));
    return part;
  }

  // Every finding of the check, by step and wire; a click selects what it is about.
  function findings() {
    const answer = state.report;
    const title = el("h3", {}, "Check");
    if (!state.model.nodes.length) return [title, el("p", { class: "quiet" }, "Add a tool to start.")];
    if (!answer) return [title, el("p", { class: "quiet" }, "Checking...")];
    if (answer.error) return [title, el("p", { class: "error" }, answer.error)];
    const items = [];
    for (const node of answer.report.nodes) {
      for (const finding of node.findings) {
        items.push(el("li", { class: `finding fit-${finding.fit}`, onclick: () => select("node", node.id) },
          el("strong", { class: "mono" }, node.id), `: ${finding.reason}`));
      }
    }
    answer.report.edges.forEach((check, at) => {
      const ends = answer.edges[at];
      const index = state.model.edges.findIndex((edge) => sameWire(edge, ends));
      for (const finding of check.findings) {
        items.push(el("li", { class: `finding fit-${finding.fit}`, onclick: () => index >= 0 && select("edge", index) },
          el("strong", { class: "mono" }, `${ends.from.node}/${ends.from.port} → ${ends.to.node}/${ends.to.port}`), `: ${finding.reason}`));
      }
    });
    return [title, el("p", {}, badge(fitState(answer.report.fit), `the graph ${fitText(answer.report.fit)}`)),
      items.length ? el("ul", { class: "findings" }, items) : el("p", { class: "quiet" }, "No findings.")];
  }

  function renameNode(old, next) {
    const id = next.replace(/[^A-Za-z0-9_.-]/g, "_");
    if (!id || id === old || state.model.nodes.some((item) => item.id === id)) {
      context.redraw();
      return;
    }
    change((model) => {
      const node = model.nodes.find((item) => item.id === old);
      node.id = id;
      for (const edge of model.edges) {
        if (edge.from.node === old) edge.from.node = id;
        if (edge.to.node === old) edge.to.node = id;
      }
      for (const output of model.outputs) if (output.from.node === old) output.from.node = id;
      state.selected = { kind: "node", id };
    });
  }

  function removeNode(id) {
    change((model) => {
      model.nodes = model.nodes.filter((item) => item.id !== id);
      model.edges = model.edges.filter((edge) => edge.from.node !== id && edge.to.node !== id);
      model.outputs = model.outputs.filter((item) => item.from.node !== id);
      state.selected = null;
    });
  }

  return { draw };
}

// A labelled control of the inspector.
function field(label, control) {
  return el("label", { class: "field" }, el("span", { class: "field-label" }, label), control);
}

// One fact of a definition list, like the version of a tool.
function fact(term, value, kind) {
  return el("div", {}, el("dt", {}, term), el("dd", kind ? { class: kind } : {}, value));
}
