// The Agents tab: the agents installed on this computer, with their version and whether a
// newer one is out, launched from here and authorized without a question, with their global instructions and their past conversations, found by
// what was said, resumed, renamed, deleted or saved as Markdown; and the presets — an agent, a folder, the programs it may use and how it starts. Finding, launching and the presets live in
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
  /// deleting, found, cleanup, top, body }. `renaming` is { id, value }: what is typed stays
  /// from one drawing to the next. `found` is { words, ids } for the words searched in what
  /// was said, `cleanup` { days, confirming } while it is open. `top` (the title, the filter,
  /// the switch) is drawn once: only `body` is redrawn, so the filter keeps the focus while it
  /// is typed in.
  let talks = null;
  const filter = el("input", { type: "search", placeholder: "Filter", "aria-label": "Filter conversations", spellcheck: "false" });
  filter.addEventListener("input", () => {
    draw();
    searchText();
  });
  /// Grouped by folder or not, and the conversations pinned at the top: remembered on this
  /// computer, for the next time.
  const BY_FOLDER = "clak.conversations.byFolder";
  const PINNED = "clak.conversations.pinned";
  let byFolder = (() => { try { return localStorage.getItem(BY_FOLDER) === "1"; } catch { return false; } })();
  const pinned = new Set((() => { try { return JSON.parse(localStorage.getItem(PINNED) || "[]"); } catch { return []; } })());

  /// Per agent: { version, latest, newer, update: { itself, command } }, read after the rest.
  let versions = {};
  /// The hub's usage rows ({ at, agent, program, calls }), and how many days are shown.
  let usage = null;
  let usageDays = 1;

  const loadVersions = async () => {
    try {
      versions = Object.fromEntries((await Clak.invoke("agents_versions")).map((v) => [v.id, v]));
      draw();
    } catch { /* without versions, the rows as they were */ }
  };

  const refresh = async () => {
    Panel.send({ type: "usage" });
    loadVersions();
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
        el("div", { class: "name" }, el("strong", null, a.name), Kit.tag(a.app ? "App" : "Command line"),
          versions[a.id]?.version ? Kit.tag(versions[a.id].version, { mono: true, help: "The version installed" }) : null),
        el("div", { class: "mono quiet ellipsis selectable", title: a.path }, a.path),
        updateLine(a),
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
        a.conversations ? el("button", { title: a.conversations.in_app ? `The ${a.name} Code sessions: open them again` : `The past ${a.name} conversations: find and resume them`, onclick: () => openTalks(a) }, "Conversations") : null,
        el("button", { class: Clak.mac ? "" : "accent", disabled: busy.has(row), title: `Starts ${a.name}${a.app ? "" : " in a terminal"}`,
          onclick: () => launch(row, { agent: a.id, folder: folder || null }) }, "Launch")));
  };

  /// A newer version, and how to get it: the agent's own update in a terminal, or the
  /// command to type.
  const updateLine = (a) => {
    const v = versions[a.id];
    if (!v?.newer || !v.update) return null;
    const row = `agent:${a.id}`;
    const action = v.update.itself
      ? Kit.link("Update", "refresh", () => updateAgent(a), `Runs ${v.update.command} in a terminal`)
      : Kit.link("Copy the command", "copy", async () => {
        notes[row] = (await Kit.copy(v.update.command)) ? `Copied: ${v.update.command}` : v.update.command;
        draw();
      }, v.update.command);
    return el("div", { class: "update-line" }, Kit.badge(`${v.latest} available`, "accent"), action);
  };

  const updateAgent = async (a) => {
    const row = `agent:${a.id}`;
    delete notes[row];
    try {
      await Clak.invoke("agent_update", { agent: a.id });
      notes[row] = "Updating in a terminal. The new version shows here once it's done.";
    } catch (e) {
      notes[row] = String(e);
    }
    draw();
  };

  // --- how much the agents used the programs ----------------------------------------

  /// Since local midnight, `days` days back (1: today).
  const since = (days) => {
    const start = new Date();
    start.setHours(0, 0, 0, 0);
    start.setDate(start.getDate() - (days - 1));
    return start.getTime() / 1000;
  };

  /// Per agent, its commands and the programs they went to, the busiest first.
  const usageRows = () => {
    const from = since(usageDays);
    const agents = new Map();
    for (const r of usage || []) {
      if (r.at < from) continue;
      const a = agents.get(r.agent) || { total: 0, programs: new Map() };
      a.total += r.calls;
      a.programs.set(r.program, (a.programs.get(r.program) || 0) + r.calls);
      agents.set(r.agent, a);
    }
    return [...agents].sort((x, y) => y[1].total - x[1].total).map(([agent, a]) =>
      el("div", { class: Clak.mac ? "row setup usage" : "card setup usage", "data-id": `usage:${agent}` },
        el("div", { class: "lines" },
          el("div", { class: "name" }, el("strong", null, agent || "Agent"), Kit.tag(Clak.plural(a.total, "command", "commands"))),
          el("div", { class: "chips" }, [...a.programs].sort((x, y) => y[1] - x[1]).map(([program, calls]) =>
            el("span", { class: "chip" }, `${program} · ${calls}`))))));
  };

  const usageSection = () => {
    const pick = (days, label) => el("button", { class: "small" + (usageDays === days ? " selected" : ""), "aria-pressed": String(usageDays === days),
      onclick: () => { usageDays = days; draw(); } }, label);
    const rows = usage ? usageRows() : [];
    return [
      Kit.section("Usage", {
        help: "The commands the agents ran, per program: only how many, never what they sent",
        right: el("div", { class: "usage-span" }, pick(1, "Today"), pick(7, "7 days")),
      }),
      usage === null
        ? el("div", { class: "quiet" }, "Reading…")
        : rows.length ? group(rows) : Kit.empty("calls", usageDays === 1 ? "No command today" : "No command in 7 days", "What the agents run through Clak shows up here"),
    ];
  };

  // --- an agent's past conversations ----------------------------------------------

  const openTalks = async (a) => {
    closeEditor();
    filter.value = "";
    delete notes.cleanup;
    talks = { agent: a, all: null, error: null, renaming: null, deleting: null, found: null, cleanup: null, top: el("div"), body: el("div") };
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

  /// What this agent's conversations allow: { rename, delete, search, export, in_app }.
  const can = () => talks?.agent.conversations || {};

  /// The words searched in what was said too, a moment after the last key: the files can be
  /// many megabytes.
  let searching = null;
  const searchText = () => {
    clearTimeout(searching);
    if (!talks || !can().search) return;
    const words = filter.value.trim().toLowerCase().split(/\s+/).filter(Boolean);
    if (!words.length) {
      talks.found = null;
      return;
    }
    const shown = talks;
    searching = setTimeout(async () => {
      const key = words.join(" ");
      shown.found = { words: key, ids: null };
      draw();
      try {
        const ids = await Clak.invoke("conversations_search", { agent: shown.agent.id, words });
        if (shown.found?.words === key) shown.found.ids = new Set(ids);
      } catch (e) {
        shown.found = null;
        shown.error = String(e);
      }
      draw();
    }, 350);
  };

  /// "2.3 MB".
  const megabytes = (bytes) => {
    if (bytes < 1e6) return `${Math.max(1, Math.round(bytes / 1e3))} KB`;
    return `${(bytes / 1e6).toFixed(bytes < 1e7 ? 1 : 0)} MB`;
  };

  /// The ages the cleanup offers, in days.
  const AGES = [[7, "a week"], [30, "a month"], [90, "3 months"], [180, "6 months"], [365, "a year"]];

  /// What the cleanup would delete: not used for longer than its age, and not pinned.
  const stale = () => {
    const since = Date.now() - talks.cleanup.days * 86400e3;
    return (talks.all || []).filter((c) => c.updated < since && !pinned.has(c.id));
  };

  const toggleCleanup = () => {
    delete notes.cleanup;
    talks.cleanup = talks.cleanup ? null : { days: 90, confirming: false };
    draw();
  };

  const runCleanup = async () => {
    const ids = stale().map((c) => c.id);
    talks.cleanup.busy = true;
    draw();
    try {
      const failed = await Clak.invoke("conversations_delete_many", { agent: talks.agent.id, ids });
      notes.cleanup = failed.length
        ? `${ids.length - failed.length} deleted; ${failed.length} not: ${failed[0].error}`
        : `${ids.length} deleted.`;
      talks.cleanup = null;
    } catch (e) {
      notes.cleanup = String(e);
      talks.cleanup.busy = false;
    }
    await loadTalks();
  };

  const cleanupBar = () => {
    const c = talks.cleanup;
    const all = talks.all || [];
    const gone = stale();
    const freed = gone.reduce((sum, t) => sum + (t.size || 0), 0);
    const total = all.reduce((sum, t) => sum + (t.size || 0), 0);
    const age = el("select", { "aria-label": "Not used for more than", disabled: c.confirming || !!c.busy },
      AGES.map(([days, label]) => el("option", { value: String(days) }, label)));
    age.value = String(c.days);
    age.addEventListener("change", () => { c.days = Number(age.value); draw(); });
    const what = gone.length
      ? `${Clak.plural(gone.length, "conversation", "conversations")}, ${megabytes(freed)}`
      : "none";
    return el("div", { class: "card cleanup" },
      el("div", { class: "lines" },
        el("div", null, c.confirming
          ? `Delete ${Clak.plural(gone.length, "conversation", "conversations")} for good? ${talks.agent.name} won't find ${gone.length === 1 ? "it" : "them"} either.`
          : ["Not used for more than ", age, `: ${what}. Pinned ones stay.`]),
        el("div", { class: "quiet" }, `All together: ${megabytes(total)} in ${Clak.plural(all.length, "conversation", "conversations")}.`)),
      el("div", { class: "actions" },
        c.busy ? el("span", { class: "spinner", "aria-label": "Deleting" }) : null,
        c.confirming
          ? [el("button", { disabled: !!c.busy, onclick: () => { c.confirming = false; draw(); } }, "Cancel"),
            el("button", { class: "danger", disabled: !!c.busy, onclick: runCleanup }, "Delete")]
          : [el("button", { onclick: toggleCleanup }, "Close"),
            el("button", { class: "danger", disabled: !gone.length, onclick: () => { c.confirming = true; draw(); } }, gone.length ? `Delete ${gone.length}` : "Delete")]));
  };

  const exportTalk = async (c) => {
    const row = `talk:${c.id}`;
    delete notes[row];
    busy.add(row);
    draw();
    try {
      const path = await Clak.invoke("conversations_export", { agent: talks.agent.id, id: c.id });
      if (path) notes[row] = `Saved: ${home(path)}`;
    } catch (e) {
      notes[row] = String(e);
    }
    busy.delete(row);
    draw();
  };

  const togglePin = (c) => {
    if (!pinned.delete(c.id)) pinned.add(c.id);
    try { localStorage.setItem(PINNED, JSON.stringify([...pinned])); } catch { /* only for the next time */ }
    draw();
  };

  const openFolder = async (c) => {
    const row = `talk:${c.id}`;
    delete notes[row];
    try {
      await Clak.invoke("conversations_open_folder", { agent: talks.agent.id, id: c.id });
    } catch (e) {
      notes[row] = String(e);
    }
    draw();
  };

  /// The command that resumes it by hand, in the folder where it took place: for the shell of
  /// a new terminal (zsh or bash on the Mac, PowerShell on Windows).
  const COMMANDS = {
    "claude-code": (c) => c.terminal && `claude --resume ${c.terminal}`,
    "claude-desktop": (c) => c.terminal && `claude --resume ${c.terminal}`,
    codex: (c) => `codex resume ${c.id}`,
    gemini: (c) => `gemini --resume ${c.id}`,
    opencode: (c) => `opencode --session ${c.id}`,
  };
  const resumeCommand = (c) => {
    const resume = COMMANDS[talks.agent.id]?.(c);
    if (!resume || !c.folder) return resume;
    return Clak.mac
      ? `cd '${c.folder.replace(/'/g, "'\\''")}' && ${resume}`
      : `cd '${c.folder.replace(/'/g, "''")}'; ${resume}`;
  };

  /// What was copied, under the row for a moment.
  const copied = {};
  const copyCommand = async (c) => {
    const row = `talk:${c.id}`;
    const command = resumeCommand(c);
    notes[row] = (await Kit.copy(command)) ? `Copied: ${command}` : "Couldn't copy it.";
    clearTimeout(copied[row]);
    copied[row] = setTimeout(() => { delete notes[row]; draw(); }, 4000);
    draw();
  };

  /// The name being typed: a new field at every drawing, which Kit.replace gives the focus
  /// and the selection of the one it replaces.
  const nameField = () => {
    const field = el("input", { type: "text", "aria-label": "Name", maxlength: "120", spellcheck: "false" });
    field.value = talks.renaming.value;
    field.addEventListener("input", () => { if (talks?.renaming) talks.renaming.value = field.value; });
    return field;
  };

  /// The row's other actions, in its "More" menu: only those this conversation allows.
  const moreItems = (c, where) => {
    const { rename, delete: deletes, export: exports, in_app: inApp } = can();
    const items = [
      { label: pinned.has(c.id) ? "Unpin" : "Pin to the top", run: () => togglePin(c) },
      rename ? { label: "Rename…", run: () => startRename(c) } : null,
      c.folder ? { label: `Open ${where}`, run: () => openFolder(c) } : null,
      resumeCommand(c) ? { label: "Copy the command that resumes it", run: () => copyCommand(c) } : null,
      exports && (!inApp || c.terminal) ? { label: "Save as Markdown…", run: () => exportTalk(c) } : null,
    ].filter(Boolean);
    if (deletes) items.push("-", { label: "Delete…", danger: true, run: () => askDelete(c) });
    return items;
  };

  const talkRow = (c, now, grouped) => {
    const row = `talk:${c.id}`;
    const renaming = talks.renaming?.id === c.id;
    const deleting = talks.deleting === c.id;
    const size = c.size ? ` · ${megabytes(c.size)}` : "";
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
        : [el("button", { class: Clak.mac ? "" : "accent", disabled: busy.has(row), title: can().in_app ? `Opens it in ${talks.agent.name}` : `Resumes it in a terminal, in ${where}`,
            onclick: () => launch(row, { agent: talks.agent.id, resume: c.id }) }, "Resume"),
          Editors.moreButton(() => moreItems(c, where))];
    return el("div", { class: Clak.mac ? "row setup talk" : "card setup talk", "data-id": row },
      el("div", { class: "lines" },
        renaming
          ? el("div", { class: "ed-dialog talk-name" }, field)
          : el("div", { class: "name" },
            pinned.has(c.id) ? el("span", { class: "talk-pin", title: "Pinned" }, Kit.icon("pin")) : null,
            el("strong", { class: "ellipsis", title: c.title }, c.title)),
        deleting
          ? el("div", { class: "quiet" }, `Delete it for good? ${talks.agent.name} won't find it either.`)
          : grouped
            ? el("div", { class: "quiet" }, when + size)
            : el("div", { class: "quiet ellipsis", title: c.folder || "" }, el("span", { class: "mono" }, where), ` · ${when}${size}`),
        note(row)),
      el("div", { class: "actions" }, spinner(row), buttons));
  };

  /// The way back to the agents: on the Mac a back button over the title, on Windows the
  /// breadcrumbs. On the right the filter; below, the switch that groups by folder and the
  /// cleanup.
  const talksHeader = (title) => {
    const search = el("label", { class: "search" }, Kit.icon("search"), filter);
    const cleanup = talks.agent.conversations?.delete
      ? el("button", { title: "Deletes the conversations not used for a while", onclick: toggleCleanup }, "Clean up…")
      : null;
    const grouping = el("div", { class: "talk-group", title: "The conversations under the folder where they took place" },
      el("span", null, "By folder"),
      Kit.toggle(byFolder, "Group by folder", (on) => {
        byFolder = on;
        try { localStorage.setItem(BY_FOLDER, on ? "1" : "0"); } catch { /* only for the next time */ }
        draw();
      }));
    const tools = el("div", { class: "talk-tools" }, grouping, cleanup);
    if (Clak.mac) {
      return [el("button", { class: "back", onclick: closeTalks }, Kit.icon("back"), el("span", null, "Agents")), Kit.title(title, search), tools];
    }
    return [el("header", { class: "title crumbs" },
      el("button", { class: "crumb", onclick: closeTalks, title: "Agents" }, "Agents"), Kit.icon("chevron", "sep"),
      el("h1", null, title), el("div", { class: "actions" }, search)), tools];
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
    // Found by the words in what was said: only for the words still in the filter.
    const found = talks.found?.words === words.join(" ") ? talks.found : null;
    let inText = 0;
    const matches = (talks.all || []).filter((c) => {
      const text = `${c.title} ${c.folder || ""}`.toLowerCase();
      if (words.every((w) => text.includes(w))) return true;
      if (!found?.ids?.has(c.id)) return false;
      inText++;
      return true;
    });
    const searchNote = !words.length || !can().search
      ? null
      : !found || !found.ids
        ? "Searching in what was said too…"
        : inText ? `${Clak.plural(inText, "more", "more")} found in what was said.` : null;
    const now = Date.now();
    const shown = matches.slice(0, SHOWN);
    // The pinned ones over the others, whichever the order: in their folder they show it.
    const top = shown.filter((c) => pinned.has(c.id));
    const rest = shown.filter((c) => !pinned.has(c.id));
    Kit.replace(talks.body,
      talks.cleanup ? cleanupBar() : null,
      notes.cleanup ? Kit.bar("info", notes.cleanup) : null,
      talks.error ? Kit.bar("err", talks.error) : null,
      searchNote ? el("div", { class: "quiet hint" }, searchNote) : null,
      talks.all === null
        ? el("div", { class: "quiet" }, "Reading the conversations…")
        : shown.length
          ? [top.length ? [el("div", { class: "section" }, el("h2", null, "Pinned"), el("span", { class: "count" }, String(top.length))),
              group(top.map((c) => talkRow(c, now, false)))] : null,
            top.length > 0 && rest.length > 0 && !byFolder ? el("div", { class: "section" }, el("h2", null, "Others")) : null,
            byFolder ? byFolders(rest, now) : group(rest.map((c) => talkRow(c, now, false)))]
          : Kit.empty("chat", words.length ? "No match" : "No conversations", words.length ? `Nothing with these words in the title or the folder${can().search ? ", or in what was said" : ""}` : `The conversations with ${a.name} show up here`),
      matches.length > SHOWN ? el("div", { class: "quiet hint" }, `The latest ${SHOWN} of ${matches.length}: the filter finds the others.`) : null,
      el("div", { class: "quiet hint" }, can().in_app
        ? `A session opens again in ${a.name}, authorized like a launch from here. ${a.name} renames and deletes its sessions itself.`
        : `A conversation resumes in a terminal, in the folder where it took place, authorized like a launch from here.`));
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
    if (!chosen) return el("div", { class: "pick-row", "data-id": c.id }, head);
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
    return el("div", { class: "pick-row", "data-id": c.id }, head,
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
    Kit.replace(host, el("div", { class: "card ed-dialog preset-editor" },
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
      presets.length ? group(presets.map((p) => presetRow(p, state))) : Kit.empty("agents", "None", "A preset launches an agent already allowed to use some programs"),
      usageSection());
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
    /// The hub's answer to "usage".
    usage(rows) {
      usage = rows || [];
      draw();
    },
  };
})();
