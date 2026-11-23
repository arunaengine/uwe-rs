// A form for a JSON Schema of inputs or tool parameters: texts, numbers, booleans, choices,
// lists, nested objects, files and the `if`/`then` branches of conditionals.
// Copyright (c) 2026 The uwe Contributors
// SPDX-License-Identifier: MIT or Apache-2.0
"use strict";

// Builds the form of `schema` with `values` filled in. `options.onChange` hears every change.
// It returns the node, `value()` for the values as typed JSON, and `showErrors(diagnostics)`.
function schemaForm(schema, values, options) {
  const settings = options || {};
  const definitions = { ...(schema.definitions || {}), ...(schema.$defs || {}) };
  const slots = new Map();
  const changed = () => { if (settings.onChange) settings.onChange(); };

  // A schema with its `$ref` followed and its nullable union narrowed to the other type.
  function resolved(part) {
    let found = part || {};
    for (let depth = 0; found.$ref && depth < 16; depth += 1) {
      const { $ref: reference, ...rest } = found;
      found = { ...(definitions[reference.split("/").pop()] || {}), ...rest };
    }
    const union = found.anyOf || found.oneOf;
    if (union && !found.type && !found.properties) {
      const member = union.map(resolved).find((item) => item.type !== "null");
      if (member) found = { ...member, ...without(found, ["anyOf", "oneOf"]) };
    }
    return found;
  }

  function kind(part) {
    if (part.enum) return "choice";
    let type = Array.isArray(part.type) ? part.type.find((item) => item !== "null") : part.type;
    if (!type && part.properties) type = "object";
    if (type === "string" && part.format === "file-path") return "file";
    if (type === "string" && part.format === "directory-path") return "directory";
    return type || "any";
  }

  // The slot of the messages of one value, by its JSON pointer.
  function slot(pointer) {
    const node = el("div", { class: "field-error", hidden: true });
    slots.set(pointer, node);
    return node;
  }

  function labelOf(name, part, required) {
    return el("div", { class: "field-label" },
      el("span", { class: "mono" }, part.title || name),
      required ? el("span", { class: "chip required" }, "required") : null,
      part.fileFormat && !part.listed ? formats(part.fileFormat) : null);
  }

  function formats(list) {
    const items = Array.isArray(list) ? list : [list];
    return items.map((format) => el("span", { class: "chip format" }, String(format).split("/").pop()));
  }

  // One value: it returns its node and a function that reads its value, `undefined` when unset.
  function field(part, value, pointer, name, required) {
    const schemaPart = resolved(part);
    const type = kind(schemaPart);
    const given = value !== undefined ? value : schemaPart.default;
    if (type === "object") return objectField(schemaPart, given, pointer, name, required);
    if (type === "array") return arrayField(schemaPart, given, pointer, name, required);
    const help = schemaPart.description ? el("div", { class: "quiet field-help" }, schemaPart.description) : null;
    let input;
    let read;
    if (type === "boolean") {
      let touched = given !== undefined;
      input = el("input", { type: "checkbox", onchange: () => { touched = true; changed(); } });
      input.checked = given === true;
      read = () => (touched || required ? input.checked : undefined);
    } else if (type === "choice") {
      input = el("select", { onchange: changed },
        required ? null : el("option", { value: "" }, ""),
        schemaPart.enum.map((choice, index) => el("option", { value: String(index) }, String(choice))));
      const at = schemaPart.enum.findIndex((choice) => choice === given);
      input.value = at >= 0 ? String(at) : "";
      read = () => (input.value === "" ? undefined : schemaPart.enum[Number(input.value)]);
    } else {
      // An integer is a text field, so that a typed `3.5` stays visible with its error.
      const integer = type === "integer";
      input = el("input", {
        type: type === "number" ? "number" : "text",
        inputmode: integer ? "numeric" : undefined,
        step: type === "number" ? "any" : undefined,
        class: type === "file" || type === "directory" ? "mono" : undefined,
        placeholder: type === "file" ? "path or address of a file"
          : type === "directory" ? "path or address of a folder" : undefined,
        oninput: () => { if (integer) flagInteger(input, messages); changed(); },
      });
      if (given !== undefined && given !== null) {
        input.value = typeof given === "object" ? JSON.stringify(given) : String(given);
      }
      read = () => {
        const text = input.value;
        if (text === "") return undefined;
        // Text that is no whole number goes as typed, so the check refuses it, never cut.
        if (integer) return wholeNumber(text.trim()) ? Number(text.trim()) : text;
        if (type === "number") return Number(text);
        if (type === "any") { try { return JSON.parse(text); } catch (_) { return text; } }
        return text;
      };
    }
    const messages = slot(pointer);
    if (type === "integer") flagInteger(input, messages);
    const node = el("label", { class: `field field-${type}` }, labelOf(name, schemaPart, required), input, help, messages);
    return { node, read };
  }

  function objectField(part, value, pointer, name, required) {
    const current = value && typeof value === "object" ? value : {};
    const children = new Map();
    const body = el("div", { class: "field-body" });
    const requiredNames = new Set(part.required || []);
    for (const [key, item] of Object.entries(part.properties || {})) {
      const child = field(item, current[key], `${pointer}/${key}`, key, requiredNames.has(key));
      children.set(key, child);
      body.append(child.node);
    }
    // Conditionals show the fields of the branch that the values choose, as Galaxy tools have them.
    const conditions = [part, ...(part.allOf || []).map(resolved)].filter((item) => item.if);
    const branches = conditions.map((condition) => ({ condition, node: el("div", { class: "branch" }), shown: null, fields: new Map() }));
    for (const branch of branches) body.append(branch.node);
    const read = () => {
      const result = {};
      for (const [key, child] of children) {
        const item = child.read();
        if (item !== undefined) result[key] = item;
      }
      for (const branch of branches) {
        for (const [key, child] of branch.fields) {
          const item = child.read();
          if (item !== undefined) result[key] = item;
        }
      }
      return Object.keys(result).length || required ? result : undefined;
    };
    const refresh = () => {
      const now = read() || {};
      for (const branch of branches) {
        const chosen = holds(branch.condition.if, now) ? "then" : "else";
        if (branch.shown === chosen) continue;
        branch.shown = chosen;
        const target = resolved(branch.condition[chosen]);
        const names = new Set(target.required || []);
        branch.fields = new Map();
        const nodes = [];
        for (const [key, item] of Object.entries(target.properties || {})) {
          if (children.has(key)) continue;
          const child = field(item, current[key], `${pointer}/${key}`, key, names.has(key));
          branch.fields.set(key, child);
          nodes.push(child.node);
        }
        branch.node.replaceChildren(...nodes);
      }
    };
    body.addEventListener("input", refresh);
    body.addEventListener("change", refresh);
    refresh();
    if (!pointer) return { node: el("div", { class: "schema-form" }, body, slot("")), read };
    const node = el("fieldset", { class: "field-group" },
      el("legend", {}, labelOf(name, part, required)),
      part.description ? el("div", { class: "quiet field-help" }, part.description) : null,
      body, slot(pointer));
    return { node, read };
  }

  function arrayField(part, value, pointer, name, required) {
    // The rows of a list show no format of their own; the list shows it once.
    const items = { ...resolved(part.items || {}), listed: true };
    const rows = [];
    const list = el("div", { class: "field-rows" });
    const add = (item) => {
      const row = { child: null, node: null };
      const index = rows.length;
      row.child = field(items, item, `${pointer}/${index}`, `item ${index + 1}`, true);
      row.node = el("div", { class: "field-row" }, row.child.node,
        el("button", { type: "button", class: "quiet-button", title: "remove this item", onclick: () => {
          rows.splice(rows.indexOf(row), 1);
          renumber();
          changed();
        } }, "remove"));
      rows.push(row);
      list.append(row.node);
    };
    // Draws the rows again from their values, so that each row has the pointer of its place
    // and a message about row 2 lands at the row that is second now.
    const renumber = () => {
      const kept = rows.map((row) => row.child.read());
      for (const key of [...slots.keys()]) if (key.startsWith(`${pointer}/`)) slots.delete(key);
      rows.length = 0;
      list.replaceChildren();
      for (const item of kept) add(item);
    };
    for (const item of Array.isArray(value) ? value : []) add(item);
    const node = el("fieldset", { class: "field-group" },
      el("legend", {}, labelOf(name, part, required),
        !part.fileFormat && items.fileFormat ? formats(items.fileFormat) : null),
      part.description ? el("div", { class: "quiet field-help" }, part.description) : null,
      list,
      el("button", { type: "button", onclick: () => { add(undefined); changed(); } }, "add item"),
      slot(pointer));
    const read = () => {
      const result = rows.map((row) => row.child.read()).filter((item) => item !== undefined);
      return result.length || required ? result : undefined;
    };
    return { node, read };
  }

  const top = resolved(schema);
  const root = kind(top) === "object" || top.properties
    ? objectField(top, values || {}, "", "", true)
    : field(top, values, "", "value", true);

  function showErrors(diagnostics) {
    for (const node of slots.values()) { node.hidden = true; node.replaceChildren(); }
    for (const diagnostic of diagnostics || []) {
      const pointer = target(diagnostic);
      const node = slots.get(pointer) || slots.get("");
      if (!node) continue;
      node.hidden = false;
      node.append(el("div", { class: diagnostic.severity === "warning" ? "warning" : "error" }, diagnostic.message));
    }
  }

  // The slot of a diagnostic: its pointer, or for a missing value the pointer of that value.
  function target(diagnostic) {
    let pointer = diagnostic.instance_path || "";
    if (diagnostic.keyword === "required" && diagnostic.input) {
      const named = `/${String(diagnostic.input).replace(/\[(\d+)\]/g, ".$1").split(/[.|]/).join("/")}`;
      if (slots.has(named)) return named;
    }
    while (pointer && !slots.has(pointer)) pointer = pointer.slice(0, pointer.lastIndexOf("/"));
    return pointer;
  }

  return { node: root.node, value: () => root.read(), showErrors };
}

