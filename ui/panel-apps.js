// The Applications tab: the programs the user allowed once and that the hub
// remembers, by executable. "Edit" gives one a name and a command of the user's own, or
// the place it has moved to; "Forget" makes it ask again on the next connection. Below,
// the standing permissions made with "Always allow" in a consent window: "Remove" makes
// the agent ask again.
"use strict";

(() => {
  const el = Clak.el;
  const D = Describe;
  let page = null;
  let list = null;
  let host = null;
  let state = Panel.state();
  let ctx = Panel.ctx();
  /// The application being edited: { key, name, label, command, exe, was, error }.
  let draft = null;

  /// Starts the application; if it can't, the button says so for a moment, with the reason on hover.
  const launch = (app, button) =>
    Clak.invoke("app_launch", { key: app.key }).catch((e) => {
      button.textContent = "Couldn't open";
      button.title = String(e);
      setTimeout(() => { button.textContent = "Open"; button.title = `Starts ${shown(app)}`; }, 3000);
    });

  /// The command the hub derives from a name (see cli_names::from in the hub).
  const RESERVED = ["list", "status", "help", "version", "settings"];
  const slug = (name) => {
    let text = name.normalize("NFD").replace(/[\u0300-\u036f]/g, "").toLowerCase()
      .replace(/[^a-z0-9]+/g, "-").replace(/^-+|-+$/g, "");
    if (text.length > 24) text = text.slice(0, 24).replace(/-+$/, "");
    return !text || RESERVED.includes(text) ? `app-${text}`.replace(/-+$/, "") : text;
  };

  const shown = (app) => app.label || app.name;

  const edit = (app) => {
    draft = { key: app.key, name: app.name, label: app.label || "", command: app.command || "", exe: app.exe, was: app.exe, error: null };
    drawEditor();
    host.scrollIntoView({ block: "nearest" });
    host.querySelector("input")?.focus();
  };

  const closeEditor = () => {
    draft = null;
    host.replaceChildren();
  };

  const choose = async () => {
    try {
      const exe = await Clak.invoke("app_choose", { exe: draft.exe });
      if (exe && draft) { draft.exe = exe; draft.error = null; drawEditor(); }
    } catch (e) {
      if (draft) { draft.error = String(e); drawEditor(); }
    }
  };

  const save = async () => {
    try {
      await Clak.invoke("app_edit", { key: draft.key, label: draft.label, command: draft.command, exe: draft.exe === draft.was ? null : draft.exe });
      closeEditor();
    } catch (e) {
      if (draft) { draft.error = String(e); drawEditor(); }
    }
  };

  const drawEditor = () => {
    if (!draft) return;
    const name = el("input", { type: "text", placeholder: draft.name, "aria-label": "Name", value: draft.label, maxlength: "64" });
    const command = el("input", { type: "text", class: "mono", placeholder: slug(draft.label.trim() || draft.name), "aria-label": "Command", value: draft.command, maxlength: "24", spellcheck: "false" });
    name.addEventListener("input", () => { draft.label = name.value; command.placeholder = slug(name.value.trim() || draft.name); });
    command.addEventListener("input", () => { draft.command = command.value; });
    const moved = draft.exe !== draft.was;
    Kit.replace(host, el("div", { class: "card ed-dialog preset-editor app-editor" },
      el("div", { class: "editor-title" }, el("strong", null, `Edit “${draft.label.trim() || draft.name}”`)),
      el("label", { class: "field" }, el("span", null, "Name"), name),
      el("label", { class: "field" }, el("span", null, "Command"), el("div", { class: "command-line" }, el("span", { class: "mono quiet" }, "clak"), command)),
      el("div", { class: "field" }, el("span", null, "Location"),
        el("div", { class: "folder-line" },
          el("span", { class: "mono ellipsis selectable", title: draft.exe }, draft.exe),
          Kit.link("Choose…", null, choose),
          moved ? Kit.link("Undo", null, () => { draft.exe = draft.was; drawEditor(); }) : null)),
      el("div", { class: "quiet hint" }, moved
        ? "Remembered by the file at the new location, its developer or, if unsigned, its content. Its copy at the old location will ask again."
        : "Agents see the name and use the application with the command; a second open document gets -2. Empty: its own."),
      draft.error ? Kit.bar("err", draft.error) : null,
      el("div", { class: "buttons" },
        el("button", { onclick: closeEditor }, "Cancel"),
        el("button", { class: "accent", onclick: save }, "Save"))));
  };

  const row = (app, now) => {
    const signed = !!app.signer;
    // A program that runs in Java or Python: the signature is the runtime's.
    const runtime = Clak.runtime(app.exe);
    return el("div", { class: Clak.mac ? "row allowed" : "card allowed", "data-id": app.key },
      Kit.tile("apps", 36, "#007aff", app.exe),
      el("div", { class: "lines" },
        el("div", { class: "name" }, el("strong", null, shown(app)),
          app.command ? Kit.tag(`clak ${app.command}`, { mono: true, help: "The command agents use it with" }) : null,
          runtime ? Kit.tag(runtime, { help: `Runs in ${runtime}` }) : null,
          signed ? el("span", { title: runtime ? `${runtime} signed by` : "Signed by" }, Kit.badge(app.signer, "ok")) : Kit.badge("unsigned", "warn")),
        el("div", { class: "mono quiet ellipsis selectable", title: app.exe }, app.exe),
        el("div", { class: "quiet" }, [app.label ? `Its own name: ${app.name}` : null, signed ? `Allowed ${D.ago(app.since, now)}` : `This version of the file only · allowed ${D.ago(app.since, now)}`].filter(Boolean).join(" · "))),
      el("div", { class: "actions" },
        el("button", { title: `Starts ${shown(app)}`, onclick: (e) => launch(app, e.currentTarget) }, "Open"),
        ctx.readonly ? null : Kit.iconButton("options", "Edit: name, command, location", () => edit(app)),
        el("button", { title: "It will ask again the next time it connects", onclick: () => Panel.send({ type: "forget_app", key: app.key }) }, "Forget")));
  };

  const ruleRow = (r, now) =>
    el("div", { class: Clak.mac ? "row allowed" : "card allowed", "data-id": `rule:${r.id}` },
      Kit.tile("shield", 36, "#34c759", r.exe),
      el("div", { class: "lines" },
        el("div", { class: "name" }, el("strong", null, `${r.agent} → ${r.program}`)),
        el("div", { class: "mono quiet ellipsis selectable", title: r.folder || "" }, r.folder || "In any folder"),
        el("div", { class: "quiet" }, `Always allowed since ${D.ago(r.since, now)}`)),
      el("div", { class: "actions" },
        el("button", { title: "It will ask again the next time it starts", onclick: () => Panel.send({ type: "forget_rule", id: r.id }) }, "Remove")));

  const draw = () => {
    if (!page) return;
    const now = Date.now();
    const apps = [...(state.apps || [])].sort((a, b) => a.since - b.since);
    const rows = apps.map((a) => row(a, now));
    const rules = [...(state.rules || [])].sort((a, b) => a.since - b.since);
    const ruleRows = rules.map((r) => ruleRow(r, now));
    const group = (items) => (Clak.mac ? el("div", { class: "card group rows" }, items) : items);
    Kit.replace(list,
      Kit.section("Allowed", { count: apps.length, help: "Remembered by executable: same path and same developer or, if unsigned, same content" }),
      apps.length === 0 ? Kit.empty("apps", "None allowed") : group(rows),
      Kit.section("Always allowed", { count: rules.length, help: "An agent that may use a program without asking: made with “Always allow” in a consent window" }),
      rules.length === 0
        ? Kit.empty("shield", "None", "Tick “Always allow” in a consent window: that agent won't ask again for that program")
        : group(ruleRows));
  };

  Panel.tab({
    id: "apps",
    title: "Applications",
    icon: Kit.navIcon("apps", Kit.TINTS.apps),
    order: 30,
    count: (s) => (s.apps || []).length,
    mount(section) {
      page = section;
      list = el("div");
      host = el("div");
      page.append(Kit.title("Applications"), list, host);
      setInterval(() => !page.classList.contains("hidden") && draw(), 30000);
    },
    render(s, c) {
      state = s;
      ctx = c;
      // Forgotten meanwhile, or the hub is now elsewhere: nothing left to edit.
      if (draft && (ctx.readonly || !(s.apps || []).some((a) => a.key === draft.key))) closeEditor();
      draw();
    },
  });
})();
