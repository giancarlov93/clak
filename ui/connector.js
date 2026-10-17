// Adding an MCP server, or changing one. It is a user's step in their own window: a
// link can only fill it in, never confirm it, and on an existing connector it can change
// only the sign-in. That's why, from a link, the button lights up after a moment and isn't
// the default one. With OAuth nothing is saved until the sign-in is finished. The
// checks and the saving are done by the app (editors.rs); the saved token doesn't come back here. A
// program (stdio) can be chosen only for a new server: of its saved variables, only
// the names arrive here.
"use strict";

(async () => {
  const { invoke } = Clak;
  const E = Editors;
  const $ = (id) => document.getElementById(id);

  const draft = await invoke("connector_draft");
  if (!draft) {
    invoke("editor_close");
    return;
  }
  const target = draft.target;
  let armed = draft.armed_in === 0;
  // The token box is still the link's one (the token stays in the app).
  let linkToken = draft.link_token;
  let busy = false;
  let probing = 0;
  let testing = false;
  let check = {};
  let checking = 0;
  let stdio = draft.stdio;

  $("name").value = draft.name;
  $("command").value = draft.command;
  $("url").value = draft.url;
  $("launch").value = draft.launch;
  $("env").placeholder = draft.env_names.length ? `Stored: ${draft.env_names.join(", ")} · type to replace` : "API_KEY=…";
  for (const id of ["name", "command", "url"]) $(id).readOnly = draft.restricted;
  $("link").classList.toggle("hidden", !draft.from_link);
  $(draft.oauth ? "mode-oauth" : "mode-token").checked = true;
  $("reauth").checked = draft.reauth;
  $("probe").append(E.glyph("test", null, 14), "Test");

  const oauth = () => $("mode-oauth").checked;
  const typedToken = () => $("token").value.trim();
  const hasToken = () => typedToken().length > 0 || linkToken;

  const form = () => ({
    name: $("name").value,
    command: $("command").value,
    url: $("url").value,
    oauth: oauth(),
    token: $("token").value,
    link_token: linkToken,
    drop_token: $("drop").checked,
    reauth: $("reauth").checked,
    client_id: $("client-id").value,
    client_secret: $("client-secret").value,
    stdio,
    launch: $("launch").value,
    env: $("env").value,
    drop_env: $("drop-env").checked,
  });

  const show = (id, on) => $(id).classList.toggle("hidden", !on);

  const fail = (message) => {
    $("error-text").textContent = message;
    show("error", !!message);
  };

  const paint = () => {
    const name = target ? target.name : "";
    show("kind-row", draft.can_choose);
    $("kind-url").classList.toggle("on", !stdio);
    $("kind-stdio").classList.toggle("on", stdio);
    $("kind-url").setAttribute("aria-checked", String(!stdio));
    $("kind-stdio").setAttribute("aria-checked", String(stdio));
    show("url-panel", !stdio);
    show("stdio-panel", stdio);
    $("program").textContent = check.program || "";
    show("program-line", stdio && !!check.program);
    show("drop-env-row", stdio && !!check.keeps_env);
    $("env").disabled = $("drop-env").checked;
    show("runs", stdio && !!check.program_changes);
    $("command").placeholder = check.auto || "";
    $("host").textContent = check.host || "";
    show("host-line", !!check.host);
    show("insecure", !stdio && !!check.unencrypted);
    show("oauth-panel", oauth());
    show("token-panel", !oauth());
    $("inputs").disabled = busy;
    $("probe").disabled = busy || testing || !check.can_probe;

    show("reauth-row", !!check.show_reauth);
    show("drop-row", !!check.keeps_token);
    $("token").disabled = $("drop").checked;
    $("token").placeholder = check.keeps_token ? "Stored · type to replace" : linkToken ? "••••••••••••" : "";
    $("token-note").textContent = draft.link_token ? "From the link · stored encrypted"
      : check.keeps_token ? "Empty keeps the stored one" : "Optional · stored encrypted";

    $("heading").textContent = !target ? "Add an MCP server"
      : draft.restricted ? (oauth() ? `Sign in to “${name}”` : `Token for “${name}”`)
      : `Edit “${name}”`;
    // Only what changes the decision: an existing connector, and permissions starting over.
    $("intro").textContent = draft.restricted ? "Already added: the link only updates its sign-in"
      : check.permissions_restart ? "Agents will have to ask permission again" : "";

    const unencrypted = !stdio && check.unencrypted ? ", unencrypted" : "";
    $("save").textContent = stdio ? (target ? "Save" : "Add")
      : !target ? (check.needs_sign_in ? `Sign in and add${unencrypted}` : `Add${unencrypted}`)
      : check.needs_sign_in ? (draft.restricted ? "Sign in" : "Sign in and save")
      : draft.restricted ? (hasToken() ? "Update the token" : "Remove the token")
      : "Save";
    $("save").disabled = !armed || busy || !!check.problem || check.problem === undefined;
    $("save").title = check.problem || "";
    $("cancel").textContent = busy ? "Stop" : "Cancel";
  };

  const refresh = async () => {
    const mine = ++checking;
    const result = await invoke("connector_check", { form: form() });
    if (mine !== checking) return;
    check = result;
    paint();
  };

  // Every change: the earlier test no longer counts, nor does the earlier error.
  const changed = () => {
    probing++;
    testing = false;
    $("probe-result").textContent = "";
    fail("");
    paint();
    refresh();
  };
  for (const id of ["name", "command", "url", "client-id", "launch", "env"]) $(id).addEventListener("input", changed);
  for (const id of ["mode-oauth", "mode-token", "reauth", "drop", "drop-env"]) $(id).addEventListener("change", changed);
  const kind = (on) => {
    if (stdio === on) return;
    stdio = on;
    check = {};
    changed();
    E.fitToContent();
    $(on ? "launch" : "url").focus();
  };
  $("kind-url").addEventListener("click", () => kind(false));
  $("kind-stdio").addEventListener("click", () => kind(true));
  $("token").addEventListener("input", () => {
    linkToken = false;
    changed();
  });

  $("probe").addEventListener("click", async () => {
    const mine = ++probing;
    testing = true;
    $("probe-result").textContent = "Testing…";
    paint();
    const result = await invoke("connector_probe", { form: form() });
    if (mine !== probing) return;
    testing = false;
    // A server that asks for sign-in, and no token written: sign-in is offered.
    if (!stdio && result.sign_in && !hasToken() && !oauth()) {
      $("mode-oauth").checked = true;
      changed();
      probing = mine;
    }
    $("probe-result").textContent = result.ok ? `✓ ${result.message}` : result.message;
    paint();
  });

  const save = async () => {
    if (!armed || busy || check.problem) return;
    fail("");
    busy = true;
    $("status-text").textContent = "";
    show("status", !stdio && !!check.needs_sign_in);
    paint();
    try {
      await invoke("connector_save", { form: form() });
      return; // saved: the window closes by itself
    } catch (error) {
      fail(E.message(error));
    }
    busy = false;
    show("status", false);
    paint();
    refresh();
  };
  $("save").addEventListener("click", save);

  // During sign-in, the first click stops the sign-in; the window stays.
  const cancel = () => invoke(busy ? "editor_stop" : "editor_close");
  $("cancel").addEventListener("click", cancel);
  document.addEventListener("keydown", (e) => {
    if (e.key === "Escape") cancel();
  });

  E.listenHere("connector-status", (text) => {
    if (busy) $("status-text").textContent = text;
  });

  await refresh();
  E.fitToContent();
  if (!target && !draft.from_link) $("name").focus();
  else if (!oauth() && draft.restricted) $("token").focus();
  else $("save").focus();

  // From a link, the button lights up after a moment: a click already under way doesn't find it ready.
  // Opened to sign in again, the sign-in starts right away.
  if (armed) {
    if (draft.sign_in && target) save();
  } else {
    setTimeout(() => {
      armed = true;
      refresh();
    }, draft.armed_in);
  }
})();
