// An endpoint: method, path, command name, parameters and body. The parameters in braces
// in the path appear and disappear with it. A parameter that comes from an OpenAPI keeps
// its schema while the type stays the same; the whole body keeps its own while it stays JSON
// without fields. OK sends the endpoint back to the APIs window (editors.rs) and closes.
"use strict";

(async () => {
  const { invoke, el } = Clak;
  const E = Editors;
  const $ = (id) => document.getElementById(id);

  const draft = await invoke("endpoint_draft");
  if (!draft) {
    invoke("editor_close");
    return;
  }
  const original = draft.endpoint;
  const originalWhole = !!original && original.body === "json" && !(original.parameters || []).some((p) => p.in === "body");
  /// The parameters while they are being written; `orig` is what was there, with the OpenAPI schema.
  let params = [];
  let check = {};
  let checking = 0;

  const edit = (orig, name = "", place = "query") => ({
    orig,
    name: orig ? orig.name : name,
    in: orig ? orig.in : place,
    type: orig ? orig.type : "string",
    required: orig ? !!orig.required : place === "path",
    description: orig ? orig.description || "" : "",
  });

  /// The automatic name (ApiEndpoint::name_from): GET /users/{id} → get_users_id.
  const nameFrom = (method, path) => {
    const parts = [method.toLowerCase()];
    for (const part of path.split("/").filter(Boolean)) {
      const mapped = [...part].map((c) => (/^[A-Za-z0-9]$/.test(c) ? c.toLowerCase() : "_".repeat(c.length))).join("");
      const trimmed = mapped.replace(/^_+|_+$/g, "");
      if (trimmed) parts.push(trimmed);
    }
    let name = parts.join("_").replace(/_{2,}/g, "_");
    if (name.length > 64) name = name.slice(0, 64).replace(/_+$/, "");
    return name;
  };

  for (const m of draft.methods) $("method").append(el("option", { value: m }, m));
  $("method").value = original ? original.method : "GET";
  $("path").value = original ? original.path : "/";
  // A name equal to the automatic one stays automatic: it follows method and path.
  $("name").value = original && original.name !== nameFrom(original.method, original.path) ? original.name : "";
  $("description").value = original ? original.description || "" : "";
  params = (original ? original.parameters || [] : []).map((p) => edit(p));
  $("body").value = original && original.body ? original.body : "";
  $("body-required").checked = !!(original && original.body_required);
  $("add").append(E.glyph("plus", null, 12), "Parameter");

  /// The OpenAPI schema counts while the type is the same: a new type makes a simple one.
  const toParameter = (p) => {
    const name = p.name.trim();
    const description = p.description.trim() || null;
    const required = p.required || p.in === "path";
    return p.orig && p.orig.type === p.type
      ? { ...p.orig, name, in: p.in, required, description }
      : { name, in: p.in, type: p.type, required, description, default: null, choices: null, schema: null };
  };

  const endpoint = () => {
    const parameters = params.filter((p) => p.name.trim()).map(toParameter);
    const body = $("body").value || null;
    const fields = parameters.some((p) => p.in === "body");
    // The schema of the whole body counts while the body is the same.
    const schema = body === "json" && originalWhole && !fields ? original.body_schema : null;
    return {
      name: $("name").value.trim(),
      method: $("method").value,
      path: $("path").value.trim(),
      description: $("description").value.trim() || null,
      parameters,
      body,
      body_schema: schema || null,
      body_required: body !== null && $("body-required").checked,
    };
  };

  const fail = (message) => {
    $("error-text").textContent = message;
    $("error").classList.toggle("hidden", !message);
  };

  const select = (options, value, label, onchange) => {
    const s = el("select", { "aria-label": label });
    for (const o of options) s.append(el("option", { value: o }, o));
    s.value = value;
    s.addEventListener("change", () => onchange(s.value));
    return s;
  };

  const drawParams = () => {
    $("params").replaceChildren(...params.map((p) => {
      const name = el("input", { type: "text", class: "mono", maxlength: "128", spellcheck: "false", "aria-label": "Parameter name" });
      name.value = p.name;
      name.addEventListener("input", () => {
        p.name = name.value;
        changed();
      });
      const required = el("input", { type: "checkbox", "aria-label": "Required" });
      required.checked = p.required || p.in === "path";
      required.disabled = p.in === "path"; // a path parameter is always there: without it, the address doesn't exist
      required.addEventListener("change", () => {
        p.required = required.checked;
        changed();
      });
      const description = el("input", { type: "text", maxlength: "300", "aria-label": "Parameter description" });
      description.value = p.description;
      description.addEventListener("input", () => {
        p.description = description.value;
        changed();
      });
      const remove = el("button", { class: "ed-icon", title: "Remove", "aria-label": "Remove parameter", onclick: () => {
        params = params.filter((x) => x !== p);
        drawParams();
        changed();
      } });
      remove.innerHTML = E.icon("trash", 14);
      return el("div", { class: "ed-param" },
        name,
        select(draft.places, p.in, "In", (value) => {
          p.in = value;
          if (value === "path") p.required = true;
          drawParams();
          changed();
        }),
        select(draft.types, p.type, "Type", (value) => {
          p.type = value;
          changed();
        }),
        required, description, remove);
    }));
  };

  /// The parameters in braces follow the path: they appear and disappear with it.
  const followPath = () => {
    const path = $("path").value.trim();
    const names = [];
    const re = /\{([^{}]+)\}/g;
    for (let m; (m = re.exec(path)); ) names.push(m[1]);
    params = params.filter((p) => p.in !== "path" || names.includes(p.name.trim()));
    for (const name of names) {
      if (params.some((p) => p.in === "path" && p.name.trim() === name)) continue;
      const at = params.filter((p) => p.in === "path").length;
      params.splice(at, 0, edit(null, name, "path"));
    }
    drawParams();
  };

  const paint = () => {
    const e = endpoint();
    const whole = e.body === "json" && !e.parameters.some((p) => p.in === "body");
    let problem = check.problem;
    if (!problem && params.some((p) => !p.name.trim())) problem = "A parameter has no name.";
    $("name").placeholder = check.auto || "";
    $("heading").textContent = original ? `Edit “${original.name}”` : "Add an endpoint";
    $("no-params").classList.toggle("hidden", params.length > 0);
    $("params-head").classList.toggle("hidden", params.length === 0);
    $("body-required-row").classList.toggle("hidden", !whole);
    $("body-note").textContent = e.body === null
      ? (e.parameters.some((p) => p.in === "body") ? "Parameters “in body” need a body" : "")
      : whole ? "No fields: agents pass the whole JSON with --body" : "Fields: the parameters in body";
    $("save").textContent = original ? "OK" : "Add";
    $("save").disabled = !!problem || check.problem === undefined;
    $("save").title = problem || "";
    return problem;
  };

  const refresh = async () => {
    const mine = ++checking;
    const result = await invoke("endpoint_check", { endpoint: endpoint() });
    if (mine !== checking) return;
    check = result;
    paint();
  };

  const changed = () => {
    paint();
    refresh();
  };
  for (const id of ["name", "description"]) $(id).addEventListener("input", changed);
  for (const id of ["method", "body", "body-required"]) $(id).addEventListener("change", changed);
  $("path").addEventListener("input", () => {
    followPath();
    changed();
  });
  $("add").addEventListener("click", () => {
    params.push(edit(null, "", $("body").value ? "body" : "query"));
    drawParams();
    changed();
    const inputs = $("params").querySelectorAll(".ed-param input.mono");
    inputs[inputs.length - 1]?.focus();
  });

  const save = async () => {
    await refresh();
    const problem = paint();
    if (problem) {
      fail(problem);
      return;
    }
    try {
      await invoke("endpoint_done", { endpoint: endpoint() });
    } catch (error) {
      fail(E.message(error));
    }
  };
  $("save").addEventListener("click", save);
  $("cancel").addEventListener("click", () => invoke("editor_close"));
  document.addEventListener("keydown", (e) => {
    if (e.key === "Escape") invoke("editor_close");
    // OK is the default button.
    else if (e.key === "Enter" && e.target.tagName === "INPUT" && e.target.type !== "checkbox") {
      e.preventDefault();
      if (!$("save").disabled) save();
    }
  });

  drawParams();
  paint();
  await refresh();
  E.fitToContent();
  $("path").focus();
  $("path").setSelectionRange($("path").value.length, $("path").value.length);
})();
