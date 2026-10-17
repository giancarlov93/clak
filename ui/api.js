// Adding a REST API, or changing one. Two ways: from an OpenAPI document, which fills in
// everything and offers the endpoints to choose from, or by hand, one endpoint at a time (the
// endpoint dialog). In both, endpoints can be added, changed and removed before saving.
// With OAuth nothing is saved until the sign-in is finished. Checks and saving are done
// by the app (editors.rs); the saved secret doesn't come back here.
"use strict";

(async () => {
  const { invoke, el } = Clak;
  const E = Editors;
  const $ = (id) => document.getElementById(id);

  const draft = await invoke("api_draft");
  if (!draft) {
    invoke("editor_close");
    return;
  }
  const target = draft.target;
  /// The rows: the endpoint (the hub's JSON) and whether it becomes a command. `id` stays with the row.
  let rows = [];
  let nextId = 1;
  const row = (endpoint, include) => ({ id: nextId++, endpoint, include });
  let source = target ? target.source : null;
  let importedName = null;
  let importedUrl = null;
  let fromOpenApi = !target || !!target.source;
  let loading = false;
  let signingIn = false;
  let selected = null;
  let check = {};
  let checking = 0;

  const show = (id, on) => $(id).classList.toggle("hidden", !on);
  const busy = () => loading || signingIn;
  const kind = () => $("auth").value;
  const isOAuth = () => kind() === "oauth" || kind() === "oauth-app";

  const fillOAuth = (auth) => {
    $("authorize-url").value = auth.authorization_url || "";
    $("token-url").value = auth.token_url || "";
    $("scope").value = auth.scope || "";
    if (auth.client_id || !$("client-id").value) $("client-id").value = auth.client_id || "";
  };
  const selectAuth = (value) => {
    $("auth").value = value;
    if ($("auth").selectedIndex < 0) $("auth").value = "none";
  };

  $("from-openapi").append(E.glyph("link", null, 13), "From OpenAPI");
  $("manual").append(E.glyph("edit", null, 13), "Manual");
  $("browse").append(E.glyph("folder", null, 14));
  $("add-endpoint").append(E.glyph("plus", null, 12), "Endpoint…");

  if (target) {
    $("name").value = target.name;
    $("command").value = target.command || "";
    $("url").value = target.base_url;
    $("auth-name").value = target.auth.name || "";
    fillOAuth(target.auth);
    $("source").value = target.source || "";
    rows = target.endpoints.map((e) => row(e, true));
    show("mode-row", false);
    $("load").textContent = "Reload";
  }
  selectAuth(target ? target.auth.kind : "none");
  $("reauth").checked = draft.sign_in;

  const form = () => ({
    name: $("name").value,
    command: $("command").value,
    base_url: $("url").value,
    auth: {
      kind: kind(),
      name: $("auth-name").value,
      authorization_url: $("authorize-url").value,
      token_url: $("token-url").value,
      scope: $("scope").value,
      client_id: $("client-id").value,
    },
    secret: $("secret").value,
    drop_secret: $("drop").checked,
    client_secret: $("client-secret").value,
    reauth: $("reauth").checked,
    endpoints: rows.filter((r) => r.include).map((r) => r.endpoint),
    source: fromOpenApi || target ? source : null,
  });

  const fail = (message) => {
    $("error-text").textContent = message;
    show("error", !!message);
  };

  // --- the endpoint list -------------------------------------------------------

  const filterText = () => $("filter").value.trim().toLowerCase();
  const matches = (r, f) => {
    const e = r.endpoint;
    return !f || e.path.toLowerCase().includes(f) || e.name.toLowerCase().includes(f)
      || e.method.toLowerCase() === f || (e.description || "").toLowerCase().includes(f);
  };
  const shown = () => rows.filter((r) => matches(r, filterText()));

  const tip = (e) => {
    const lines = [`${e.method} ${e.path}`];
    if (e.description) lines.push(e.description);
    for (const p of e.parameters || []) lines.push(`--${p.name}  ${p.type} in ${p.in}${p.required ? ", required" : ""}`);
    const whole = e.body === "json" && !(e.parameters || []).some((p) => p.in === "body");
    if (whole) lines.push(`--body  JSON${e.body_required ? ", required" : ""}`);
    return lines.join("\n");
  };

  const iconButton = (name, title, onclick) => {
    const b = el("button", { class: "ed-icon", title, "aria-label": title, onclick, tabindex: "-1" });
    b.innerHTML = E.icon(name, 14);
    return b;
  };

  const drawList = () => {
    const visible = shown();
    const items = visible.map((r) => {
      const e = r.endpoint;
      const box = el("input", { type: "checkbox", "aria-label": e.name, tabindex: "-1" });
      box.checked = r.include;
      box.addEventListener("change", () => {
        r.include = box.checked;
        changed();
      });
      const summary = e.description ? "  ·  " + e.description.split("\n")[0] : "";
      const item = el("div", { class: "ed-ep" + (selected === r.id ? " selected" : ""), role: "option", tabindex: "0", title: tip(e) },
        box,
        el("span", { class: "ed-method" }, e.method),
        el("div", { class: "ed-ep-text" },
          el("div", { class: "ed-ep-path" }, e.path),
          el("div", { class: "quiet" }, el("span", { class: "ed-ep-name" }, e.name), summary)),
        iconButton("edit", "Edit", () => edit(r)),
        iconButton("trash", "Remove", () => remove(r)));
      item.addEventListener("focus", () => (selected = r.id));
      item.addEventListener("dblclick", (ev) => {
        if (ev.target !== box) edit(r);
      });
      item.addEventListener("keydown", (ev) => {
        if (ev.target !== item) return;
        if (ev.key === "Enter") edit(r);
        else if (ev.key === "Delete") remove(r);
        else if (ev.key === " ") {
          r.include = !r.include;
          box.checked = r.include;
          changed();
        } else if (ev.key === "ArrowDown") item.nextElementSibling?.focus();
        else if (ev.key === "ArrowUp") item.previousElementSibling?.focus();
        else return;
        ev.preventDefault();
        ev.stopPropagation();
      });
      return item;
    });
    if (items.length === 0) {
      items.push(el("div", { class: "ed-none" }, E.glyph("api", null, 24), el("div", null, rows.length === 0 ? "None" : "No match")));
    }
    $("list").replaceChildren(...items);
  };

  const remove = (r) => {
    rows = rows.filter((x) => x !== r);
    drawList();
    changed();
  };

  /// The endpoint dialog, modal: the result arrives as an event, with the row.
  const edit = (r) => {
    const taken = rows.filter((x) => x !== r).map((x) => x.endpoint.name);
    invoke("endpoint_open", { endpoint: r ? r.endpoint : null, taken, tag: r ? r.id : 0 })
      .catch((e) => fail(E.message(e)));
  };
  E.listenHere("endpoint-result", ({ tag, endpoint }) => {
    if (!endpoint) return;
    const r = rows.find((x) => x.id === tag);
    if (r) {
      r.endpoint = endpoint;
      r.include = true;
      selected = r.id;
    } else {
      const added = row(endpoint, true);
      rows.push(added);
      selected = added.id;
    }
    drawList();
    changed();
    $("list").querySelector(".ed-ep.selected")?.scrollIntoView({ block: "nearest" });
  });
  $("add-endpoint").addEventListener("click", () => edit(null));

  const checkAll = (on) => {
    for (const r of shown()) r.include = on;
    drawList();
    changed();
  };
  $("all").addEventListener("click", () => checkAll(true));
  $("none").addEventListener("click", () => checkAll(false));
  $("filter").addEventListener("input", () => {
    drawList();
    paint();
  });

  // --- the window state --------------------------------------------------------

  const paint = () => {
    const k = kind();
    $("command").placeholder = check.auto || "";
    $("from-openapi").classList.toggle("on", fromOpenApi);
    $("manual").classList.toggle("on", !fromOpenApi);
    $("from-openapi").setAttribute("aria-checked", String(fromOpenApi));
    $("manual").setAttribute("aria-checked", String(!fromOpenApi));
    show("source-panel", fromOpenApi || !!(target && target.source));
    $("load").disabled = busy() || $("source").value.trim().length === 0;
    $("browse").disabled = busy();
    show("insecure", !!check.unencrypted);

    show("auth-name-panel", k === "header" || k === "query" || k === "basic");
    $("auth-name-label").textContent = k === "header" ? "Header" : k === "query" ? "Parameter" : "User";
    $("auth-name").placeholder = k === "header" ? "X-API-Key" : k === "query" ? "api_key" : "";
    show("secret-panel", k !== "none" && !isOAuth());
    show("oauth-panel", isOAuth());
    show("authorize-panel", k === "oauth");
    show("redirect", k === "oauth");
    $("client-secret-label").textContent = k === "oauth-app" ? "Client secret" : "Client secret (if it has one)";
    $("client-secret").placeholder = check.has_client_secret ? "Stored · type to replace" : "";
    show("reauth-row", !!check.show_reauth);
    $("secret-label").textContent = k === "bearer" ? "Token" : k === "basic" ? "Password" : "Key";
    show("drop-row", !!check.keeps_secret);
    $("secret").disabled = $("drop").checked && !!check.keeps_secret;
    $("secret").placeholder = check.keeps_secret ? "Stored · type to replace" : "";
    $("secret-note").textContent = check.keeps_secret ? "Empty keeps the stored one" : "Stored encrypted";

    const included = rows.filter((r) => r.include).length;
    $("count").textContent = rows.length === included ? `${included}` : `${included} of ${rows.length}`;
    show("filter-row", rows.length > 1);

    $("heading").textContent = target ? `Edit “${target.name}”` : "Add a REST API";
    $("intro").textContent = check.permissions_restart ? "Agents will have to ask permission again" : "";

    const insecure = check.unencrypted ? ", unencrypted" : "";
    const signIn = k === "oauth-app" ? "Get a token" : "Sign in";
    $("save").textContent = !target ? (check.needs_sign_in ? `${signIn} and add${insecure}` : `Add${insecure}`)
      : check.needs_sign_in ? `${signIn} and save` : "Save";
    $("save").disabled = busy() || !!check.problem || check.problem === undefined;
    $("save").title = check.problem || "";
    $("cancel").textContent = signingIn ? "Stop" : "Cancel";
    $("inputs").disabled = signingIn;
  };

  let timer = 0;
  const refresh = () => new Promise((resolve) => {
    clearTimeout(timer);
    // With hundreds of endpoints, a check on every key is too much: one after a pause.
    timer = setTimeout(async () => {
      const mine = ++checking;
      const result = await invoke("api_check", { form: form() });
      if (mine === checking) {
        check = result;
        paint();
      }
      resolve();
    }, 80);
  });

  const changed = () => {
    fail("");
    paint();
    refresh();
  };
  for (const id of ["name", "command", "url", "auth-name", "authorize-url", "token-url", "scope", "client-id", "secret", "client-secret"]) {
    $(id).addEventListener("input", changed);
  }
  for (const id of ["auth", "reauth", "drop"]) $(id).addEventListener("change", changed);
  $("source").addEventListener("input", () => {
    show("source-result", false);
    paint();
  });
  const mode = (openApi) => {
    fromOpenApi = openApi;
    changed();
  };
  $("from-openapi").addEventListener("click", () => mode(true));
  $("manual").addEventListener("click", () => mode(false));

  // --- the OpenAPI document ------------------------------------------------------

  /// The document that was read fills the window. When adding, everything; when re-reading
  /// an existing API, only the endpoints: the chosen ones stay chosen, the new ones are
  /// offered turned off, and those written by hand stay.
  const apply = (doc, from) => {
    source = from;
    const url = $("url").value.trim();
    if (!target) {
      if (!$("name").value.trim() || $("name").value === importedName) $("name").value = importedName = doc.name;
      if (doc.base_url && (!url || url === importedUrl)) $("url").value = importedUrl = doc.base_url;
      selectAuth(doc.auth.kind);
      $("auth-name").value = doc.auth.name || "";
      fillOAuth(doc.auth);
      rows = doc.endpoints.map((e) => row(e, true));
    } else {
      if (!url && doc.base_url) $("url").value = doc.base_url;
      const chosen = new Set(rows.filter((r) => r.include).map((r) => r.endpoint.name));
      const imported = new Set(doc.endpoints.map((e) => e.name));
      const manual = rows.filter((r) => !imported.has(r.endpoint.name));
      rows = [...doc.endpoints.map((e) => row(e, chosen.has(e.name))), ...manual];
    }
    $("source-result").textContent = `${doc.version} · ${E.plural(doc.endpoints.length, "endpoint", "endpoints")}`
      + (doc.base_url ? "" : " · no base URL: write it below");
    $("source-result").classList.remove("err");
    show("source-result", true);
    show("skipped", doc.skipped.length > 0);
    $("skipped-text").textContent = `${E.plural(doc.skipped.length, "endpoint", "endpoints")} left out`;
    $("skipped").title = doc.skipped.slice(0, 40).join("\n") + (doc.skipped.length > 40 ? "\n…" : "");
    drawList();
  };

  const load = async () => {
    const from = $("source").value.trim();
    if (busy() || !from) return;
    loading = true;
    $("load").textContent = "Loading…";
    show("source-result", false);
    paint();
    try {
      apply(await invoke("api_import", { source: from }), from);
    } catch (error) {
      const message = E.message(error);
      if (message) {
        $("source-result").textContent = message;
        $("source-result").classList.add("err");
        show("source-result", true);
      }
    }
    loading = false;
    $("load").textContent = target ? "Reload" : "Load";
    changed();
  };
  $("load").addEventListener("click", load);
  $("source").addEventListener("keydown", (e) => {
    if (e.key === "Enter") {
      e.preventDefault();
      e.stopPropagation();
      load();
    }
  });
  $("browse").addEventListener("click", async () => {
    const path = await invoke("api_browse");
    if (!path) return;
    $("source").value = path;
    load();
  });

  // --- saving --------------------------------------------------------------------

  const save = async () => {
    await refresh();
    if (busy() || check.problem) return;
    fail("");
    signingIn = !!check.needs_sign_in;
    $("status-text").textContent = "Starting…";
    show("status", signingIn);
    $("save").disabled = true;
    paint();
    try {
      await invoke("api_save", { form: form() });
      return; // saved: the window closes by itself
    } catch (error) {
      fail(E.message(error));
    }
    signingIn = false;
    show("status", false);
    paint();
  };
  $("save").addEventListener("click", save);
  E.listenHere("api-status", (text) => {
    if (signingIn) $("status-text").textContent = text;
  });

  const cancel = () => invoke(busy() ? "editor_stop" : "editor_close");
  $("cancel").addEventListener("click", cancel);
  document.addEventListener("keydown", (e) => {
    if (e.key === "Escape") cancel();
    // Save is the default button.
    else if (e.key === "Enter" && e.target.tagName === "INPUT" && e.target.type !== "checkbox" && e.target.id !== "filter") {
      e.preventDefault();
      if (!$("save").disabled) save();
    }
  });

  drawList();
  paint();
  await refresh();
  invoke("editor_fit", {});
  $(target ? "name" : "source").focus();
  // Opened to sign in again: the sign-in starts right away.
  if (draft.sign_in && target) save();
})();
