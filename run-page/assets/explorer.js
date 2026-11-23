// The task explorer of the combiner: searching the tool library by words, ecosystem and
// formats, the cards of the tools found, and scanning a folder for more tools.
// Copyright (c) 2026 The uwe Contributors
// SPDX-License-Identifier: MIT or Apache-2.0
"use strict";

// The ecosystems of the explorer, by the `kind` that the tool query takes.
const ECOSYSTEMS = [["all", "All"], ["galaxy", "Galaxy"], ["cwl", "CWL"], ["wdl", "WDL"],
  ["nextflow", "Nextflow"], ["snakemake", "Snakemake"], ["argo", "Argo"], ["recorded", "Recorded"]];

// The explorer draws into `context.parts.explorer`; a click on a tool selects it, and its
// button adds it to the canvas. It returns `search()`, which asks the library again.
function makeExplorer(context) {
  const { state, parts } = context;
  let searching = null;

  async function search() {
    const params = new URLSearchParams();
    for (const [key, value] of Object.entries(state.query)) if (value && value !== "all") params.set(key, value);
    let found;
    try {
      found = await json(`/api/tools?${params}`);
      Object.assign(state.labels, found.labels || {});
      state.tools = found.tools;
    } catch (error) {
      found = { error: error.message };
    }
    if (view && view.kind === "compose") draw(found);
  }

  function draw(found) {
    const query = state.query;
    const text = el("input", { type: "search", placeholder: "Search tools", value: query.q, "aria-label": "search tools",
      oninput: (event) => { query.q = event.target.value; clearTimeout(searching); searching = setTimeout(search, 250); } });
    parts.explorer.replaceChildren(...present(
      el("h2", {}, "Tasks"),
      text,
      pills(found),
      el("div", { class: "tools" }, filter("input_format", "takes"), filter("output_format", "gives")),
      list(found),
      skipped(found),
      ...scanner()));
    if (document.activeElement === document.body && query.q) text.focus();
  }

  // One pill per ecosystem, with how many tools of it the library holds.
  function pills(found) {
    const counts = (found && found.facets && found.facets.ecosystems) || {};
    return el("div", { class: "pills" }, ECOSYSTEMS.map(([kind, label]) => el("button", {
      "aria-pressed": String(state.query.kind === kind),
      onclick: () => { state.query.kind = kind; search(); },
    }, label, kind === "all" ? "" : ` ${counts[kind] || 0}`)));
  }

  function filter(key, label) {
    const choice = el("select", { "aria-label": label, onchange: (event) => { state.query[key] = event.target.value; search(); } },
      FORMATS.map(([term, name]) => el("option", { value: term }, `${label} ${name}`)));
    choice.value = state.query[key];
    return choice;
  }

  function list(found) {
    const tools = el("div", { class: "tool-list" });
    if (!found) tools.append(el("p", { class: "quiet" }, "Loading the tools..."));
    else if (found.error) tools.append(el("p", { class: "error" }, found.error));
    else if (!found.tools.length) {
      tools.append(el("p", { class: "quiet" }, found.count ? "No tool matches."
        : "The library has no tools. Scan a folder of tools, or start `uwe serve --tools <folder>`."));
    } else tools.append(...found.tools.map(toolCard));
    return tools;
  }

  function skipped(found) {
    if (!found || !found.skipped || !found.skipped.length) return null;
    return el("details", {}, el("summary", {}, `${found.skipped.length} files held no tool`),
      el("ul", { class: "skipped" }, found.skipped.map((item) =>
        el("li", {}, el("span", { class: "mono" }, base(item.path)), `: ${item.reason}`))));
  }

  // The field and button that add the tools of a folder, and what the last scan found.
  function scanner() {
    const path = el("input", { type: "text", class: "mono", placeholder: "folder or file of tools", "aria-label": "folder of tools" });
    const said = el("div", { class: "quiet" });
    const scan = el("button", { onclick: async () => {
      if (!path.value.trim()) return;
      scan.disabled = true;
      try {
        const answer = await send("/api/tools/scan", { path: path.value.trim() });
        said.replaceChildren(`${answer.added.length} tools added, ${answer.skipped.length} files skipped; ${answer.count} in the library.`);
        search();
      } catch (error) {
        said.replaceChildren(el("span", { class: "error" }, error.message));
      }
      scan.disabled = false;
    } }, "Scan folder");
    return [el("div", { class: "scan" }, path, scan), said];
  }

  function toolCard(tool) {
    const formats = new Set();
    for (const port of [...tool.inputs, ...tool.outputs]) for (const format of port.formats || []) formats.add(format);
    const add = el("button", { class: "primary small", title: "add to the canvas",
      onclick: (event) => { event.stopPropagation(); context.addTool(tool); } }, "+ Add");
    return el("div", { class: "tool-card", onclick: () => context.select("tool", `${tool.id}\u0000${tool.version}`) },
      el("div", { class: "tool-card-head" }, el("strong", {}, tool.title || tool.id), add),
      el("div", { class: "quiet mono" }, `${tool.id} ${tool.version}`),
      el("div", { class: "tags" },
        el("span", { class: `chip eco ${tool.ecosystem}` }, tool.ecosystem),
        el("span", { class: `chip curation ${tool.curation}` }, tool.curation),
        [...formats].slice(0, 4).map((format) => el("span", { class: "chip format", title: format }, context.label(format))),
        el("span", { class: "quiet" }, `${tool.inputs.length} in, ${tool.outputs.length} out`)));
  }

  return { search };
}
