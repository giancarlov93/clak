// The Applications tab: the programs the user allowed once and that the hub
// remembers, by executable. Open (play) starts one; Edit gives it a name and a command of
// the user's own, or the place it has moved to; Forget (the bin) makes it ask again on the
// next connection. An agent's permission to use a program is never kept here: it ends with
// the agent's process, and only a preset lets an agent in without asking.
"use strict";

(() => {
  const el = Clak.el;
  const D = Describe;
  let page = null;
  let list = null;
  let state = Panel.state();
  let ctx = Panel.ctx();
  /// The application being edited, right under its row: { key, name, label, command, exe,
  /// was, error }.
  let draft = null;

  /// Starts the application; if it can't, the button shows a warning for a moment, with the
  /// reason on hover.
  const launch = (app, button) =>
    Clak.invoke("app_launch", { key: app.key }).catch((e) => {
      button.replaceChildren(Kit.icon("warn"));
      button.title = `Couldn't open: ${e}`;
      setTimeout(() => { button.replaceChildren(Kit.icon("play")); button.title = `Open ${shown(app)}`; }, 3000);
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
    draw();
    const editor = [...list.querySelectorAll(".app-editor")].find((n) => n.dataset.id === `edit:${app.key}`);
    editor?.scrollIntoView({ block: "nearest" });
    editor?.querySelector("input")?.focus();
  };

  const closeEditor = () => {
    draft = null;
    draw();
  };

  const choose = async () => {
    try {
      const exe = await Clak.invoke("app_choose", { exe: draft.exe });
      if (exe && draft) { draft.exe = exe; draft.error = null; draw(); }
    } catch (e) {
      if (draft) { draft.error = String(e); draw(); }
    }
  };

  const save = async () => {
    try {
      await Clak.invoke("app_edit", { key: draft.key, label: draft.label, command: draft.command, exe: draft.exe === draft.was ? null : draft.exe });
      closeEditor();
    } catch (e) {
      if (draft) { draft.error = String(e); draw(); }
    }
  };

  /// The editor, under the row it edits. What is typed is kept as an attribute too, so that a
  /// redraw of the list (a program connecting) finds it unchanged and leaves it alone.
  const editor = () => {
    const name = el("input", { type: "text", placeholder: draft.name, "aria-label": "Name", value: draft.label, maxlength: "64" });
    const command = el("input", { type: "text", class: "mono", placeholder: slug(draft.label.trim() || draft.name), "aria-label": "Command", value: draft.command, maxlength: "24", spellcheck: "false" });
    name.addEventListener("input", () => {
      draft.label = name.value;
      name.setAttribute("value", name.value);
      command.setAttribute("placeholder", slug(name.value.trim() || draft.name));
    });
    command.addEventListener("input", () => {
      draft.command = command.value;
      command.setAttribute("value", command.value);
    });
    const moved = draft.exe !== draft.was;
    return el("div", { class: (Clak.mac ? "" : "card ") + "preset-editor app-editor", "data-id": `edit:${draft.key}` },
      el("label", { class: "field" }, el("span", null, "Name"), name),
      el("label", { class: "field" }, el("span", null, "Command"), el("div", { class: "command-line" }, el("span", { class: "mono quiet" }, "clak"), command)),
      el("div", { class: "field" }, el("span", null, "Location"),
        el("div", { class: "folder-line" },
          el("span", { class: "mono ellipsis selectable", title: draft.exe }, draft.exe),
          Kit.link("Choose…", null, choose),
          moved ? Kit.link("Undo", null, () => { draft.exe = draft.was; draw(); }) : null)),
      el("div", { class: "quiet hint" }, moved
        ? "Remembered by the file at the new location, its developer or, if unsigned, its content. Its copy at the old location will ask again."
        : "Agents see the name and use the application with the command; a second open instance gets -2. Empty: its own."),
      draft.error ? Kit.bar("err", draft.error) : null,
      el("div", { class: "buttons" },
        el("button", { onclick: closeEditor }, "Cancel"),
        el("button", { class: "accent", onclick: save }, "Save")));
  };

  const row = (app, now) => {
    const signed = !!app.signer;
    // A program that runs in Java or Python: the signature is the runtime's.
    const runtime = Clak.runtime(app.exe);
    const editing = draft?.key === app.key;
    return el("div", { class: (Clak.mac ? "row allowed" : "card allowed") + (editing ? " editing" : ""), "data-id": app.key },
      Kit.tile("apps", 36, "#007aff", app.exe),
      el("div", { class: "lines" },
        el("div", { class: "name" }, el("strong", null, shown(app)),
          app.command ? Kit.tag(`clak ${app.command}`, { mono: true, help: "The command agents use it with" }) : null,
          runtime ? Kit.tag(runtime, { help: `Runs in ${runtime}` }) : null,
          signed ? el("span", { title: runtime ? `${runtime} signed by` : "Signed by" }, Kit.badge(app.signer, "ok")) : Kit.badge("unsigned", "warn")),
        el("div", { class: "mono quiet ellipsis selectable", title: app.exe }, app.exe),
        el("div", { class: "quiet" }, [app.label ? `Its own name: ${app.name}` : null, signed ? `Allowed ${D.ago(app.since, now)}` : `This version of the file only · allowed ${D.ago(app.since, now)}`].filter(Boolean).join(" · ")),
        commandsLink(`app:${app.key}`)),
      el("div", { class: "actions" },
        Kit.iconButton("play", `Open ${shown(app)}`, (e) => launch(app, e.currentTarget)),
        ctx.readonly ? null : Kit.iconButton("options", "Edit: name, command, location", () => (editing ? closeEditor() : edit(app))),
        Kit.iconButton("trash", "Forget: it will ask again the next time it connects", () => Panel.send({ type: "forget_app", key: app.key }))));
  };

  /// Its commands as Clak keeps them, also when it isn't open: a link to review them.
  const commandsLink = (key) => {
    const kept = (state.commands || []).find((p) => p.key === key);
    if (!kept) return null;
    const text = `Commands: ${kept.approved} approved` + (kept.to_review ? ` · ${kept.to_review} to review` : "");
    return el("button", { class: "link small-link", title: "Which commands run without asking", onclick: () => Panel.show(`status:commands:${key}`) }, text);
  };

  const draw = () => {
    if (!page) return;
    const now = Date.now();
    const apps = [...(state.apps || [])].sort((a, b) => a.since - b.since);
    const rows = apps.flatMap((a) => (draft?.key === a.key ? [row(a, now), editor()] : [row(a, now)]));
    const group = (items) => (Clak.mac ? el("div", { class: "card group rows" }, items) : items);
    Kit.replace(list,
      Kit.section("Allowed", { count: apps.length, help: "Remembered by executable: same path and same developer or, if unsigned, same content" }),
      apps.length === 0 ? Kit.empty("apps", "None allowed") : group(rows));
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
      page.append(Kit.title("Applications"), list);
      setInterval(() => !page.classList.contains("hidden") && draw(), 30000);
    },
    render(s, c) {
      state = s;
      ctx = c;
      // Forgotten meanwhile, or the hub is now elsewhere: nothing left to edit.
      if (draft && (ctx.readonly || !(s.apps || []).some((a) => a.key === draft.key))) draft = null;
      draw();
    },
  });
})();
