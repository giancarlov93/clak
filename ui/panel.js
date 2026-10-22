// The panel: the navigation, the pages, and the hub state that reaches all of them.
//
// Each tab lives in its own file and registers itself like this:
//
//   Panel.tab({ id: "mcp", title: "MCP", icon: "<svg…>", order: 40,
//               count: (state) => number or 0,        // the small count next to the item
//               accent: true,                          // the small count in the accent color
//               mount(page) { … },                     // once, with its <section>
//               render(state, ctx) { … },              // on every new hub state
//               message(m) { … },                      // the hub's replies (tools, history)
//               log(items) { … } })                    // the new log rows
//
// ctx = { mode: "local" | "remote" | "offline", home, readonly }.
"use strict";

const Panel = (() => {
  const tabs = [];
  let current = null;
  let state = { sessions: [], resources: [], grants: [], pending: [], apps: [] };
  let ctx = { mode: "offline", home: "", readonly: true };
  const log = [];

  const tab = (definition) => tabs.push(definition);

  /// A tab, and optionally a place inside it after a colon: "status:history:<grant id>".
  const show = (where) => {
    const [id, ...rest] = String(where || "").split(":");
    const target = tabs.find((t) => t.id === id) || tabs[0];
    if (rest.length && target.id === id) target.route?.(rest.join(":"));
    // Another tab: its page rises into place, as WinUI's pages and the Mac's panes come in.
    if (current !== null && current !== target.id) Clak.enter(target.page, { rise: Clak.mac ? 6 : 12 });
    current = target.id;
    for (const t of tabs) {
      t.page.classList.toggle("hidden", t.id !== current);
      t.item.classList.toggle("selected", t.id === current);
      t.item.setAttribute("aria-current", t.id === current ? "page" : "false");
    }
    if (location.hash.slice(1) !== current) history.replaceState(null, "", "#" + current);
    target.shown?.(state, ctx);
  };

  const badges = () => {
    for (const t of tabs) {
      const n = t.count ? t.count(state) : 0;
      // What needs the user (a sign-in expired) shows instead of the count, in red.
      const alert = t.alert ? t.alert(state) : 0;
      t.badge.textContent = alert > 0 ? String(alert) : n > 0 ? String(n) : "";
      t.badge.classList.toggle("hidden", !(alert > 0 || n > 0));
      t.badge.classList.toggle("alert", alert > 0);
      t.badge.title = alert > 0 ? (alert === 1 ? "One needs you" : `${alert} need you`) : "";
    }
  };

  const render = () => {
    badges();
    document.getElementById("readonly").classList.toggle("hidden", ctx.mode !== "remote");
    for (const t of tabs) t.render?.(state, ctx);
  };

  const start = async () => {
    tabs.sort((a, b) => (a.order || 0) - (b.order || 0));
    const nav = document.getElementById("nav");
    const pages = document.getElementById("pages");
    for (const t of tabs) {
      t.badge = Clak.el("span", { class: "badge hidden" + (t.accent ? " accent" : "") });
      t.item = Clak.el("button", { class: "item", onclick: () => show(t.id) },
        Clak.el("span", { class: "icon" }), Clak.el("span", { class: "label" }, t.title), t.badge);
      t.item.querySelector(".icon").innerHTML = t.icon || "";
      nav.append(t.item);
      t.page = Clak.el("section", { class: "page hidden", id: "page-" + t.id, "aria-label": t.title });
      pages.append(t.page);
      t.mount?.(t.page);
    }

    const hello = await Clak.invoke("hub_hello");
    ctx = { mode: hello.mode, home: hello.home, readonly: hello.mode !== "local" };
    state = hello.state;
    log.push(...hello.log);
    for (const t of tabs) t.log?.(hello.log, true);
    show(location.hash.slice(1) || "status");
    render();

    Clak.listen("hub-mode", (mode) => {
      ctx = { ...ctx, mode, readonly: mode !== "local" };
      render();
    });
    Clak.listen("hub", (m) => {
      if (m.type === "state") {
        state = m;
        render();
      } else if (m.type === "log") {
        log.push(...m.items);
        if (log.length > 500) log.splice(0, log.length - 500);
        for (const t of tabs) t.log?.(m.items, false);
      } else {
        for (const t of tabs) t.message?.(m);
      }
    });
    Clak.listen("panel-tab", (id) => show(id));
  };

  return {
    tab,
    show,
    start,
    state: () => state,
    ctx: () => ctx,
    log: () => log,
    /// What the panel asks the hub: revoke, forget_session, forget_app, describe, history, usage.
    send: (message) => Clak.invoke("hub_send", { message }),
  };
})();