// Whether a text is a whole number that JSON keeps exactly, like `-12`; not `3.5` or `1e3`.
function wholeNumber(text) {
  return /^[+-]?\d+$/.test(text) && Number.isSafeInteger(Number(text));
}

// Marks an integer field whose text is no whole number, below the field, as the user types.
function flagInteger(input, messages) {
  const text = input.value.trim();
  const wrong = text !== "" && !wholeNumber(text);
  input.setAttribute("aria-invalid", String(wrong));
  messages.hidden = !wrong;
  messages.replaceChildren(...(wrong ? [el("div", { class: "error" }, `\`${text}\` is no whole number, like 3`)] : []));
}

// Whether `values` meet the `if` of a conditional: its `properties` with `const`, `enum` or
// `type`, and its `required` names.
function holds(condition, values) {
  if (!condition || typeof condition !== "object") return Boolean(condition);
  for (const name of condition.required || []) if (!(name in values)) return false;
  for (const [name, wanted] of Object.entries(condition.properties || {})) {
    if (!(name in values)) continue;
    const value = values[name];
    if ("const" in wanted && value !== wanted.const) return false;
    if (wanted.enum && !wanted.enum.includes(value)) return false;
    if (wanted.type === "boolean" && typeof value !== "boolean") return false;
  }
  return true;
}

function without(object, keys) {
  const copy = { ...object };
  for (const key of keys) delete copy[key];
  return copy;
}
