// The Launch section, at the top of the Agents tab: the agents installed on this computer,
// launched from here and authorized without a question, and the presets — an agent, a folder
// and the programs it may use. Finding, launching and the presets live in launch.rs
// (clak-setup), the authorization in the hub (clak-hub/src/launch.rs): here only the rows,
// the buttons and the preset editor.
"use strict";

const Launch = (() => {
  const el = Clak.el;
  const D = Describe;
  let list = null;
  let host = null;
  let data = null;
  /// The preset being edited, as a draft: { id?, name, agent, folder, programs: Map }.
  let draft = null;
  /// Per agent: the folder chosen for its next launch.
  const folders = {};
  /// Per row: what happened, "Launched…" or the error as it is.
  const notes = {};
  const busy = new Set();

  const refresh = async () => {
    try {
      data = await Clak.invoke("launch_status");
    } catch (e) {
      data = data || { installed: [], presets: [], authorizes: false };
      notes.section = String(e);
    }
    draw();
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
        el("button", { class: Clak.mac ? "" : "accent", disabled: busy.has(row), title: `Starts ${a.name}${a.app ? "" : " in a terminal"}`,
          onclick: () => launch(row, { agent: a.id, folder: folder || null }) }, "Launch")));
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
        el("div", { class: "name" }, el("strong", null, p.name)),
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
    draft = {
      id: preset?.id,
      name: preset?.name || "",
      agent: preset?.agent || installed[0]?.id || "",
      folder: preset?.folder || "",
      programs: new Map((preset?.programs || []).map((p) => [idOf(p), { ...p }])),
      error: null,
    };
    drawEditor();
    host.scrollIntoView({ block: "nearest" });
    host.querySelector("input")?.focus();
  };

  const closeEditor = () => {
    draft = null;
    host.replaceChildren();
  };

  const save = async () => {
    const preset = {
      id: draft.id,
      name: draft.name,
      agent: draft.agent,
      folder: draft.folder || null,
      programs: [...draft.programs.values()],
    };
    try {
      await Clak.invoke("preset_save", { preset });
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

  const drawEditor = () => {
    if (!draft) return;
    const installed = data?.installed || [];
    const name = el("input", { type: "text", maxlength: "60", placeholder: "Shopping list", "aria-label": "Name" });
    name.value = draft.name;
    name.addEventListener("input", () => { draft.name = name.value; });
    const agent = el("select", { "aria-label": "Agent" }, installed.map((a) => el("option", { value: a.id }, a.name)));
    agent.value = draft.agent;
    agent.addEventListener("change", () => { draft.agent = agent.value; });
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
    const state = Panel.state();
    const installed = data.installed || [];
    const presets = data.presets || [];
    Kit.replace(list,
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
