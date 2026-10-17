// The icon's box: who is using what, the waiting questions, and two buttons.
"use strict";

(async () => {
  const { invoke, listen, el, plural } = Clak;
  const $ = (x) => document.getElementById(x);
  let mode = "offline";

  const render = (state) => {
    const sessions = state.sessions || [];
    const resources = state.resources || [];
    const grants = state.grants || [];
    const pending = state.pending || [];
    const offline = mode === "offline" || state.offline;

    $("dot").className = "dot " + (offline ? "" : pending.length ? "pending" : grants.length ? "linked" : "idle");
    $("headline").textContent = offline
      ? "Not running"
      : [plural(sessions.length, "agent", "agents"), plural(resources.length, "program", "programs"), plural(grants.length, "permission", "permissions")].join(" · ");

    $("pending").replaceChildren(...pending.map((p) => el("div", { class: "row" }, el("div", { class: "text" }, el("div", { class: "title" }, p.title), el("div", { class: "quiet" }, "Waiting for your answer")))));
    $("pending").classList.toggle("hidden", pending.length === 0);

    const byId = (list, id) => list.find((x) => x.id === id);
    const rows = grants.map((g) => {
      const resource = byId(resources, g.resource_id);
      const session = byId(sessions, g.session_id);
      const title = resource ? resource.resource.title : g.resource_id;
      const exe = resource && !resource.origin?.kind ? resource.origin?.exe : null;
      return el("div", { class: "row" },
        Clak.withIcon(el("span", { class: "app-icon" }), exe, 20),
        el("div", { class: "text" },
          el("div", { class: "title" }, title),
          el("div", { class: "quiet" }, session ? session.label : "")),
        el("button", { onclick: () => invoke("hub_send", { message: { type: "revoke", grant_id: g.id } }) }, "Revoke"));
    });
    $("grants").replaceChildren(...(rows.length ? rows : [el("div", { class: "empty" }, offline ? "Not running" : "No agent is using a program")]));
  };

  $("panel").addEventListener("click", () => invoke("open_panel", {}));
  $("quit").addEventListener("click", () => invoke("quit"));

  const hello = await invoke("hub_hello");
  mode = hello.mode;
  render(hello.state);
  listen("hub-mode", (m) => { mode = m; });
  listen("hub", (message) => { if (message.type === "state") render(message); });
})();
