// The MCP tab: the MCP servers the hub reaches on behalf of the agents. Adding,
// changing, turning off and removing are user steps: the dialogs are opened by the app
// (editors.rs), and only if the hub lives here; otherwise the tab is view-only.
"use strict";

(() => {
  const { el, invoke } = Clak;
  const D = Describe;
  const E = Editors;
  const TINT = "#5856d6";
  let page = null;
  let list = null;
  let add = null;
  let state = Panel.state();
  let ctx = Panel.ctx();

  const run = (cmd, args) => invoke(cmd, args).catch((e) => console.warn(cmd, E.message(e)));

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
          users ? el("div", { class: "users" }, Kit.icon("people"), el("span", null, users)) : null),
        actions),
      bars.length ? el("div", { class: "indent ed-bars" }, bars) : null);
  };

  const draw = () => {
    if (!page) return;
    const manage = !ctx.readonly;
    add.disabled = !manage;
    const now = Date.now();
    const connectors = [...(state.connectors || [])].sort((a, b) => a.since - b.since);
    Kit.replace(list,
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
      list = el("div");
      page.append(Kit.title("MCP", add), list);
      setInterval(() => !page.classList.contains("hidden") && draw(), 30000);
    },

    render(s, c) {
      state = s;
      ctx = c;
      draw();
    },
  });
})();
