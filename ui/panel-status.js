// The Status tab: the hub, the agents and the connected programs. A click on a permission opens
// its history, one on a program its commands, one on a command its detail.
// Descriptions, parameters and history are asked of the hub only there (`describe`, `history`):
// the snapshot, which arrives on every change, only has the names.
//
// At the bottom of the navigation, the hub box: whether it is there, and since when.
"use strict";

(() => {
  const el = Clak.el;
  const D = Describe;

  let page = null;
  let footer = null;
  let state = Panel.state();
  let ctx = Panel.ctx();
  /// Where we are: the overview, a program, one of its commands, a permission's history.
  let view = { page: "overview" };
  let tools = null; // the last `tools` reply, read
  let history = null; // the last `history` reply, read
  let asked = null; // what the hub has already been asked for

  const running = () => !!state && !state.offline && !!state.broker;
  const hosting = () => ctx.mode === "local";

  const go = (next) => {
    if (next.page === "program" && view.page !== "command") tools = null;
    if (next.page === "history") history = null;
    if (next.page === "overview" || next.page === "history" || (next.page === "program" && view.id !== next.id)) asked = null;
    view = next;
    draw();
    page.scrollTop = 0;
  };

  const glyphOf = (r) => {
    const o = r?.origin;
    if (o?.kind === "hub") return "settings";
    if (o?.kind === "connector") return o.protocol === "rest" ? "api" : "connector";
    return "program";
  };

  /// The executable of a program, for its icon: a connector or the hub has none.
  const exeOf = (r) => (r?.origin?.kind ? null : r?.origin?.exe);

  const users = (resourceId) => D.users(state, resourceId);
  const inUse = (resourceId) => (state.grants || []).some((g) => g.resource_id === resourceId);
  const byAge = (list) => [...(list || [])].sort((a, b) => a.since - b.since);

  // --- the breadcrumbs: "Status › Revit — Project › command" ----------------------

  /// `trail`: [text, on click]…, then the page title. On the Mac the way back to the page
  /// above, over a title; on Windows the large NavigationView breadcrumbs.
  const crumbs = (trail, last, mono) => {
    if (Clak.mac) {
      const [text, back] = trail[trail.length - 1];
      return [el("button", { class: "back", onclick: back }, Kit.icon("back"), el("span", null, text)),
        el("header", { class: "title" }, el("h1", { class: mono ? "mono" : "" }, last))];
    }
    const parts = [];
    for (const [text, onclick] of trail) parts.push(el("button", { class: "crumb", onclick, title: text }, text), Kit.icon("chevron", "sep"));
    parts.push(el("h1", { class: mono ? "mono" : "" }, last));
    return [el("header", { class: "title crumbs" }, parts)];
  };

  const toOverview = () => go({ page: "overview" });

  // --- the overview -------------------------------------------------------------

  const stat = (glyph, value, label, tone) =>
    el("div", { class: "card stat" }, el("span", { class: "tile" + (tone ? " " + tone : "") }, Kit.icon(glyph)),
      el("div", null, el("div", { class: "value" }, String(value)), el("div", { class: "quiet" }, label)));

  const brokerLine = (now) => {
    if (!running()) return Clak.mac ? "Not answering · reopening the app starts it" : "Starts when an agent runs clak";
    const b = state.broker;
    return `Started ${D.ago(b.since, now)} · pid ${b.pid}` + (b.can_ask ? "" : " · no interface: can't grant permissions") + (hosting() ? "" : " · separate process");
  };

  const grantRow = (g, now) => {
    const r = D.resourceOf(state, g.resource_id);
    const title = r ? D.resourceTitle(r) : g.resource_id;
    const content = el("span", { class: "row-body" }, Kit.tile(glyphOf(r), 24, null, exeOf(r)),
      el("span", { class: "lines" }, el("strong", { class: "ellipsis" }, title),
        el("span", { class: "quiet ellipsis", title: "" }, `${D.grantDetail(g, now)} · ${D.plural((g.tools || []).length, "command allowed", "commands allowed")}`)),
      g.errors > 0 ? Kit.badge(D.plural(g.errors, "error", "errors"), "err") : null);
    return el("div", { class: "grant", "data-id": g.id },
      Kit.clickRow(content, () => go({ page: "history", id: g.id }), "Calls and statistics", title),
      el("button", { class: "small", title: "Ends this permission", onclick: () => Panel.send({ type: "revoke", grant_id: g.id }) }, "Revoke"));
  };

  const agentCard = (s, now) => {
    const grants = byAge((state.grants || []).filter((g) => g.session_id === s.id));
    const mcp = s.via === "mcp";
    return el("div", { class: "card agent", "data-id": s.id },
      el("div", { class: "head" }, Kit.tile(mcp ? "chat" : "terminal", 40, "#ff9500", s.exe),
        el("div", { class: "lines" },
          el("div", { class: "name" }, el("strong", { class: "big" }, D.sessionTitle(s)), Kit.tag(mcp ? "MCP" : "CLI", { accent: true })),
          el("div", { class: "quiet" }, D.sessionDetail(s, now)),
          s.cwd ? el("div", { class: "folder quiet", title: s.cwd }, Kit.icon("folder"), el("span", { class: "mono ellipsis" }, s.cwd)) : null),
        el("button", { title: "Ends all its permissions", onclick: () => Panel.send({ type: "forget_session", session_id: s.id }) }, "Disconnect")),
      grants.length === 0
        ? el("div", { class: "quiet indent" }, "No programs in use")
        : el("div", { class: "inset indent" }, grants.map((g) => grantRow(g, now))));
  };

  const programCard = (r, now) => {
    const title = D.resourceTitle(r);
    const content = el("span", { class: "row-body top" }, Kit.tile(glyphOf(r), 36, null, exeOf(r)),
      el("span", { class: "lines" }, el("strong", null, title), el("span", { class: "quiet" }, D.resourceDetail(r, now)),
        el("span", { class: "users" + (inUse(r.id) ? "" : " idle") }, Kit.icon("people"), el("span", null, users(r.id)))));
    return el("div", { class: "card click", "data-id": r.id }, Kit.clickRow(content, () => go({ page: "program", id: r.id }), "Origin and commands", title));
  };

  const overview = (now) => {
    const on = running();
    const sessions = byAge(state.sessions);
    const resources = byAge(state.resources);
    const programs = resources.filter((r) => r.origin?.kind !== "hub").length; // the settings are always there: they don't count
    const grants = (state.grants || []).length;
    const pending = state.pending || [];
    return [
      Kit.title("Status"),
      el("div", { class: "card hero" }, el("span", { class: "tile round " + (on ? "ok" : "err") }, Kit.icon("status")),
        el("div", null, el("div", { class: "subtitle" }, on ? "Clak is running" : "Clak isn't running"), el("div", { class: "quiet" }, brokerLine(now)))),
      el("div", { class: "stats" },
        stat("agents", sessions.length, sessions.length === 1 ? "agent" : "agents"),
        stat("program", programs, programs === 1 ? "program" : "programs"),
        stat("shield", grants, grants === 1 ? "permission" : "permissions")),
      pending.length
        ? el("div", { title: Clak.mac ? "The question is in a window at the top right" : "The question is in a window at the bottom right" },
          Kit.bar("warn", [el("strong", null, "Waiting for your answer"), ...pending.map((p) => el("div", null, p.title))]))
        : null,
      Kit.section("Agents", { count: sessions.length }),
      sessions.length ? sessions.map((s) => agentCard(s, now)) : Kit.empty("agents", "None"),
      Kit.section("Connected programs", { count: programs }),
      resources.length
        ? el("div", { class: "programs" }, resources.map((r) => programCard(r, now)))
        : Kit.empty("program", "None", "A program connects on its own, usually with a button inside it"),
    ];
  };

  // --- a program -------------------------------------------------------------------

  /// The commands are asked of the hub when the view opens, and again if they change.
  const askTools = (r) => {
    const names = (r.tools || []).join(",");
    if (asked === names) return;
    asked = names;
    Panel.send({ type: "describe", resource_id: r.id });
  };

  const program = (now) => {
    const r = D.resourceOf(state, view.id);
    if (!r) return [...crumbs([["Status", toOverview]], view.title || "Program"), el("div", { class: "quiet" }, "Disconnected")];
    askTools(r);
    view.title = D.resourceTitle(r);
    const list = tools?.resourceId === r.id ? tools.tools : null;
    return [
      ...crumbs([["Status", toOverview]], view.title),
      el("div", { class: "card hero" }, Kit.tile(glyphOf(r), 48, null, exeOf(r)),
        el("div", null, el("div", { class: "selectable" }, D.origin(r.origin, r.resource?.publisher)),
          el("div", { class: "tags" }, Kit.tag(`clak ${r.name}`, { accent: Clak.mac, mono: true }), Kit.tag(D.plural((r.tools || []).length, "command", "commands")),
            Kit.tag(`connected ${D.ago(r.since, now)}`)),
          el("div", { class: "users" }, Kit.icon("people"), el("span", null, users(r.id))))),
      r.status ? Kit.bar("warn", r.status) : null,
      Kit.section("Commands", { count: Clak.mac && list ? list.length : undefined }),
      list === null ? el("div", { class: "quiet" }, "Loading…") : list.length === 0 ? el("div", { class: "quiet" }, "No commands") : null,
      list?.length
        ? el("div", { class: "card group commands" }, list.map((t) =>
          Kit.clickRow(el("span", { class: "row-body" }, el("span", { class: "mono strong cmd" }, t.name), el("span", { class: "quiet ellipsis" }, t.summary)),
            () => go({ page: "command", id: r.id, name: t.name }), null, t.name)))
        : null,
    ];
  };

  // --- a command -------------------------------------------------------------------

  const parameter = (p) =>
    el("div", { class: "parameter" },
      el("div", { class: "name" }, el("span", { class: "mono strong" }, p.name), Kit.tag(D.parameterType(p), { accent: true }),
        Kit.tag(p.required ? "required" : "optional", { tone: p.required ? "caution" : "" })),
      p.description ? el("div", { class: "selectable" }, p.description) : null,
      p.choices.length ? el("div", { class: "quiet" }, "values  ", el("span", { class: "mono" }, p.choices.join(", "))) : null,
      p.default ? el("div", { class: "quiet" }, "if omitted  ", el("span", { class: "mono" }, p.default)) : null);

  const command = () => {
    const r = D.resourceOf(state, view.id);
    const t = tools?.resourceId === view.id ? tools.tools.find((x) => x.name === view.name) : null;
    if (!r) {
      queueMicrotask(toOverview);
      return [];
    }
    askTools(r);
    const programTitle = D.resourceTitle(r);
    const toProgram = () => go({ page: "program", id: r.id });
    const head = Clak.mac ? crumbs([[programTitle, toProgram]], view.name, true) : crumbs([["Status", toOverview], [programTitle, toProgram]], view.name, true);
    if (!t) {
      if (tools?.resourceId === view.id) queueMicrotask(toProgram); // the command is gone
      return [...head, el("div", { class: "quiet" }, "Loading…")];
    }
    const line = D.usage(r.name, t);
    return [
      ...head,
      t.description ? el("p", { class: "description selectable" }, t.description) : null,
      Kit.section("From the command line"),
      el("div", { class: "inset code" }, el("span", { class: "mono selectable" }, line), Kit.iconButton("copy", "Copy", () => Kit.copy(line))),
      Kit.section("Parameters", { count: t.parameters.length }),
      t.parameters.length ? el("div", { class: "card group" }, t.parameters.map(parameter)) : Kit.empty(null, "No parameters"),
      t.schema ? el("details", { class: "expander" }, el("summary", null, "JSON schema"), el("pre", { class: "mono selectable" }, t.schema)) : null,
    ];
  };

  // --- a permission's history --------------------------------------------------------

  const bars = (list, failing) => {
    const top = Math.max(1, ...list.map((u) => u.calls));
    return list.map((u) =>
      el("div", { class: "usage" + (failing ? " failing" : "") },
        el("div", { class: "line" }, el("span", { class: "mono strong ellipsis" }, u.tool), el("span", { class: "quiet" }, failing ? D.failureLine(u) : D.usageLine(u))),
        el("div", { class: "meter" }, el("span", { style: `width:${Math.max(2, Math.round((failing ? u.errorRate : u.calls / top) * 100))}%` }))));
  };

  const callRow = (c, now) => {
    const kind = c.failed ? "err" : c.outcome === "ok" ? "ok" : "warn";
    return el("tr", null, el("td", { class: "tone " + kind }, Kit.icon(kind === "warn" ? "minus" : kind)),
      el("td", { class: "quiet-text" }, D.when(c.at, now)), el("td", { class: "mono" }, c.tool),
      el("td", { class: "quiet-text right" }, D.duration(c.seconds)), el("td", { class: c.failed ? "danger" : "quiet-text" }, D.outcome(c.outcome)));
  };

  const historyPage = (now) => {
    const g = (state.grants || []).find((x) => x.id === view.id);
    // The history is requested when the view opens, and again on every new call.
    if (g) {
      const key = `${g.calls}|${g.finished}|${g.errors}`;
      if (asked !== key) {
        asked = key;
        Panel.send({ type: "history", grant_id: g.id });
      }
    }
    const r = g ? D.resourceOf(state, g.resource_id) : null;
    if (r) view.title = D.resourceTitle(r);
    if (g) view.agent = D.agentOf(state, g);
    const data = history?.grantId === view.id ? history : null;
    const calls = data?.calls || [];
    const usage = data?.usage || [];
    const timed = usage.reduce((n, u) => n + u.calls, 0);
    const spent = usage.reduce((n, u) => n + u.seconds, 0);
    const mostUsed = data ? data.mostUsed() : [];
    const mostFailing = data ? data.mostFailing() : [];
    const status = !g ? "" : !data ? "Loading…" : calls.length === 0 ? "No calls yet" : "";
    return [
      ...crumbs([["Status", toOverview]], view.title || "Permission"),
      el("div", { class: "card hero" }, Kit.tile(glyphOf(r), 48, null, exeOf(r)),
        el("div", null, el("div", null, "Used by ", el("strong", null, view.agent || "")), el("div", { class: "quiet" }, g ? D.grantDetail(g, now) : "")),
        g ? el("button", { title: "Ends this permission", onclick: () => Panel.send({ type: "revoke", grant_id: g.id }) }, "Revoke") : null),
      g ? null : Kit.bar("info", "Permission ended"),
      g ? el("div", { class: "stats" },
        stat("calls", g.calls, g.calls === 1 ? "call" : "calls"),
        stat("warn", g.errors, g.errors === 1 ? "error" : "errors", g.errors > 0 ? "err" : "plain"),
        stat("stopwatch", timed === 0 ? "–" : D.duration(spent / timed), "on average")) : null,
      status ? el("div", { class: "quiet status" }, status) : null,
      data && usage.some((u) => u.calls > 0)
        ? el("div", { class: "columns" },
          el("div", null, Kit.section("Most used"), el("div", { class: "card" }, bars(mostUsed, false))),
          el("div", null, Kit.section("Most errors"),
            el("div", { class: "card" }, mostFailing.length ? bars(mostFailing, true) : el("div", { class: "no-errors" }, Kit.icon("ok"), el("span", null, "No errors")))))
        : null,
      calls.length ? Kit.section("Calls", { count: calls.length }) : null,
      calls.length
        ? el("div", { class: "card group" }, el("table", { class: "calls" },
          el("thead", null, el("tr", null, el("th"), el("th", null, "When"), el("th", null, "Command"), el("th", { class: "right" }, "Time"), el("th", null, "Outcome"))),
          el("tbody", null, calls.map((c) => callRow(c, now)))))
        : null,
      data && data.limit > 0 && calls.length >= data.limit ? el("div", { class: "quiet" }, `Last ${data.limit} · counts cover all`) : null,
      g && (g.tools || []).length
        ? el("details", { class: "expander" }, el("summary", null, `Commands allowed (${g.tools.length})`),
          Clak.mac ? el("div", { class: "tags" }, g.tools.map((t) => Kit.tag(t, { mono: true }))) : el("div", { class: "mono selectable" }, g.tools.join(", ")))
        : null,
    ];
  };

  // --- the drawing -----------------------------------------------------------------

  const drawFooter = () => {
    const on = running();
    Kit.replace(footer, el("span", { class: "dot " + (on ? "ok" : "err") }),
      el("div", { class: "lines" }, el("strong", null, on ? "Hub running" : "Hub not running"),
        el("span", { class: "quiet ellipsis" }, on ? `pid ${state.broker.pid} · ${hosting() ? "in this app" : "separate"}` : "Reopen the app to start it")));
    // Always last: the navigation items are added before it.
    if (footer.parentNode && footer !== footer.parentNode.lastElementChild) footer.parentNode.append(footer);
  };

  const draw = () => {
    if (!page) return;
    const now = Date.now();
    if (!running() && view.page !== "overview") view = { page: "overview" }; // without a hub there is nothing to look at
    const content =
      view.page === "program" ? program(now) : view.page === "command" ? command(now) : view.page === "history" ? historyPage(now) : overview(now);
    Kit.replace(page, content);
    drawFooter();
  };

  Panel.tab({
    id: "status",
    title: "Status",
    icon: Kit.navIcon("status", Kit.TINTS.status),
    order: 10,
    count: (s) => (s.pending || []).length,
    accent: true,
    mount(section) {
      page = section;
      footer = el("div", { class: "hub-footer" });
      document.getElementById("nav").append(footer);
      // A click on Status from inside a program goes back to the overview, as expected.
      const back = () => {
        if (!page.classList.contains("hidden") && view.page !== "overview") toOverview();
      };
      this.item.addEventListener("pointerdown", back);
      this.item.addEventListener("keydown", (e) => (e.key === "Enter" || e.key === " ") && back());
      // Time passes even if the hub is silent: "5 min ago" is updated here.
      setInterval(() => !page.classList.contains("hidden") ? draw() : drawFooter(), 15000);
    },
    render(s, c) {
      state = s;
      ctx = c;
      draw();
    },
    message(m) {
      if (m.type === "tools") {
        tools = D.tools(m);
        if (view.id === tools.resourceId) draw();
      } else if (m.type === "history") {
        history = D.history(m);
        if (view.id === history.grantId) draw();
      }
    },
  });
})();
