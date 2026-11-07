// The backup, in Settings › General: off until the user turns it on, with a folder and a
// password of theirs; then written by itself after every change (backup.rs). And the
// restore from a file: the password, then each item as it stands on this computer, ticked
// or not, with what to point to (an application moved) or to change afterwards (a program
// or a folder that isn't here). What the backup holds in clear never comes to this page.
//
// The fields are made once and kept across redraws: what the user is typing stays there
// when the hub's state redraws the tab.
"use strict";

const Backup = (() => {
  const el = Clak.el;
  const host = el("div", { class: "backup" });
  /// `{ enabled, folder, keep, password, last }` from backup_status.
  let data = null;
  let message = "";
  let busy = false;
  /// The form: turning it on, or changing the folder or the password.
  let form = null;
  /// The restore: { path, header, stage: "password" | "plan" | "done", plan, chosen, results, error, busy }.
  let restore = null;

  const field = (label, type = "password") =>
    el("input", { type, "aria-label": label, autocomplete: "new-password", spellcheck: "false" });
  const fields = {
    password: field("Password"),
    again: field("Password again"),
    unlock: field("Password of the backup"),
  };
  fields.unlock.autocomplete = "current-password";
  fields.unlock.addEventListener("keydown", (e) => { if (e.key === "Enter") open(); });
  fields.again.addEventListener("keydown", (e) => { if (e.key === "Enter") save(); });

  const refresh = async () => {
    try {
      data = await Clak.invoke("backup_status");
    } catch (e) {
      message = String(e);
    }
    draw();
  };

  Clak.listen("backup", (status) => {
    data = status;
    draw();
  });

  const when = (seconds) => {
    const d = new Date(seconds * 1000);
    return d.toDateString() === new Date().toDateString()
      ? `today at ${d.toLocaleTimeString([], { hour: "2-digit", minute: "2-digit" })}`
      : d.toLocaleString([], { day: "numeric", month: "short", year: "numeric", hour: "2-digit", minute: "2-digit" });
  };

  const configure = async (change) => {
    busy = true;
    message = "";
    draw();
    try {
      data = await Clak.invoke("backup_configure", change);
      busy = false;
      return true;
    } catch (e) {
      message = String(e);
      busy = false;
      return false;
    } finally {
      draw();
    }
  };

  // --- turning it on, the folder, the password ---------------------------------------

  /// `what`: "on" (everything), "folder" or "password".
  const edit = (what) => {
    form = { what, folder: data?.folder || "", error: "" };
    fields.password.value = "";
    fields.again.value = "";
    draw();
    if (what !== "folder") setTimeout(() => fields.password.focus(), 0);
  };

  const close = () => {
    form = null;
    fields.password.value = "";
    fields.again.value = "";
    draw();
  };

  const chooseFolder = async () => {
    const chosen = await Clak.invoke("backup_choose_folder").catch(() => null);
    if (chosen && form) {
      form.folder = chosen;
      form.error = "";
      draw();
    }
  };

  const save = async () => {
    if (!form || busy) return;
    const change = {};
    if (form.what !== "password") {
      if (!form.folder) { form.error = "Choose the folder the backups go to."; return draw(); }
      change.folder = form.folder;
    }
    if (form.what !== "folder") {
      const password = fields.password.value;
      if (password.length < 8) { form.error = "The password must be at least 8 characters long."; return draw(); }
      if (password !== fields.again.value) { form.error = "The two passwords aren't the same."; return draw(); }
      change.password = password;
    }
    if (form.what === "on") change.enabled = true;
    form.error = "";
    if (await configure(change)) close();
    else if (form) { form.error = message; message = ""; draw(); }
  };

  const formCard = () => {
    const titles = { on: "Automatic backup", folder: "Where the backups go", password: "A new password" };
    const rows = [el("div", { class: "editor-title" }, el("strong", null, titles[form.what]))];
    if (form.what !== "password") {
      rows.push(el("div", { class: "field" }, el("span", null, "Folder"),
        el("div", { class: "folder-line" },
          form.folder
            ? el("span", { class: "mono ellipsis", title: form.folder }, form.folder)
            : el("span", { class: "quiet-text" }, "None chosen"),
          Kit.link("Choose…", null, chooseFolder))));
    }
    if (form.what !== "folder") {
      rows.push(el("label", { class: "field" }, el("span", null, "Password"), fields.password));
      rows.push(el("label", { class: "field" }, el("span", null, "Again"), fields.again));
      rows.push(el("div", { class: "quiet hint" },
        "Asked when you restore a backup. Clak doesn't keep it: without it the backups can't be opened, by anyone.",
        form.what === "password" ? " The backups already written keep the password they were written with." : ""));
    } else {
      rows.push(el("div", { class: "quiet hint" }, "A folder another computer or a cloud keeps a copy of is a good one: the backups are locked with your password."));
    }
    return el("div", { class: "card ed-dialog preset-editor" }, rows,
      form.error ? Kit.bar("err", form.error) : null,
      el("div", { class: "buttons" },
        el("button", { onclick: close, disabled: busy }, "Cancel"),
        el("button", { class: "accent", onclick: save, disabled: busy },
          busy ? "Locking…" : form.what === "on" ? "Turn on" : "Save")));
  };

  // --- the restore ------------------------------------------------------------------

  const pick = async () => {
    message = "";
    try {
      const chosen = await Clak.invoke("backup_restore_pick");
      if (!chosen) return;
      restore = { ...chosen, stage: "password", error: "" };
      fields.unlock.value = "";
      draw();
      setTimeout(() => fields.unlock.focus(), 0);
    } catch (e) {
      message = String(e);
      draw();
    }
  };

  const open = async () => {
    if (!restore || restore.busy || restore.stage !== "password") return;
    restore.busy = true;
    restore.error = "";
    draw();
    try {
      const plan = await Clak.invoke("backup_restore_open", { path: restore.path, password: fields.unlock.value });
      fields.unlock.value = "";
      restore = { ...restore, stage: "plan", plan, chosen: chosenOf(plan) };
    } catch (e) {
      restore.error = String(e);
    }
    restore.busy = false;
    draw();
  };

  const chosenOf = (plan) => new Set(plan.items.filter((i) => i.chosen).map((i) => i.id));

  const locate = async (id) => {
    restore.error = "";
    try {
      const plan = await Clak.invoke("backup_restore_locate", { id });
      if (plan) {
        // The ticks the user gave stay; the item pointed to is ticked if it can be.
        const chosen = new Set([...restore.chosen].filter((c) => plan.items.some((i) => i.id === c && canTick(i))));
        const item = plan.items.find((i) => i.id === id);
        if (item?.chosen) chosen.add(id);
        restore = { ...restore, plan, chosen };
      }
    } catch (e) {
      restore.error = String(e);
    }
    draw();
  };

  const apply = async () => {
    if (!restore || restore.busy) return;
    restore.busy = true;
    restore.error = "";
    draw();
    try {
      const results = await Clak.invoke("backup_restore_apply", { ids: [...restore.chosen] });
      restore = { ...restore, stage: "done", results };
    } catch (e) {
      restore.error = String(e);
    }
    restore.busy = false;
    draw();
  };

  const cancel = () => {
    if (restore && restore.stage !== "done") Clak.invoke("backup_restore_cancel").catch(() => {});
    restore = null;
    fields.unlock.value = "";
    draw();
  };

  const canTick = (i) => ["ready", "secret", "warn"].includes(i.state);

  const GROUPS = [
    ["app", "Allowed applications"],
    ["connector", "MCP servers"],
    ["api", "APIs"],
    ["preset", "Presets"],
    ["rule", "Always allow"],
  ];

  const from = (h) => {
    const systems = { windows: "Windows", macos: "macOS", linux: "Linux" };
    const parts = [h.computer || null, systems[h.system] || h.system || null].filter(Boolean);
    return `${parts.length ? `From ${parts.join(", ")}, ` : ""}${when(h.created)}${h.clak ? ` · Clak ${h.clak}` : ""}`;
  };

  const restoreLabel = () => {
    const n = restore.chosen.size;
    return n ? `Restore ${n}` : "Restore";
  };

  const itemRow = (i, go) => {
    const tick = canTick(i);
    const box = el("input", { type: "checkbox", "aria-label": i.name, disabled: !tick || restore.busy });
    box.checked = tick && restore.chosen.has(i.id);
    box.addEventListener("change", () => {
      if (box.checked) restore.chosen.add(i.id);
      else restore.chosen.delete(i.id);
      go.textContent = restoreLabel();
      go.disabled = !restore.chosen.size;
    });
    const tone = i.state === "blocked" ? "danger" : i.state === "warn" ? "caution-text" : "quiet";
    return el("label", { class: "pick import-row", "data-id": `restore:${i.id}` }, box,
      el("div", { class: "lines" },
        el("div", { class: "ed-title" }, el("strong", null, i.name || "(no name)")),
        i.detail ? el("div", { class: "mono quiet ellipsis", title: i.detail }, i.detail) : null,
        i.message ? el("div", { class: tone }, i.message) : null,
        i.locate ? Kit.link(i.state === "blocked" ? "Point to where it is…" : "Point elsewhere…", null, (e) => { e.preventDefault(); locate(i.id); }) : null));
  };

  const restoreCard = () => {
    const r = restore;
    const head = el("div", { class: "editor-title" }, el("strong", null, "Restore a backup"),
      el("div", { class: "quiet" }, from(r.header)));
    if (r.stage === "password") {
      return el("div", { class: "card ed-dialog preset-editor" }, head,
        el("div", { class: "field" }, el("span", null, "File"),
          el("span", { class: "mono ellipsis", title: r.path }, r.path)),
        el("label", { class: "field" }, el("span", null, "Password"), fields.unlock),
        r.error ? Kit.bar("err", r.error) : null,
        el("div", { class: "buttons" },
          el("button", { onclick: cancel, disabled: r.busy }, "Cancel"),
          el("button", { class: "accent", onclick: open, disabled: r.busy }, r.busy ? "Opening…" : "Open")));
    }
    if (r.stage === "done") {
      // As the list it came from, grouped the same way: what happened to each one.
      const result = (o) => el("div", { class: "import-row result-row", "data-id": `restored:${o.id}` },
        Kit.icon(o.ok ? "ok" : "err", o.ok ? "done" : "failed"),
        el("div", { class: "lines" },
          el("div", { class: "ed-title" }, el("strong", null, o.name || "(no name)")),
          el("div", { class: o.ok ? "quiet" : "danger" }, o.message)));
      return el("div", { class: "card ed-dialog import-card" }, head,
        r.results.length
          ? GROUPS.map(([kind, title]) => {
            const done = r.results.filter((o) => o.kind === kind);
            return done.length
              ? el("div", { class: "import-source" }, el("div", { class: "section" }, el("h2", null, title)), done.map(result))
              : null;
          })
          : el("div", { class: "quiet" }, "Nothing was restored."),
        el("div", { class: "buttons" }, el("button", { class: "accent", onclick: cancel }, "Done")));
    }
    const go = el("button", { class: "accent", disabled: !r.chosen.size || r.busy, onclick: apply }, r.busy ? "Restoring…" : restoreLabel());
    return el("div", { class: "card ed-dialog import-card" }, head,
      r.plan.foreign
        ? Kit.bar("warn", "This backup comes from another system: each path of that computer was checked here.")
        : null,
      el("div", { class: "quiet hint" }, "Nothing here is removed or replaced. An application or a rule is restored only if its program is the same one at the same place; a server whose program isn't here comes back turned off."),
      GROUPS.map(([kind, title]) => {
        const items = r.plan.items.filter((i) => i.kind === kind);
        return items.length
          ? el("div", { class: "import-source" }, el("div", { class: "section" }, el("h2", null, title)), items.map((i) => itemRow(i, go)))
          : null;
      }),
      r.error ? Kit.bar("err", r.error) : null,
      el("div", { class: "buttons" },
        el("button", { onclick: cancel, disabled: r.busy }, "Cancel"),
        go));
  };

  // --- the rows ---------------------------------------------------------------------

  const setting = (glyph, tint, title, detail, control, options = {}) =>
    el("div", { class: "setting", title: options.help },
      Clak.mac ? Kit.tile(glyph, 28, tint) : Kit.icon(glyph, "lead"),
      el("div", { class: "lines" }, el("div", null, title),
        detail ? el("div", { class: (options.tone || "quiet") + " selectable" + (options.mono ? " mono" : "") }, detail) : null),
      control ? el("div", { class: "actions" }, control) : null);

  const toggle = (on) => {
    if (on && !(data.password && data.folder)) return edit("on");
    configure({ enabled: on });
  };

  /// A backup being written from Back up now.
  let backing = false;

  const backUpNow = async () => {
    backing = true;
    message = "";
    draw();
    try {
      data = await Clak.invoke("backup_now");
    } catch (e) {
      message = String(e);
    }
    backing = false;
    draw();
  };

  /// When the last one was written, or why it couldn't be.
  const lastLine = () => {
    const last = data.last;
    if (!last) return { text: "None yet", tone: "quiet" };
    if (last.error) return { text: last.error, tone: "danger" };
    const when_ = when(last.at);
    return { text: when_.charAt(0).toUpperCase() + when_.slice(1), tone: "quiet" };
  };

  const rows = () => {
    const ready = data.password && data.folder;
    const last = lastLine();
    const list = [
      setting("sync", "#34c759", "Back up automatically",
        data.enabled ? "After every change: servers, APIs, presets and permissions, with their tokens"
          : "Servers, APIs, presets and permissions, with their tokens, locked with a password of yours",
        Kit.toggle(data.enabled, "Back up automatically", toggle, busy)),
    ];
    if (ready) {
      list.push(setting("folder", "#007aff", "Folder", data.folder,
        el("button", { onclick: () => edit("folder"), disabled: busy }, "Change…"), { mono: true }));
      const keep = el("select", { "aria-label": "Backups kept", disabled: busy },
        [3, 5, 10, 20, 50].concat([3, 5, 10, 20, 50].includes(data.keep) ? [] : [data.keep]).sort((a, b) => a - b)
          .map((n) => el("option", { value: String(n) }, `The last ${n}`)));
      keep.value = String(data.keep);
      keep.addEventListener("change", () => configure({ keep: Number(keep.value) }));
      list.push(setting("history", "#8e8e93", "Backups kept", "The older ones are removed from the folder", keep));
      list.push(setting("lock", "#5856d6", "Password", "Asked when you restore. Clak doesn't keep it",
        el("button", { onclick: () => edit("password"), disabled: busy }, "Change…")));
      list.push(setting("upload", "#34aadc", "Last backup", last.text,
        el("button", { onclick: backUpNow, disabled: busy || backing }, backing ? "Backing up…" : "Back up now"),
        { tone: last.tone }));
    }
    list.push(setting("restore", "#ff9500", "Restore", "From a backup file, even from another computer: each item is checked here first",
      el("button", { onclick: pick, disabled: !!restore || Panel.state().offline }, "Restore…")));
    return list;
  };

  const draw = () => {
    if (!data) return;
    Kit.replace(host,
      form ? formCard() : null,
      restore ? restoreCard() : null,
      el("div", { class: "card group settings" }, rows()),
      message ? Kit.bar("err", message) : null);
  };

  return {
    /// The section's content: the same node every time, drawn here.
    node: () => host,
    refresh,
    draw,
  };
})();
