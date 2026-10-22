// The Agents tab: the agents installed on this computer, launched from here and authorized
// without a question, with their global instructions and their past conversations, resumed,
// renamed or deleted; and the presets — an agent, a folder, the programs it may use and how it starts. Finding, launching and the presets live in
// launcher.rs and presets.rs (clak-setup), the instructions files in instructions.rs, the
// authorization in the hub (clak-hub/src/launch.rs): here only the rows, the buttons and the
// editors.
"use strict";

const Launch = (() => {
  const el = Clak.el;
  const D = Describe;
  let list = null;
  let host = null;
  let data = null;
  /// The preset being edited, as a draft: { id?, name, agent, folder, programs: Map, … }.
  let draft = null;
  /// The global instructions being edited: { agent, name, path, text, error }.
  let editing = null;
  /// Per agent: the folder chosen for its next launch.
  const folders = {};
  /// Per row: what happened, "Launched…" or the error as it is.
  const notes = {};
  const busy = new Set();
  /// An agent's past conversations, when that page is open: { agent, all, error, renaming,
  /// deleting, top, body }. `renaming` is { id, value }: what is typed stays from one drawing
  /// to the next. `top` (the title, the filter, the switch) is drawn once: only `body` is
  /// redrawn, so the filter keeps the focus while it is typed in.
  let talks = null;
  const filter = el("input", { type: "search", placeholder: "Filter", "aria-label": "Filter conversations", spellcheck: "false" });
  filter.addEventListener("input", () => draw());
  /// Grouped by folder or not: remembered on this computer, for the next time.
  const BY_FOLDER = "clak.conversations.byFolder";
  let byFolder = (() => { try { return localStorage.getItem(BY_FOLDER) === "1"; } catch { return false; } })();

  const refresh = async () => {
    try {
      data = await Clak.invoke("launch_status");
    } catch (e) {
      data = data || { installed: [], presets: [], authorizes: false };
      notes.section = String(e);
    }
    draw();
    // An agent's conversations go on while the page is open: re-read with the rest.
    if (talks && !talks.renaming) loadTalks();
  };

  const agentOf = (id) => (data?.installed || []).find((a) => a.id === id);
  const home = (path) => String(path || "").replace(/^\/Users\/[^/]+|^[A-Z]:\\Users\\[^\\]+/i, "~");

  const launch = async (row, args) => {
    if (busy.has(row)) return;
    busy.add(row);
    delete notes[row];
    draw();
    try {
      await Clak.invoke("launch_agent", args);
      notes[row] = data.authorizes
        ? "Launched: it is authorized as soon as it uses Clak."
        : "Launched. The hub runs in another process: it will ask as usual.";
    } catch (e) {
      notes[row] = String(e);
    }
    busy.delete(row);
    draw();
  };

  const chooseFolder = async () => Clak.invoke("launch_choose_folder").catch(() => null);

  const note = (row) => (notes[row] ? el("div", { class: "message" }, Clak.mac ? Kit.bar("info", notes[row]) : notes[row]) : null);
  const group = (rows) => (Clak.mac ? el("div", { class: "card group rows" }, rows) : rows);
  const spinner = (row) => (busy.has(row) ? el("span", { class: "spinner", "aria-label": "Launching" }) : null);

  // --- the agents installed --------------------------------------------------------

  const agentRow = (a) => {
    const row = `agent:${a.id}`;
    const folder = folders[a.id];
    return el("div", { class: Clak.mac ? "row setup launch" : "card setup launch", "data-id": row },
      Kit.tile(a.app ? "chat" : "terminal", 32, "#ff9500", a.path),
      el("div", { class: "lines" },
        el("div", { class: "name" }, el("strong", null, a.name), Kit.tag(a.app ? "App" : "Command line")),
        el("div", { class: "mono quiet ellipsis selectable", title: a.path }, a.path),
        a.folder
          ? el("div", { class: "folder-line quiet" }, Kit.icon("folder"),
            el("span", { class: "mono ellipsis", title: folder || "" }, folder ? home(folder) : a.app ? "No folder" : "Home folder"),
            Kit.link("Choose…", null, async () => {
              const chosen = await chooseFolder();
              if (chosen) { folders[a.id] = chosen; draw(); }
            }))
          : null,
        note(row)),
      el("div", { class: "actions" }, spinner(row),
        a.instructions ? Kit.iconButton("file", `Instructions · ${home(a.instructions)}`, () => editInstructions(a)) : null,
        a.conversations ? el("button", { title: `The past ${a.name} conversations: resume or rename them`, onclick: () => openTalks(a) }, "Conversations") : null,
        el("button", { class: Clak.mac ? "" : "accent", disabled: busy.has(row), title: `Starts ${a.name}${a.app ? "" : " in a terminal"}`,
          onclick: () => launch(row, { agent: a.id, folder: folder || null }) }, "Launch")));
  };

  // --- an agent's past conversations ----------------------------------------------

  const openTalks = async (a) => {
    closeEditor();
    filter.value = "";
    talks = { agent: a, all: null, error: null, renaming: null, deleting: null, top: el("div"), body: el("div") };
    Kit.replace(talks.top, ...talksHeader(`${a.name} conversations`));
    draw();
    list.scrollIntoView({ block: "start" });
    await loadTalks();
  };

  const loadTalks = async () => {
    const shown = talks;
    if (!shown) return;
    try {
      shown.all = await Clak.invoke("conversations_list", { agent: shown.agent.id });
      shown.error = null;
    } catch (e) {
      shown.all = shown.all || [];
      shown.error = String(e);
    }
    draw();
  };

  const closeTalks = () => {
    talks = null;
    draw();
  };

  /// One question at a time under the rows: renaming one closes the delete of another.
  const startRename = (c) => {
    talks.renaming = { id: c.id, value: c.title };
    talks.deleting = null;
    draw();
    const field = talks.body.querySelector(".talk-name input");
    field?.focus();
    field?.select();
  };

  const cancel = () => {
    talks.renaming = null;
    talks.deleting = null;
    draw();
  };

  const saveRename = async (c) => {
    const row = `talk:${c.id}`;
    const title = talks.renaming?.value || "";
    delete notes[row];
    try {
      await Clak.invoke("conversations_rename", { agent: talks.agent.id, id: c.id, title });
      talks.renaming = null;
      await loadTalks();
    } catch (e) {
      notes[row] = String(e);
      draw();
    }
  };

  const askDelete = (c) => {
    talks.deleting = c.id;
    talks.renaming = null;
    draw();
  };

  const deleteTalk = async (c) => {
    const row = `talk:${c.id}`;
    delete notes[row];
    busy.add(row);
    draw();
    try {
      await Clak.invoke("conversations_delete", { agent: talks.agent.id, id: c.id });
      talks.deleting = null;
      talks.all = talks.all.filter((t) => t.id !== c.id);
    } catch (e) {
      notes[row] = String(e);
    }
    busy.delete(row);
    await loadTalks();
  };

  /// The name being typed: a new field at every drawing, which takes over the focus and
  /// the selection of the one it replaces.
  const nameField = () => {
    const field = el("input", { type: "text", "aria-label": "Name", maxlength: "120", spellcheck: "false" });
    field.value = talks.renaming.value;
    field.addEventListener("input", () => { if (talks?.renaming) talks.renaming.value = field.value; });
    return field;
  };

  const talkRow = (c, now, grouped) => {
    const row = `talk:${c.id}`;
    const renaming = talks.renaming?.id === c.id;
    const deleting = talks.deleting === c.id;
    const where = c.folder ? home(c.folder) : "No folder";
    const when = D.ago(c.updated / 1000, now);
    let field = null;
    if (renaming) {
      field = nameField();
      field.addEventListener("keydown", (e) => {
        if (e.key === "Enter") saveRename(c);
        if (e.key === "Escape") cancel();
      });
    }
    const buttons = renaming
      ? [el("button", { onclick: cancel }, "Cancel"),
        el("button", { class: "accent", onclick: () => saveRename(c) }, "Save")]
      : deleting
        ? [el("button", { disabled: busy.has(row), onclick: cancel }, "Cancel"),
          el("button", { class: "danger", disabled: busy.has(row), onclick: () => deleteTalk(c) }, "Delete")]
        : [Kit.iconButton("trash", "Delete", () => askDelete(c)),
          el("button", { disabled: busy.has(row), title: "Gives it a name of your own", onclick: () => startRename(c) }, "Rename"),
          el("button", { class: Clak.mac ? "" : "accent", disabled: busy.has(row), title: `Resumes it in a terminal, in ${where}`,
            onclick: () => launch(row, { agent: talks.agent.id, resume: c.id }) }, "Resume")];
    return el("div", { class: Clak.mac ? "row setup talk" : "card setup talk", "data-id": row },
      el("div", { class: "lines" },
        renaming
          ? el("div", { class: "ed-dialog talk-name" }, field)
          : el("div", { class: "name" }, el("strong", { class: "ellipsis", title: c.title }, c.title)),
        deleting
          ? el("div", { class: "quiet" }, "Delete it for good? Claude Code won't find it either.")
          : grouped
            ? el("div", { class: "quiet" }, when)
            : el("div", { class: "quiet ellipsis", title: c.folder || "" }, el("span", { class: "mono" }, where), ` · ${when}`),
        note(row)),
      el("div", { class: "actions" }, spinner(row), buttons));
  };

  /// The way back to the agents: on the Mac a back button over the title, on Windows the
  /// breadcrumbs. On the right the filter and the switch that groups by folder.
  const talksHeader = (title) => {
    const search = el("label", { class: "search" }, Kit.icon("search"), filter);
    const grouping = el("div", { class: "talk-group", title: "The conversations under the folder where they took place" },
      el("span", null, "By folder"),
      Kit.toggle(byFolder, "Group by folder", (on) => {
        byFolder = on;
        try { localStorage.setItem(BY_FOLDER, on ? "1" : "0"); } catch { /* only for the next time */ }
        draw();
      }));
    if (Clak.mac) {
      return [el("button", { class: "back", onclick: closeTalks }, Kit.icon("back"), el("span", null, "Agents")), Kit.title(title, grouping, search)];
    }
    return [el("header", { class: "title crumbs" },
      el("button", { class: "crumb", onclick: closeTalks, title: "Agents" }, "Agents"), Kit.icon("chevron", "sep"),
      el("h1", null, title), el("div", { class: "actions" }, grouping, search))];
  };

  /// At most this many rows: the filter finds the others.
  const SHOWN = 200;

  /// The rows under their folder: the folders in the order of their latest conversation.
  const byFolders = (shown, now) => {
    const folders = new Map();
    for (const c of shown) {
      const key = c.folder || "";
      if (!folders.has(key)) folders.set(key, []);
      folders.get(key).push(c);
    }
    return [...folders].map(([folder, all]) => [
      el("div", { class: "section talk-folder", title: folder },
        el("h2", { class: "mono ellipsis" }, folder ? home(folder) : "No folder"),
        el("span", { class: "count" }, String(all.length))),
      group(all.map((c) => talkRow(c, now, true))),
    ]);
  };

  const drawTalks = () => {
    const a = talks.agent;
    if (list.firstChild !== talks.top || list.childNodes.length !== 2) list.replaceChildren(talks.top, talks.body);
    const words = filter.value.trim().toLowerCase().split(/\s+/).filter(Boolean);
    const matches = (talks.all || []).filter((c) => {
      const text = `${c.title} ${c.folder || ""}`.toLowerCase();
      return words.every((w) => text.includes(w));
    });
    const now = Date.now();
    const shown = matches.slice(0, SHOWN);
    // The name being typed: its field is new, the focus and the selection go over to it.
    const typing = document.activeElement?.closest?.(".talk-name") ? document.activeElement : null;
    const changed = Kit.replace(talks.body,
      talks.error ? Kit.bar("err", talks.error) : null,
      talks.all === null
        ? el("div", { class: "quiet" }, "Reading the conversations…")
        : shown.length
          ? byFolder ? byFolders(shown, now) : group(shown.map((c) => talkRow(c, now, false)))
          : Kit.empty("chat", words.length ? "No match" : "No conversations", words.length ? "Nothing with these words in the title or the folder" : `The conversations with ${a.name} show up here`),
      matches.length > SHOWN ? el("div", { class: "quiet hint" }, `The latest ${SHOWN} of ${matches.length}: the filter finds the others.`) : null,
      el("div", { class: "quiet hint" }, `A conversation resumes in a terminal, in the folder where it took place, authorized like a launch from here.`));
    const field = talks.body.querySelector(".talk-name input");
    if (changed && typing && field) {
      field.focus();
      field.setSelectionRange(typing.selectionStart, typing.selectionEnd);
    }
  };

  // --- an agent's global instructions ----------------------------------------------

  const editInstructions = async (a) => {
    draft = null;
    try {
      const file = await Clak.invoke("instructions_read", { agent: a.id });
      editing = { agent: a.id, name: a.name, path: file.path, text: file.text, error: null };
    } catch (e) {
      editing = { agent: a.id, name: a.name, path: a.instructions, text: "", error: String(e), unreadable: true };
    }
    drawInstructions();
    host.scrollIntoView({ block: "nearest" });
    host.querySelector("textarea")?.focus();
  };

  const saveInstructions = async () => {
    try {
      await Clak.invoke("instructions_write", { agent: editing.agent, text: editing.text });
      closeEditor();
    } catch (e) {
      editing.error = String(e);
      drawInstructions();
    }
  };

  const drawInstructions = () => {
    if (!editing) return;
    const text = el("textarea", { class: "mono", spellcheck: "false", "aria-label": "Instructions", disabled: !!editing.unreadable });
    text.value = editing.text;
    text.addEventListener("input", () => { editing.text = text.value; });
    host.replaceChildren(el("div", { class: "card ed-dialog preset-editor instructions-editor" },
      el("div", { class: "editor-title" }, el("strong", null, editing.name),
        el("div", { class: "mono quiet ellipsis selectable", title: editing.path }, editing.path)),
      text,
      editing.error ? Kit.bar("err", editing.error) : null,
      el("div", { class: "buttons" },
        el("button", { onclick: closeEditor }, "Cancel"),
        el("button", { class: "accent", disabled: !!editing.unreadable, onclick: saveInstructions }, "Save"))));
  };

  // --- the presets -----------------------------------------------------------------

  /// What a preset program points to, in the hub's terms: the open documents of a known
  /// application, or the connector with that name.
  const resourcesOf = (target, state) =>
    (state.resources || []).filter((r) => (target.app ? r.key === target.app : !r.key && r.name === target.name));

  const programChip = (p, state) => {
    const open = resourcesOf(p, state);
    const label = p.label || p.name || "Program";
    const tools = p.tools ? ` · ${D.plural(p.tools.length, "command", "commands")}` : "";
    const known = p.app ? (state.apps || []).some((a) => a.key === p.app) : true;
    return el("span", { class: "chip" + (open.length ? " on" : ""), title: open.length ? "Connected" : p.app ? "Not open" : "Not connected" },
      el("span", { class: "dot" }), `${label}${tools}`,
      p.app && !open.length && known
        ? el("button", { class: "link small-link", title: `Opens ${label}`, onclick: (e) => openApp(p.app, e.currentTarget) }, "Open")
        : null);
  };

  /// Opens a known application of a preset; if it can't, the button says so for a moment.
  const openApp = (key, button) =>
    Clak.invoke("app_launch", { key }).catch((e) => {
      button.textContent = "Couldn't open";
      button.title = String(e);
      setTimeout(() => { button.textContent = "Open"; }, 3000);
    });

  const presetRow = (p, state) => {
    const row = `preset:${p.id}`;
    const agent = agentOf(p.agent);
    return el("div", { class: Clak.mac ? "row setup launch" : "card setup launch", "data-id": row },
      Kit.tile("agents", 32, "#ff9500", agent?.path),
      el("div", { class: "lines" },
        el("div", { class: "name" }, el("strong", null, p.name),
          p.model ? Kit.tag(p.model, { mono: true, help: "Model" }) : null,
          p.system ? Kit.tag(p.system.mode === "replace" ? "Own system prompt" : "System prompt +", { help: p.system.text.slice(0, 300) }) : null),
        el("div", { class: "quiet ellipsis" }, [agent ? agent.name : `${p.agent} (not installed)`, p.folder ? home(p.folder) : null].filter(Boolean).join(" · ")),
        p.programs.length
          ? el("div", { class: "chips" }, p.programs.map((x) => programChip(x, state)))
          : el("div", { class: "quiet" }, "No programs: it asks for each one"),
        note(row)),
      el("div", { class: "actions" }, spinner(row),
        el("button", { disabled: !agent || busy.has(row), title: agent ? `Starts ${agent.name} with this preset` : "Its agent isn't installed",
          onclick: () => launch(row, { preset: p.id }) }, "Launch"),
        Kit.iconButton("options", "Edit", () => edit(p)),
        Kit.iconButton("trash", "Delete", () => remove(p))));
  };

  const remove = async (p) => {
    try {
      await Clak.invoke("preset_remove", { id: p.id });
    } catch (e) {
      notes[`preset:${p.id}`] = String(e);
    }
    if (draft?.id === p.id) closeEditor();
    refresh();
  };

  // --- the editor ------------------------------------------------------------------

  /// What a preset can include: the known applications (all their documents) and the
  /// connectors and APIs (by the name agents type). The commands, when the program is
  /// connected and they can be read.
  const candidates = () => {
    const state = Panel.state();
    const toolsOf = (target) => [...new Set(resourcesOf(target, state).flatMap((r) => r.tools || []))].sort();
    const cli = (c) => c.cli || c.command || Editors.cliFrom(c.name);
    return [
      ...(state.apps || []).map((a) => ({ id: `app:${a.key}`, target: { app: a.key }, label: a.name, kind: "Application" })),
      ...(state.connectors || []).map((c) => ({ id: `name:${cli(c)}`, target: { name: cli(c) }, label: c.name, kind: "MCP" })),
      ...(state.apis || []).map((a) => ({ id: `name:${cli(a)}`, target: { name: cli(a) }, label: a.name, kind: "API" })),
    ].map((c) => ({ ...c, tools: toolsOf(c.target) }));
  };

  const idOf = (p) => (p.app ? `app:${p.app}` : `name:${p.name}`);

  const edit = (preset) => {
    const installed = data?.installed || [];
    editing = null;
    draft = {
      id: preset?.id,
      name: preset?.name || "",
      agent: preset?.agent || installed[0]?.id || "",
      folder: preset?.folder || "",
      programs: new Map((preset?.programs || []).map((p) => [idOf(p), { ...p }])),
      mode: preset?.system?.mode || "",
      prompt: preset?.system?.text || "",
      model: preset?.model || "",
      /// The folder's instructions file, once opened: { agent, folder, path, text, before }.
      project: null,
      error: null,
    };
    drawEditor();
    host.scrollIntoView({ block: "nearest" });
    host.querySelector("input")?.focus();
  };

  const closeEditor = () => {
    draft = null;
    editing = null;
    host.replaceChildren();
  };

  const save = async () => {
    const cli = !agentOf(draft.agent)?.app;
    const preset = {
      id: draft.id,
      name: draft.name,
      agent: draft.agent,
      folder: draft.folder || null,
      programs: [...draft.programs.values()],
      system: cli && draft.mode ? { mode: draft.mode, text: draft.prompt } : null,
      model: cli ? draft.model.trim() || null : null,
    };
    const project = draft.project;
    try {
      await Clak.invoke("preset_save", { preset });
      // The folder's instructions belong to the folder, not to the preset: written apart.
      if (project && project.text !== project.before) {
        await Clak.invoke("instructions_write", { agent: project.agent, folder: project.folder, text: project.text });
      }
      closeEditor();
      refresh();
    } catch (e) {
      draft.error = String(e);
      drawEditor();
    }
  };

  /// One program in the editor: ticked or not, and with all its commands or some.
  const programPick = (c) => {
    const chosen = draft.programs.get(c.id);
    const toggle = el("input", { type: "checkbox", "aria-label": c.label });
    toggle.checked = !!chosen;
    toggle.addEventListener("change", () => {
      if (toggle.checked) draft.programs.set(c.id, { ...c.target, label: c.label });
      else draft.programs.delete(c.id);
      drawEditor();
    });
    const head = el("label", { class: "pick" }, toggle, el("span", null, c.label), Kit.tag(c.kind));
    if (!chosen) return el("div", { class: "pick-row" }, head);
    const all = el("input", { type: "checkbox" });
    all.checked = !chosen.tools;
    all.disabled = c.tools.length === 0 && !chosen.tools;
    all.addEventListener("change", () => {
      if (all.checked) delete chosen.tools;
      else chosen.tools = [...c.tools];
      drawEditor();
    });
    const tools = chosen.tools
      ? el("div", { class: "tools" }, [...new Set([...c.tools, ...chosen.tools])].sort().map((name) => {
        const box = el("input", { type: "checkbox" });
        box.checked = chosen.tools.includes(name);
        box.addEventListener("change", () => {
          chosen.tools = box.checked ? [...chosen.tools, name] : chosen.tools.filter((t) => t !== name);
        });
        return el("label", { class: "pick mono" }, box, el("span", null, name));
      }))
      : null;
    return el("div", { class: "pick-row" }, head,
      el("label", { class: "pick indent-pick", title: c.tools.length ? "" : "Its commands can be chosen while it is connected" },
        all, el("span", null, c.tools.length || chosen.tools ? "All commands" : "All commands (connect it to choose some)")),
      tools);
  };

  /// The ways the agent takes a system prompt: "append", "replace".
  const MODES = { append: "Add to it", replace: "Replace it" };

  /// How the agent starts: its model and its system prompt. Command-line agents only.
  const startFields = (a) => {
    if (!a || a.app) return [];
    const model = el("input", { type: "text", maxlength: "100", placeholder: "Default", spellcheck: "false", class: "mono", "aria-label": "Model" });
    model.value = draft.model;
    model.addEventListener("input", () => { draft.model = model.value; });
    const modes = a.system || [];
    const mode = el("select", { "aria-label": "System prompt", disabled: !modes.length },
      el("option", { value: "" }, "Default"), modes.map((m) => el("option", { value: m }, MODES[m] || m)));
    mode.value = modes.includes(draft.mode) ? draft.mode : "";
    mode.addEventListener("change", () => { draft.mode = mode.value; drawEditor(); });
    const prompt = el("textarea", { class: "mono", spellcheck: "false", "aria-label": "System prompt text" });
    prompt.value = draft.prompt;
    prompt.addEventListener("input", () => { draft.prompt = prompt.value; });
    return [
      el("label", { class: "field" }, el("span", null, "Model"), model),
      el("div", { class: "field top" }, el("span", null, "System prompt"),
        el("div", { class: "stack" }, mode, mode.value ? prompt : null)),
    ];
  };

  /// The instructions file the agent reads in the preset's folder (CLAUDE.md, AGENTS.md…).
  const projectField = (a) => {
    if (!a?.project_file || !draft.folder) return null;
    const open = draft.project && draft.project.agent === a.id && draft.project.folder === draft.folder ? draft.project : null;
    if (!open) {
      return el("div", { class: "field" }, el("span", null, "Instructions"),
        el("div", { class: "folder-line" }, Kit.link(`Edit ${a.project_file}`, "file", async () => {
          try {
            const file = await Clak.invoke("instructions_read", { agent: a.id, folder: draft.folder });
            draft.project = { agent: a.id, folder: draft.folder, path: file.path, text: file.text, before: file.text };
          } catch (e) {
            draft.error = String(e);
          }
          drawEditor();
        })));
    }
    const text = el("textarea", { class: "mono", spellcheck: "false", "aria-label": a.project_file });
    text.value = open.text;
    text.addEventListener("input", () => { open.text = text.value; });
    return el("div", { class: "field top" }, el("span", null, "Instructions"),
      el("div", { class: "stack" }, el("div", { class: "mono quiet ellipsis selectable", title: open.path }, home(open.path)), text));
  };

  const drawEditor = () => {
    if (!draft) return;
    const installed = data?.installed || [];
    const name = el("input", { type: "text", maxlength: "60", placeholder: "Shopping list", "aria-label": "Name" });
    name.value = draft.name;
    name.addEventListener("input", () => { draft.name = name.value; });
    const agent = el("select", { "aria-label": "Agent" }, installed.map((a) => el("option", { value: a.id }, a.name)));
    agent.value = draft.agent;
    agent.addEventListener("change", () => { draft.agent = agent.value; drawEditor(); });
    const chosen = agentOf(draft.agent);
    const folder = el("div", { class: "folder-line" },
      el("span", { class: "mono ellipsis", title: draft.folder }, draft.folder ? home(draft.folder) : "Home folder"),
      Kit.link("Choose…", null, async () => {
        const chosen = await chooseFolder();
        if (chosen) { draft.folder = chosen; drawEditor(); }
      }),
      draft.folder ? Kit.link("Clear", null, () => { draft.folder = ""; drawEditor(); }) : null);
    const all = candidates();
    // A program of the preset that isn't among the candidates any more still shows.
    for (const [id, p] of draft.programs) {
      if (!all.some((c) => c.id === id)) all.push({ id, target: p.app ? { app: p.app } : { name: p.name }, label: p.label || p.name || "Program", kind: "Gone", tools: [] });
    }
    host.replaceChildren(el("div", { class: "card ed-dialog preset-editor" },
      el("div", { class: "editor-title" }, el("strong", null, draft.id ? "Edit preset" : "New preset")),
      el("label", { class: "field" }, el("span", null, "Name"), name),
      el("label", { class: "field" }, el("span", null, "Agent"), agent),
      el("div", { class: "field" }, el("span", null, "Folder"), folder),
      projectField(chosen),
      startFields(chosen),
      el("div", { class: "field top" }, el("span", null, "Programs"),
        all.length
          ? el("div", { class: "picks" }, all.map(programPick))
          : el("div", { class: "quiet" }, "No program known yet: allow an application, or add an MCP server or an API.")),
      el("div", { class: "quiet hint" }, "Launched with this preset, the agent uses these programs without a question. Other commands and programs still ask."),
      draft.error ? Kit.bar("err", draft.error) : null,
      el("div", { class: "buttons" },
        el("button", { onclick: closeEditor }, "Cancel"),
        el("button", { class: "accent", onclick: save }, "Save"))));
  };

  // --- the section -----------------------------------------------------------------

  const draw = () => {
    if (!list || !data) return;
    if (talks) return drawTalks();
    const state = Panel.state();
    const installed = data.installed || [];
    const presets = data.presets || [];
    Kit.replace(list,
      Kit.title("Agents"),
      Kit.section("Launch", { help: "The agents installed on this computer. Launched from here, they are authorized without a question" }),
      data.authorizes ? null : Kit.bar("warn", "The hub runs in another process: a launched agent asks as usual."),
      notes.section ? Kit.bar("err", notes.section) : null,
      installed.length ? group(installed.map(agentRow)) : Kit.empty("agents", "No agent found", "Claude Code, Codex, Gemini CLI, OpenCode, Claude Desktop, VS Code, Cursor, Windsurf"),
      Kit.section("Presets", {
        count: presets.length,
        help: "An agent, a folder and the programs it may use without asking",
        right: el("button", { class: "small", disabled: !installed.length, onclick: () => edit(null) }, Kit.icon("plus"), el("span", null, "New preset")),
      }),
      presets.length ? group(presets.map((p) => presetRow(p, state))) : Kit.empty("agents", "None", "A preset launches an agent already allowed to use some programs"));
  };

  return {
    /// The section's elements, for the Agents tab: the rows, then the editor.
    mount() {
      list = el("div");
      host = el("div");
      return [list, host];
    },
    refresh,
    draw,
  };
})();
