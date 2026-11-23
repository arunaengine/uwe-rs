// The model of the combiner and its composed workflow: the cards and wires that the page edits,
// the JSON form of the `.uwe.yaml` file both ways, and the summaries of the tools on the cards.
// Copyright (c) 2026 The uwe Contributors
// SPDX-License-Identifier: MIT or Apache-2.0
"use strict";

// Common EDAM formats for the filters and for the inputs of a workflow.
const FORMATS = [["", "any format"], ["format_1930", "FASTQ"], ["format_1929", "FASTA"],
  ["format_2572", "BAM"], ["format_2573", "SAM"], ["format_3016", "VCF"], ["format_3003", "BED"],
  ["format_1975", "GFF3"], ["format_3475", "TSV"], ["format_3752", "CSV"], ["format_2330", "Text"],
  ["format_3464", "JSON"], ["format_2331", "HTML"]];
// Where the combiner keeps the workflow of this browser between visits.
const SAVED = "uwe.composer";

// `references` are the tool files that the workflow names, `embedded` the tools it holds.
function blankModel() {
  return { name: "workflow", description: "", nodes: [], inputs: [], edges: [], outputs: [], references: [], embedded: [] };
}

// The workflow that this browser kept, or a blank one.
function loadModel() {
  try {
    const saved = JSON.parse(localStorage.getItem(SAVED) || "null");
    if (saved && Array.isArray(saved.nodes)) return { ...blankModel(), ...saved };
  } catch (_) { /* nothing kept, or blocked */ }
  return blankModel();
}

function saveModel(model) {
  try { localStorage.setItem(SAVED, JSON.stringify(model)); } catch (_) { /* a private window keeps nothing */ }
}

function clone(value) {
  return JSON.parse(JSON.stringify(value));
}

// Whether a wire runs between these two ends.
function sameWire(edge, other) {
  return edge.from.node === other.from.node && edge.from.port === other.from.port
    && edge.to.node === other.to.node && edge.to.port === other.to.port;
}

// The JSON form of the `.uwe.yaml` file of the model.
function toWorkflow(model) {
  const inputOf = (id) => model.inputs.find((input) => input.id === id);
  const steps = {};
  for (const node of model.nodes) {
    const wired = {};
    for (const edge of model.edges.filter((item) => item.to.node === node.id)) {
      const input = inputOf(edge.from.node);
      const from = input ? input.name : `${edge.from.node}/${edge.from.port}`;
      wired[edge.to.port] = !input && edge.asserts && edge.asserts.length ? { from, asserts: edge.asserts } : from;
    }
    const step = { tool: node.tool, version: node.version };
    if (Object.keys(wired).length) step.in = wired;
    if (node.parameters && Object.keys(node.parameters).length) step.parameters = node.parameters;
    steps[node.id] = step;
  }
  const workflow = { uwe_version: "1", name: model.name || "workflow" };
  if (model.description) workflow.description = model.description;
  const tools = [...(model.references || []), ...(model.embedded || [])];
  if (tools.length) workflow.tools = tools;
  if (model.inputs.length) workflow.inputs = Object.fromEntries(model.inputs.map((input) => [input.name, inputSpec(input)]));
  workflow.steps = steps;
  if (model.outputs.length) {
    // An output keeps what the combiner does not edit, like its description.
    workflow.outputs = Object.fromEntries(model.outputs.map((output) =>
      [output.name, { ...(output.spec || {}), from: `${output.from.node}/${output.from.port}` }]));
  }
  return workflow;
}

function inputSpec(input) {
  const spec = { type: input.type };
  if (input.list) spec.list = true;
  if (input.format && input.type === "file") spec.format = input.format;
  if (input.description) spec.description = input.description;
  if (input.optional) spec.required = false;
  return spec;
}

