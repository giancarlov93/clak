// The icon's box: each agent with the programs it may use under it, the waiting questions,
// a newer Clak if there is one, and two buttons. A click on an agent opens the panel on it, one on a program its history.
"use strict";

(async () => {
  const { invoke, listen, el } = Clak;
  const D = Describe;
  const $ = (x) => document.getElementById(x);
  let mode = "offline";
  let state = {};
  let update = null;

  const bySince = (a, b) => (a.since || 0) - (b.since || 0);
  const open = (tab) => invoke("open_panel", { tab });
  const revoke = (g) => invoke("hub_send", { message: { type: "revoke", grant_id: g.id } });

  const glyphOf = (r) => {
    const o = r?.origin;
    if (o?.kind === "hub") return "settings";
    if (o?.kind === "connector") return o.protocol === "rest" ? "api" : "connector";
    return "program";
  };
  /// Where a program's icon comes from: its executable, or the MCP server or API behind it.
  const exeOf = (r) => {
    const o = r?.origin;
    if (o?.kind === "connector") return D.iconSource(state, r.id);
    if (o?.kind === "hub") return "clak";
    return o?.kind ? null : o?.exe;
  };

  /// A row that opens something, as a button: the whole row is the target.
  const clickable = (cls, onclick, label, ...children) =>
    el("button", { class: "hit " + cls, type: "button", title: label, "aria-label": label, onclick }, children);

  /// The eye: brings the agent or the program to the front.
  const show = (kind, id, label) =>
    el("button", { class: "show", type: "button", title: label, "aria-label": label, onclick: (e) => Kit.bringToFront(e.currentTarget, kind, id) }, Kit.icon("show"));

  const program = (g, now) => {
    const r = D.resourceOf(state, g.resource_id);
    const title = r ? D.resourceShort(r) : g.resource_id;
    return el("div", { class: "program" },
      clickable("program-open", () => open(`status:history:${g.id}`), `${title}: calls and statistics`,
        Kit.tile(glyphOf(r), 24, null, exeOf(r)),
        el("span", { class: "lines" },
          el("span", { class: "title ellipsis" }, title),
          el("span", { class: "quiet ellipsis" }, D.grantDetail(g, now)))),
      D.showable(r) ? show("resource", r.id, `Brings ${title} to the front`) : null,
      el("button", { class: "revoke", type: "button", title: `Ends the permission to use ${title}`, onclick: () => revoke(g) }, "Revoke"));
  };

  const agent = (s, now) => {
    const own = (state.grants || []).filter((g) => g.session_id === s.id).sort(bySince);
    const mcp = s.via === "mcp";
    const detail = [mcp ? "MCP" : "Command line", own.length ? D.plural(own.length, "program", "programs") : "no programs yet"].join(" · ");
    return el("div", { class: "agent" },
      el("div", { class: "agent-row" },
        clickable("agent-open", () => open("status"), `${D.sessionTitle(s)}: open in the panel`,
          Kit.tile(mcp ? "chat" : "terminal", 32, "#ff9500", s.exe),
          el("span", { class: "lines" },
            el("span", { class: "title ellipsis" }, D.sessionTitle(s)),
            el("span", { class: "quiet ellipsis" }, detail)),
          el("span", { class: "chevron", "aria-hidden": "true" }, "›")),
        show("session", s.id, `Brings ${D.sessionTitle(s)} to the front`)),
      own.length ? el("div", { class: "programs" }, own.map((g) => program(g, now))) : null);
  };

  const render = () => {
    const sessions = [...(state.sessions || [])].sort(bySince);
    const pending = state.pending || [];
    const offline = mode === "offline" || state.offline;
    const now = Date.now();

    $("dot").className = "dot " + (offline ? "" : pending.length ? "pending" : (state.grants || []).length ? "linked" : "idle");
    $("headline").textContent = offline ? "Not running" : D.headline(state);

    $("pending").replaceChildren(...pending.map((p) =>
      clickable("question", () => open("status"), "Waiting for your answer",
        el("span", { class: "lines" }, el("span", { class: "title ellipsis" }, p.title), el("span", { class: "quiet" }, "Waiting for your answer")))));
    $("pending").classList.toggle("hidden", pending.length === 0);

    $("agents").replaceChildren(...(sessions.length && !offline
      ? sessions.map((s) => agent(s, now))
      : [el("div", { class: "empty" }, offline ? "Not running" : "No agent is connected")]));
  };

  /// A newer Clak: the row opens Settings, where it installs.
  const renderUpdate = () => {
    const version = update?.newer?.version;
    $("update").replaceChildren(...(version
      ? [clickable("update-row", () => open("settings:about"), `Clak ${version}: install from Settings`,
          Kit.tile("refresh", 24, "#34c759"),
          el("span", { class: "lines" },
            el("span", { class: "title ellipsis" }, `Clak ${version} is available`),
            el("span", { class: "quiet ellipsis" }, "Install from Settings")))]
      : []));
    $("update").classList.toggle("hidden", !version);
  };

  $("panel").addEventListener("click", () => open(null));
  $("quit").addEventListener("click", () => invoke("quit"));

  const hello = await invoke("hub_hello");
  mode = hello.mode;
  state = hello.state || {};
  render();
  update = await invoke("update_status");
  renderUpdate();
  listen("update", (u) => { update = u; renderUpdate(); });
  listen("hub-mode", (m) => { mode = m; render(); });
  listen("hub", (message) => { if (message.type === "state") { state = message; render(); } });
  // "2 minutes ago" moves on even when the hub is silent.
  listen("flyout-shown", render);
})();
