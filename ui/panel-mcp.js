// The MCP tab: the MCP servers the hub reaches on behalf of the agents. Adding,
// changing, turning off and removing are user steps: the dialogs are opened by the app
// (editors.rs), and only if the hub lives here; otherwise the tab is view-only.
// Import brings in the servers Claude Desktop, Cursor and VS Code already have
// (clients/import.rs in clak-setup): the panel sees their names, never their keys.
"use strict";

(() => {
  const { el, invoke } = Clak;
  const D = Describe;
  const E = Editors;
  const TINT = "#5856d6";
  let page = null;
  let list = null;
  let add = null;
  let importButton = null;
  /// The import, while it is open: { found, chosen: Set of keys, results: key → { ok, message }, error, busy }.
  let importing = null;
  let state = Panel.state();
  let ctx = Panel.ctx();

  const run = (cmd, args) => invoke(cmd, args).catch((e) => console.warn(cmd, E.message(e)));

  /// How it has been answering the agents: in the warning color if the last command failed.
  const healthLine = (resourceId, now) => {
    const h = D.health(state, resourceId, now);
    return h ? el("div", { class: "quiet-text ed-state" + (h.failed ? " caution" : ""), title: "Since the agents' permissions began" }, h.text) : null;
  };

  /// The state in one word, for the pill: the detail is in the row below.
  const word = (c) => (!c.enabled ? "Off" : c.state ? c.state[0].toUpperCase() + c.state.slice(1) : "");

  const card = (c, now, manage) => {
    const kind = D.connectorKind(c);
    const oauth = c.auth === "oauth";
    const stdio = c.transport === "stdio";
    const where = stdio ? c.launch : c.url;
    const users = D.users(state, c.resource_id, "");
    const actions = manage
      ? el("div", { class: "ed-actions" },
        Kit.toggle(c.enabled, "Enabled", (on) => run("connector_enable", { id: c.id, on })),
        E.moreButton(() => [
          ...(c.resource_id ? [{ label: "Commands…", run: () => Panel.show(`status:program:${c.resource_id}`) }] : []),
          { label: "Edit…", run: () => run("connector_open", { id: c.id, signIn: false }) },
          { label: c.enabled ? "Turn off" : "Turn on", run: () => run("connector_enable", { id: c.id, on: !c.enabled }) },
          "-",
          { label: "Remove", danger: true, run: () => run("connector_remove", { id: c.id }) },
        ]))
      : null;
    if (actions) actions.firstChild.title = "Off: hidden from agents, stays saved";

    const bars = [];
    if (c.enabled && c.state !== "active" && c.error) {
      // The button that repairs: a new sign-in, or the window to change the token.
      const fixable = manage && (c.state === "token rejected" || c.state === "sign-in required");
      const signIn = c.state === "sign-in required";
      bars.push(Kit.bar(kind === "err" ? "err" : "warn", c.error, fixable
        ? el("button", { onclick: () => run("connector_open", { id: c.id, signIn }) }, signIn ? "Sign in…" : "Edit…")
        : null));
    }
    if (D.unencrypted(c.url)) {
      const b = Kit.bar("warn", "Unencrypted (http)");
      b.title = "The token and all data travel in clear text";
      bars.push(b);
    }

    return el("div", { class: "card ed-card" + (c.enabled ? "" : " off"), "data-id": c.id },
      el("div", { class: "row-body top" },
        Clak.withIcon(E.tile(oauth ? "key" : "connector", TINT), `mcp:${c.id}`, 36),
        el("div", { class: "lines" },
          el("div", { class: "ed-title" },
            el("strong", { class: "ed-name" }, c.name),
            word(c) ? Kit.badge(word(c), kind) : null,
            oauth ? Kit.tag("OAuth") : null,
            stdio ? Kit.tag("stdio", { help: "A program on this computer, started by Clak" }) : null,
            Kit.tag(`clak ${c.cli || c.command || E.cliFrom(c.name)}`, { mono: true, accent: Clak.mac, help: "What agents type" })),
          el("div", { class: "mono quiet ellipsis selectable", title: where }, where),
          el("div", { class: "quiet-text ed-state" }, D.connectorState(c, now)),
          healthLine(c.resource_id, now),
          users ? el("div", { class: "users" }, Kit.icon("people"), el("span", null, users)) : null),
        actions),
      bars.length ? el("div", { class: "indent ed-bars" }, bars) : null);
  };

  // --- the import from the other apps ---------------------------------------------

  const openImport = async () => {
    importing = { found: null, chosen: new Set(), results: {}, error: null, busy: false };
    draw();
    try {
      importing.found = await invoke("connector_import_list");
      // Ticked from the start: what can come in and isn't here yet.
      for (const f of importing.found) if (!f.problem && !f.added) importing.chosen.add(f.key);
    } catch (e) {
      importing.found = [];
      importing.error = E.message(e);
    }
    draw();
  };

  const closeImport = () => {
    importing = null;
    draw();
  };

  const runImport = async () => {
    const keys = [...importing.chosen];
    if (!keys.length || importing.busy) return;
    importing.busy = true;
    importing.error = null;
    draw();
    try {
      for (const r of await invoke("connector_import", { keys })) importing.results[r.key] = r;
      importing.chosen.clear();
      // Read again: what came in now shows as added.
      const found = await invoke("connector_import_list");
      importing.found = found;
    } catch (e) {
      importing.error = E.message(e);
    }
    importing.busy = false;
    draw();
  };

  /// "Import 3": the button follows the ticks without drawing everything again.
  const importLabel = () => {
    const n = importing.chosen.size;
    return n ? `Import ${n}` : "Import";
  };

  const foundRow = (f, go) => {
    const result = importing.results[f.key];
    const blocked = !!f.problem || !!f.added;
    const box = el("input", { type: "checkbox", "aria-label": `${f.source}: ${f.name}`, disabled: blocked || importing.busy });
    box.checked = importing.chosen.has(f.key);
    box.addEventListener("change", () => {
      if (box.checked) importing.chosen.add(f.key);
      else importing.chosen.delete(f.key);
      go.textContent = importLabel();
      go.disabled = !importing.chosen.size;
    });
    const stdio = f.transport === "stdio";
    const where = stdio ? [f.program, ...f.args].join(" ") : f.url;
    const carries = stdio
      ? (f.env.length ? `Variables: ${f.env.join(", ")}` : "")
      : (f.token ? "With its token" : "");
    const state = result
      ? el("div", { class: result.ok ? "quiet" : "danger" }, result.message)
      : f.added
        ? el("div", { class: "quiet" }, `Already in Clak as “${f.added}”`)
        : f.problem
          ? el("div", { class: "danger" }, `${f.problem} Add it by hand.`)
          : carries ? el("div", { class: "quiet" }, carries) : null;
    return el("label", { class: "pick import-row", "data-id": `import:${f.key}` }, box,
      el("div", { class: "lines" },
        el("div", { class: "ed-title" }, el("strong", null, f.name),
          Kit.tag(stdio ? "stdio" : "http", { help: stdio ? "A program on this computer" : "An address" })),
        el("div", { class: "mono quiet ellipsis", title: where }, where),
        state));
  };

  const importCard = () => {
    const go = el("button", { class: "accent", disabled: !importing.chosen.size || importing.busy, onclick: runImport }, importLabel());
    const found = importing.found;
    const sources = found ? [...new Set(found.map((f) => f.source))] : [];
    return el("div", { class: "card ed-dialog import-card" },
      el("div", { class: "editor-title" }, el("strong", null, "Import from other apps"),
        el("div", { class: "quiet" }, "The MCP servers of Claude Desktop, Cursor and VS Code, with their variables and tokens. Those apps keep theirs.")),
      found === null
        ? el("div", { class: "quiet" }, "Reading their configurations…")
        : found.length
          ? sources.map((source) => el("div", { class: "import-source" },
            el("div", { class: "section" }, el("h2", null, source)),
            found.filter((f) => f.source === source).map((f) => foundRow(f, go))))
          : el("div", { class: "quiet" }, "No MCP server in Claude Desktop, Cursor or VS Code."),
      importing.error ? Kit.bar("err", importing.error) : null,
      el("div", { class: "buttons" },
        el("button", { onclick: closeImport }, Object.keys(importing.results).length ? "Done" : "Cancel"),
        found && found.length ? go : null));
  };

  const draw = () => {
    if (!page) return;
    const manage = !ctx.readonly;
    add.disabled = !manage;
    importButton.disabled = !manage;
    if (!manage) importing = null;
    const now = Date.now();
    const connectors = [...(state.connectors || [])].sort((a, b) => a.since - b.since);
    Kit.replace(list,
      importing ? importCard() : null,
      Kit.section("Servers", {
        count: connectors.length,
        help: Clak.mac
          ? "Agents see them as connected programs. Tokens and sign-ins stay in your keychain."
          : "Agents see them as connected programs. Tokens and sign-ins stay encrypted for your Windows user.",
      }),
      connectors.length === 0 ? Kit.empty("mcp", "None") : connectors.map((c) => card(c, now, manage)));
  };

  Panel.tab({
    id: "mcp",
    title: "MCP",
    icon: Kit.navIcon("mcp", TINT),
    order: 40,
    count: (s) => (s.connectors || []).length,

    mount(section) {
      page = section;
      add = el("button", {
        class: "accent",
        title: "An MCP server: an address, or a program on this computer",
        "aria-label": "Add…",
        onclick: () => run("connector_open", { id: null, signIn: false }),
      }, E.buttonIcon("plus"), "Add…");
      importButton = el("button", {
        title: "The MCP servers Claude Desktop, Cursor and VS Code already have",
        onclick: () => (importing ? closeImport() : openImport()),
      }, "Import…");
      list = el("div");
      page.append(Kit.title("MCP", importButton, add), list);
      setInterval(() => !page.classList.contains("hidden") && draw(), 30000);
    },

    render(s, c) {
      state = s;
      ctx = c;
      draw();
    },
  });
})();
