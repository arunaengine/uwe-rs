// The launcher of the studio: pick a workflow, read its form and support report, fill and
// check its inputs, choose where it runs, and start it.
// Copyright (c) 2026 The uwe Contributors
// SPDX-License-Identifier: MIT or Apache-2.0
"use strict";

// The names of the languages as the page shows them.
const LANGUAGE_NAMES = {
  nextflow: "Nextflow", snakemake: "Snakemake", cwl: "CWL", argo: "Argo", wdl: "WDL",
  galaxy: "Galaxy", wir: "wir", composed: "Composed",
};

async function showLaunch() {
  leave();
  trail.replaceChildren();
  document.title = "uwe: run a workflow";
  const here = { kind: "launch", form: null, described: null };
  view = here;
  await studioReady;
  if (view !== here) return;
  if (!studio.studio) {
    main.replaceChildren(el("div", { class: "card" }, el("h2", {}, "This page only shows runs"),
      el("p", { class: "quiet" }, "`uwe serve`, or `uwe page` with `--workflows`, starts runs from the page.")));
    return;
  }
  const parts = {
    pick: el("section", { class: "card" }),
    inputs: el("section", { class: "card", hidden: true }),
    run: el("section", { class: "card", hidden: true }),
  };
  here.parts = parts;
  main.replaceChildren(el("div", { class: "launch" }, parts.pick, parts.inputs, parts.run));
  drawPick(here);
}

// The choice of the workflow: those that the server found, its presets, or a path or address.
async function drawPick(here) {
  const target = el("input", { type: "text", class: "mono grow", placeholder: "path, folder or Git address of a workflow",
    "aria-label": "workflow" });
  const profile = el("input", { type: "text", placeholder: "profiles, like test,docker", "aria-label": "profiles" });
  const entry = el("input", { type: "text", placeholder: "entry or target", "aria-label": "entry" });
  const list = el("select", { "aria-label": "found workflows" }, el("option", { value: "" }, "Loading the workflows..."));
  here.pick = { target, profile, entry, preset: null };
  const load = () => describe(here);
  list.addEventListener("change", () => {
    const chosen = here.workflows[Number(list.value)];
    if (!chosen) return;
    here.pick.preset = chosen.preset || null;
    target.value = chosen.preset ? "" : chosen.path;
    target.placeholder = chosen.preset ? `preset ${chosen.preset}` : target.placeholder;
    load();
  });
  target.addEventListener("keydown", (event) => { if (event.key === "Enter") { here.pick.preset = null; load(); } });
  here.parts.pick.replaceChildren(
    el("h2", {}, "Workflow"),
    el("p", { class: "quiet" }, "Nextflow, Snakemake, CWL, Argo, WDL, Galaxy, composed `.uwe.yaml` and wir documents."),
    el("div", { class: "tools" }, list),
    el("div", { class: "tools" }, target,
      el("button", { class: "primary", onclick: () => { here.pick.preset = null; load(); } }, "Load")),
    el("div", { class: "tools" }, profile, entry));
  try {
    const found = await json("/api/workflows");
    here.workflows = found.workflows;
    const options = found.workflows.map((workflow, index) => el("option", { value: String(index) },
      `${workflow.preset ? "preset " : ""}${workflow.name} (${LANGUAGE_NAMES[workflow.language] || workflow.language || "?"})`));
    list.replaceChildren(el("option", { value: "" },
      found.workflows.length ? `${found.workflows.length} workflows found` : "No workflows found; give a path below"),
    ...options);
  } catch (error) {
    list.replaceChildren(el("option", { value: "" }, `The workflows cannot be listed: ${error.message}`));
  }
}

// The request that names the chosen workflow, as the describe, validate and run routes take it.
function chosenWorkflow(here) {
  const pick = here.pick;
  const request = {};
  if (pick.preset) request.preset = pick.preset;
  else request.target = pick.target.value.trim();
  if (pick.profile.value.trim()) request.profile = pick.profile.value.trim();
  if (pick.entry.value.trim()) request.entry = pick.entry.value.trim();
  return request;
}

// The values that a run sets. A preset fixes some inputs: only those that it lets a run set
// and that differ from its own go, so that a fixed input is never sent as a change.
function chosenValues(here) {
  const values = here.form ? here.form.value() || {} : {};
  const editable = here.described && here.described.editable;
  if (!Array.isArray(editable)) return values;
  const own = (here.described.form && here.described.form.values) || {};
  return Object.fromEntries(Object.entries(values).filter(([name, value]) =>
    editable.includes(name) && JSON.stringify(value) !== JSON.stringify(own[name])));
}

async function describe(here) {
  const request = chosenWorkflow(here);
  if (!request.target && !request.preset) return;
  // Only the answer for the latest choice counts, also when an earlier one comes later.
  const asked = here.asked = (here.asked || 0) + 1;
  const parts = here.parts;
  parts.inputs.hidden = false;
  parts.run.hidden = true;
  parts.inputs.replaceChildren(el("p", { class: "quiet" }, "Reading the workflow..."));
  let described;
  try {
    described = await send("/api/workflows/describe", request);
  } catch (error) {
    if (view !== here || asked !== here.asked) return;
    parts.inputs.replaceChildren(el("h2", {}, "Inputs"), el("pre", { class: "error" }, error.message));
    return;
  }
  if (view !== here || asked !== here.asked) return;
  here.described = described;
  here.request = request;
  const values = described.form ? described.form.values : {};
  here.form = described.form ? schemaForm(described.form.schema, values, {}) : null;
  const errors = el("div", {});
  const check = el("button", { onclick: () => validate(here, errors) }, "Check inputs");
  parts.inputs.replaceChildren(...present(
    el("div", { class: "tools" }, el("h2", { class: "grow" }, "Inputs"),
      el("span", { class: "chip" }, LANGUAGE_NAMES[described.language] || described.language)),
    supportPart(described),
    here.form ? here.form.node : el("p", { class: "quiet" }, described.form_error || "The workflow names no inputs."),
    errors,
    here.form ? el("div", { class: "tools" }, check) : null));
  drawRun(here);
}