// The model of a composed workflow in its JSON form, with the ports of each tool from the
// library, laid out in columns.
async function fromWorkflow(workflow) {
  const model = blankModel();
  model.name = workflow.name || "workflow";
  model.description = workflow.description || "";
  model.references = (workflow.tools || []).filter((item) => typeof item === "string");
  model.embedded = (workflow.tools || []).filter((item) => item && typeof item === "object");
  for (const [name, spec] of Object.entries(workflow.inputs || {})) {
    model.inputs.push({ kind: "input", id: `input:${name}`, name, type: spec.type || "file", list: Boolean(spec.list),
      format: spec.format || (spec.formats || [])[0] || "", description: spec.description || "",
      optional: spec.required === false, x: 0, y: 0 });
  }
  const checked = await send("/api/composer/check", { workflow }).catch(() => ({ versions: {} }));
  for (const [id, step] of Object.entries(workflow.steps || {})) {
    const version = step.version || checked.versions[id] || "";
    model.nodes.push({ id, tool: step.tool, version, x: 0, y: 0, parameters: step.parameters || {},
      summary: await stepSummary(step.tool, version) });
    for (const [port, source] of Object.entries(step.in || {})) {
      const from = typeof source === "string" ? source : source.from;
      const asserts = typeof source === "string" ? [] : source.asserts || [];
      const [node, output] = from.includes("/") ? from.split("/", 2) : [`input:${from}`, from];
      model.edges.push({ from: { node, port: output }, to: { node: id, port }, asserts });
    }
  }
  for (const [name, spec] of Object.entries(workflow.outputs || {})) {
    const from = typeof spec === "string" ? spec : spec.from;
    const [node, port] = from.split("/", 2);
    const kept = typeof spec === "string" ? {} : { ...spec };
    delete kept.from;
    model.outputs.push({ name, from: { node, port }, spec: kept });
  }
  placeColumns(model);
  return model;
}

// The summary of the tool of a step; the check reports a tool that the library lacks.
async function stepSummary(tool, version) {
  try {
    const answer = await json(`/api/tools/${encodeURIComponent(tool)}/${encodeURIComponent(version)}`);
    return summaryFromDefinition(answer.definition, answer.ecosystem);
  } catch (_) {
    return { title: tool, ecosystem: "", inputs: [], outputs: [] };
  }
}

function placeColumns(model) {
  model.inputs.forEach((input, row) => { input.x = 40; input.y = 40 + row * 120; });
  model.nodes.forEach((node, index) => { node.x = 40 + (index + 1) * (CARD_WIDTH + 120); node.y = 40; });
}

// Lays the cards out in columns by their depth in the graph: inputs first, then each step
// one column after the last step it reads from.
function layout(model) {
  const depth = new Map(model.inputs.map((input) => [input.id, 0]));
  const ids = model.nodes.map((node) => node.id);
  for (let round = 0; round <= ids.length; round += 1) {
    for (const id of ids) {
      const from = model.edges.filter((edge) => edge.to.node === id).map((edge) => depth.get(edge.from.node));
      depth.set(id, 1 + Math.max(0, ...from.filter((item) => item !== undefined)));
    }
  }
  const columns = new Map();
  for (const card of [...model.inputs, ...model.nodes]) {
    const column = depth.get(card.id) || 0;
    const placed = columns.get(column) || 0;
    card.x = 40 + column * (CARD_WIDTH + 120);
    card.y = 40 + placed;
    columns.set(column, placed + cardHeight(card) + 40);
  }
}

// The ports and facts of a tool that the canvas draws, from a summary of the tool query.
function summaryOf(tool) {
  return { title: tool.title, ecosystem: tool.ecosystem, image: tool.image, inputs: tool.inputs, outputs: tool.outputs };
}

function summaryFromDefinition(definition, ecosystem) {
  return {
    title: definition.title,
    ecosystem,
    image: definition.image ? definition.image.name : null,
    inputs: (definition.inputs || []).map((port) => ({ name: port.name, kind: port.kind, cardinality: port.cardinality || "one",
      formats: port.formats || [], required: port.required !== false })),
    outputs: (definition.outputs || []).map((port) => ({ name: port.name, kind: port.kind, cardinality: port.cardinality || "one",
      formats: port.format && port.format.terms ? port.format.terms : [] })),
    id: definition.id,
    version: definition.version,
  };
}

// What a workflow input delivers, as the suggestion of its followers takes it.
function supplyOf(input) {
  const kind = input.type === "file" || input.type === "directory" ? input.type : { value: input.type };
  return { kind, shape: { cardinality: input.list ? "list" : "one" }, formats: input.format ? [input.format] : [] };
}

// The short name of an EDAM term: its label among the common formats, else its id.
function labelOf(term) {
  const known = FORMATS.find(([id]) => id === term);
  return known ? known[1] : String(term).split("/").pop();
}
