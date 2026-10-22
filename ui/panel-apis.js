// The APIs tab: the REST APIs the hub calls on behalf of the agents, each one a program
// with one command per endpoint. As for MCP servers, the steps are taken by the user in the
// app's dialogs (editors.rs), and only with the hub here.
"use strict";

(() => {
  const { el, invoke } = Clak;
  const D = Describe;
  const E = Editors;
  const TINT = "#30b0c7";
  let page = null;
  let list = null;
  let add = null;
  let state = Panel.state();
  let ctx = Panel.ctx();

  const run = (cmd, args) => invoke(cmd, args).catch((e) => console.warn(cmd, E.message(e)));

  /// How it has been answering the agents: in the warning color if the last command failed.
  const healthLine = (resourceId, now) => {
    const h = D.health(state, resourceId, now);
    return h ? el("div", { class: "quiet-text ed-state" + (h.failed ? " caution" : ""), title: "Since the agents' permissions began" }, h.text) : null;
  };

  const card = (a, manage) => {
    const oauth = a.auth === "oauth" || a.auth === "oauth-app";
    // The OAuth sign-in must be redone: the tab says so, and the button redoes it.
    const needsSignIn = a.enabled && a.state === "sign-in required";
    const users = D.users(state, a.resource_id, "");
    const actions = manage
      ? el("div", { class: "ed-actions" },
        Kit.toggle(a.enabled, "Enabled", (on) => run("api_enable", { id: a.id, on })),
        E.moreButton(() => [
          ...(a.resource_id ? [{ label: "Commands…", run: () => Panel.show(`status:program:${a.resource_id}`) }] : []),
          { label: "Edit…", run: () => run("api_open", { id: a.id, signIn: false }) },
          ...(oauth ? [{ label: "Sign in…", run: () => run("api_open", { id: a.id, signIn: true }) }] : []),
          { label: a.enabled ? "Turn off" : "Turn on", run: () => run("api_enable", { id: a.id, on: !a.enabled }) },
          "-",
          { label: "Remove", danger: true, run: () => run("api_remove", { id: a.id }) },
        ]))
      : null;
    if (actions) actions.firstChild.title = "Off: hidden from agents, stays saved";

    const bars = [];
    if (needsSignIn) {
      bars.push(Kit.bar("err", "The service no longer accepts the sign-in", manage
        ? el("button", { onclick: () => run("api_open", { id: a.id, signIn: true }) }, "Sign in…")
        : null));
    }
    if (a.auth !== "none" && D.unencrypted(a.base_url)) {
      const b = Kit.bar("warn", "Unencrypted (http)");
      b.title = "The key and all data travel in clear text";
      bars.push(b);
    }

    return el("div", { class: "card ed-card" + (a.enabled ? "" : " off"), "data-id": a.id },
      el("div", { class: "row-body top" },
        Clak.withIcon(E.tile(oauth ? "key" : "api", TINT), `api:${a.id}`, 36),
        el("div", { class: "lines" },
          el("div", { class: "ed-title" },
            el("strong", { class: "ed-name" }, a.name),
            !a.enabled || needsSignIn ? Kit.badge(a.enabled ? "Sign-in required" : "Off", a.enabled ? "err" : "off") : null,
            Kit.tag(`clak ${a.cli || a.command || E.cliFrom(a.name)}`, { mono: true, accent: Clak.mac, help: "What agents type" })),
          el("div", { class: "mono quiet ellipsis selectable", title: a.base_url }, a.base_url),
          el("div", { class: "quiet-text ed-state" }, D.apiState(a)),
          healthLine(a.resource_id, Date.now()),
          users ? el("div", { class: "users" }, Kit.icon("people"), el("span", null, users)) : null),
        actions),
      bars.length ? el("div", { class: "indent ed-bars" }, bars) : null);
  };

  const draw = () => {
    if (!page) return;
    const manage = !ctx.readonly;
    add.disabled = !manage;
    const apis = [...(state.apis || [])].sort((a, b) => a.since - b.since);
    Kit.replace(list,
      Kit.section("REST APIs", {
        count: apis.length,
        help: Clak.mac
          ? "Agents see each API as a program, with a command per endpoint. Keys and tokens stay in your keychain."
          : "Agents see each API as a program, with a command per endpoint. Keys and tokens stay encrypted for your Windows user.",
      }),
      apis.length === 0 ? Kit.empty("api", "None") : apis.map((a) => card(a, manage)));
  };

  Panel.tab({
    id: "apis",
    title: "APIs",
    icon: Kit.navIcon("api", TINT),
    order: 50,
    count: (s) => (s.apis || []).length,

    mount(section) {
      page = section;
      add = el("button", {
        class: "accent",
        title: "From an OpenAPI document, or endpoint by endpoint",
        "aria-label": "Add…",
        onclick: () => run("api_open", { id: null, signIn: false }),
      }, E.buttonIcon("plus"), "Add…");
      list = el("div");
      page.append(Kit.title("APIs", add), list);
    },

    render(s, c) {
      state = s;
      ctx = c;
      draw();
    },
  });
})();