// What uwe can run of the workflow: compiled, native and refused parts, and the problems found.
function supportPart(described) {
  const report = described.support;
  if (!report) {
    return el("div", { class: "notice" }, el("strong", {}, "Support not known. "), described.support_error || "");
  }
  const counts = { compiled: 0, native: 0, unsupported: 0 };
  for (const part of report.parts) counts[part.support] = (counts[part.support] || 0) + 1;
  const refused = counts.unsupported > 0 || (report.problems || []).some((problem) => problem.kind === "unsupported");
  const summary = [
    counts.compiled ? `${counts.compiled} compiled into wir` : "",
    counts.native ? `${counts.native} run natively` : "",
    counts.unsupported ? `${counts.unsupported} refused` : "",
  ].filter(Boolean).join(", ");
  const rows = report.parts.map((part) => el("tr", {},
    el("td", { class: "mono" }, part.name),
    el("td", {}, badge(part.support === "unsupported" ? "failed" : part.support === "native" ? "reused" : "succeeded", part.support)),
    el("td", { class: "quiet" }, part.reason || ""),
    el("td", { class: "mono" }, (part.images || []).join(", "))));
  // Errors, like a missing input, depend on the values; the check of the inputs tells them.
  const problems = (report.problems || []).filter((problem) => problem.kind !== "error").map((problem) =>
    el("li", { class: problem.kind === "unsupported" ? "error" : "warning" }, `${problem.kind}: ${problem.message}`));
  return el("div", { class: refused ? "notice support refused" : "notice support" },
    el("strong", {}, refused ? "uwe refuses a part of this workflow. " : "uwe runs this workflow. "),
    `${report.parts.length} parts${summary ? `: ${summary}` : ""}.`,
    el("details", {}, el("summary", {}, "parts and images"),
      el("table", {}, el("tbody", {}, rows))),
    problems.length ? el("ul", {}, problems) : null);
}

async function validate(here, errors) {
  if (!here.form) return true;
  const request = { ...here.request, values: chosenValues(here) };
  // The run waits for the latest check, and an earlier check that answers late says nothing.
  const asked = here.checks = (here.checks || 0) + 1;
  const form = here.form;
  if (here.start) here.start.disabled = true;
  let answer;
  let failed = null;
  try {
    answer = await send("/api/workflows/validate", request);
  } catch (error) {
    failed = error;
  }
  if (view !== here || asked !== here.checks || form !== here.form) return false;
  if (here.start) here.start.disabled = Boolean(here.starting);
  if (failed) {
    errors.replaceChildren(el("pre", { class: "error" }, failed.message));
    return false;
  }
  here.form.showErrors(answer.diagnostics || []);
  const count = (answer.diagnostics || []).filter((item) => item.severity === "error").length;
  errors.replaceChildren(answer.accepted
    ? el("p", { class: "state succeeded" }, "The inputs fit the form.")
    : el("p", { class: "error" }, answer.message || `${count} inputs do not fit; each says why below its field.`));
  return answer.accepted;
}

// Where and how the workflow runs, and the button that starts it.
function drawRun(here) {
  const executor = el("select", { "aria-label": "executor" },
    studio.executors.map((name) => el("option", { value: name }, name)));
  // The executor that the run takes without one, like that of a preset, is chosen first.
  const preferred = here.described && here.described.executor;
  executor.value = studio.executors.includes(preferred) ? preferred
    : studio.executors.includes("local") ? "local" : studio.executors[0];
  const flag = (text) => {
    const box = el("input", { type: "checkbox" });
    return { box, node: el("label", { class: "check" }, box, text) };
  };
  const resume = flag("resume the last run");
  const offline = flag("no network");
  const fresh = flag("no cache");
  const image = el("input", { type: "text", class: "mono", placeholder: "default image, like debian:stable-slim", "aria-label": "default image" });
  const output = el("input", { type: "text", class: "mono", placeholder: "output folder", "aria-label": "output folder" });
  const said = el("div", {});
  const start = el("button", { class: "primary", onclick: async () => {
    start.disabled = true;
    here.starting = true;
    said.replaceChildren(el("p", { class: "quiet" }, "Starting the run..."));
    const request = { ...here.request };
    if (executor.value !== preferred) request.executor = executor.value;
    if (here.form) request.values = chosenValues(here);
    if (resume.box.checked) request.resume = true;
    if (offline.box.checked) request.no_network = true;
    if (fresh.box.checked) request.no_cache = true;
    if (image.value.trim()) request.default_image = image.value.trim();
    if (output.value.trim()) request.output_dir = output.value.trim();
    try {
      const answer = await send("/api/runs", request);
      go(`/runs/${encodeURIComponent(answer.run_id)}`);
    } catch (error) {
      here.starting = false;
      start.disabled = false;
      said.replaceChildren(el("pre", { class: "error" }, error.message));
    }
  } }, "Run workflow");
  here.start = start;
  here.parts.run.hidden = false;
  here.parts.run.replaceChildren(
    el("h2", {}, "Run"),
    el("div", { class: "tools" }, el("span", { class: "quiet" }, "Executor"), executor,
      resume.node, offline.node, fresh.node),
    el("div", { class: "tools" }, image, output),
    el("div", { class: "tools" }, start),
    said);
}
