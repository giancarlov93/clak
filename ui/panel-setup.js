// How the agents reach Clak, in Settings › Integration: the command line on the PATH, the
// skill that teaches how to use clak, the MCP clients that host the shim. The rules and the
// actions live in agents.rs (clak-setup): here only the rows, the buttons and what happened
// below each one. It is re-read on every opening and when the window comes back to the
// foreground: other programs change those files too.
"use strict";

const Setup = (() => {
  const el = Clak.el;
  let page = null;
  let body = null;
  let data = null;
  /// The last thing that happened to a row: "Copied", the client's note, or the error as it is.
  const messages = {};
  const busy = new Set();

  const refresh = async () => {
    try {
      data = await Clak.invoke("agents_status");
    } catch (e) {
      data = data || { path: null, skills: [], clients: [] };
      messages.path = String(e);
    }
    draw();
  };

  const run = async (command, id) => {
    if (busy.has(id)) return;
    busy.add(id);
    delete messages[id];
    draw();
    try {
      messages[id] = await Clak.invoke(command, { id });
    } catch (e) {
      messages[id] = String(e);
    }
    busy.delete(id);
    await refresh();
  };

  const copy = async (id, done) => {
    const text = await Clak.invoke("agents_snippet", { id }).catch(() => null);
    if (!text) return;
    await Kit.copy(text);
    messages[id] = done || "Copied";
    draw();
  };

  const message = (id) => (messages[id] ? el("div", { class: "message" }, Clak.mac ? Kit.bar("info", messages[id]) : messages[id]) : null);

  const buttons = (row, accent) => {
    const working = busy.has(row.id);
    return [
      working ? el("span", { class: "spinner", "aria-label": "Working" }) : null,
      row.primary ? el("button", { class: accent || Clak.mac ? "accent" + (Clak.mac && row.tone !== "off" ? " caution" : "") : "", disabled: working,
        title: row.id === "path" ? row.help : undefined, onclick: () => run("agents_add", row.id) }, row.primary) : null,
      row.remove ? el("button", { disabled: working, onclick: () => run("agents_remove", row.id) }, row.remove) : null,
    ];
  };

  /// A row's state: a pill on Windows, colored text on the Mac.
  const stateOf = (row) =>
    Clak.mac ? el("div", { class: "state " + row.tone, title: row.help }, row.state) : Kit.badge(row.state, row.tone);

  const row = (r, glyph, accent, copyable) =>
    el("div", { class: Clak.mac ? "row setup" : "card setup", "data-id": r.id },
      Clak.mac ? el("span", { class: "tone " + r.tone }, Kit.icon(r.tone === "warn" ? "warn" : r.tone === "err" ? "err" : r.tone === "ok" ? "ok" : "off"))
        : Kit.tile(glyph, 32),
      el("div", { class: "lines" },
        el("div", { class: "name" }, el("strong", null, r.name), Clak.mac ? null : stateOf(r)),
        Clak.mac ? stateOf(r) : null,
        el("div", { class: "mono quiet ellipsis selectable", title: r.path }, r.path),
        message(r.id)),
      el("div", { class: "actions" }, buttons(r, accent),
        copyable && r.copy ? Kit.iconButton("copy", "Copy the entry, to add it by hand", () => copy(r.id)) : null));

  /// The command line: `clak`, where it is, and whether it is on the PATH.
  const commandLine = () => {
    const p = data.path;
    const cli = data.cli;
    const missing = Clak.mac ? "clak not found next to this app" : "Can't find clak.exe next to this app.";
    return el("div", { class: "card setup cli", "data-id": "path" },
      Kit.tile("terminal", 32, "#1c1c1e"),
      el("div", { class: "lines" },
        el("div", { class: "name" }, el("strong", { class: "mono" }, "clak"), p && !Clak.mac ? Kit.tag(p.state, { tone: p.tone === "ok" ? "ok" : "" }) : null),
        el("div", { class: "mono quiet ellipsis selectable", title: cli || "" }, cli || missing),
        p && Clak.mac ? stateOf(p) : null,
        message("path")),
      el("div", { class: "actions" }, p ? buttons(p, false) : null,
        cli ? Kit.iconButton("copy", "Copy the path", () => copy("path", Clak.mac ? "Copied" : "Path copied to the clipboard.")) : null));
  };

  const group = (rows) => (Clak.mac ? el("div", { class: "card group rows" }, rows) : rows);

  const chooseShim = async () => {
    try {
      if (await Clak.invoke("agents_choose_shim")) await refresh();
    } catch (e) {
      messages.shim = String(e);
      draw();
    }
  };

  const draw = () => {
    if (!page || !data) return;
    const skills = data.skills || [];
    const shimLine = el("div", { class: "mono quiet ellipsis selectable shim", title: data.shim || "" },
      data.shim ? `Shim: ${data.shim}` : `Shim not found (${data.shim_name})`);
    const copySkill = skills.length || (!Clak.mac && data.cli)
      ? Kit.link("Copy the instructions", Clak.mac ? null : "copy", () => copy(skills[0]?.id || "skill:", "Copied"),
        "For agents without skills: paste it into their instructions file")
      : null;
    const clientsRight = Clak.mac
      ? [el("button", { class: "small", onclick: refresh }, Kit.icon("refresh"), el("span", null, "Check again"))]
      : [Kit.link("Choose the shim…", "file", chooseShim), Kit.iconButton("refresh", "Check again", refresh)];
    Kit.replace(body,
      Kit.section("Command line", { help: "For agents with a shell: Claude Code, Codex, Gemini CLI…" }),
      commandLine(),
      Kit.section("Skills", { help: "Teach each coding agent on this computer to use clak", right: copySkill }),
      skills.length
        ? group(skills.map((s) => row(s, "skill", true, false)))
        : Clak.mac ? Kit.empty("skill", "None", "Without clak next to this app there's no skill to install") : null,
      Kit.section("MCP clients", {
        help: "For agents without a shell: an MCP server with three fixed tools (programs, commands, run). A changed file is backed up as .clak-bak.",
        right: clientsRight,
      }),
      Clak.mac ? null : shimLine,
      messages.shim ? el("div", { class: "message" }, messages.shim) : null,
      group((data.clients || []).map((c) => row(c, "client", false, true))),
      Clak.mac ? shimLine : null);
  };

  return {
    /// The section, for Settings: drawn once mounted, re-read with refresh.
    mount() {
      page = el("div");
      body = el("div");
      page.append(body);
      // On the Mac there is no installer: whatever pointed at a copy that is gone moves to
      // this one, and it stays written below the rows it touched.
      if (Clak.mac) {
        Clak.invoke("agents_repair").then((done) => {
          if (done && Object.keys(done).length) {
            Object.assign(messages, done);
            refresh();
          }
        }).catch(() => {});
      }
      refresh();
      return page;
    },
    refresh,
  };
})();
