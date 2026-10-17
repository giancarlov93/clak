// The Log tab: the hub's log, oldest rows at the top and anchored at the bottom like
// a terminal. Filtering doesn't touch the rows; the noes and the errors stand out at once.
// On Windows rows are selected (click, Ctrl, Shift) and Ctrl+C copies them with the time; on the Mac
// the text is selected as in an AppKit list.
"use strict";

(() => {
  const el = Clak.el;
  const MAX = 500;
  let rows = []; // { time, text, kind, node }
  let list, empty, filter;
  let anchor = -1;

  const matches = (row) => {
    const f = filter.value.trim().toLowerCase();
    return !f || row.text.toLowerCase().includes(f);
  };

  const atBottom = () => list.scrollHeight - list.scrollTop - list.clientHeight < 24;

  const refresh = () => {
    let visible = 0;
    for (const row of rows) {
      const show = matches(row);
      row.node.classList.toggle("hidden", !show);
      if (show) visible++;
    }
    empty.textContent = rows.length === 0 ? "Empty" : "No matches";
    empty.classList.toggle("hidden", visible > 0);
  };

  const shown = () => rows.filter((r) => !r.node.classList.contains("hidden"));

  /// Click: one row; Ctrl: one more; Shift: from the previous one to this one.
  const pick = (row, e) => {
    if (Clak.mac) return;
    const visible = shown();
    const at = visible.indexOf(row);
    if (e.shiftKey && anchor >= 0) {
      const [a, b] = [Math.min(anchor, at), Math.max(anchor, at)];
      if (!e.ctrlKey) for (const r of rows) r.node.classList.remove("selected");
      visible.slice(a, b + 1).forEach((r) => r.node.classList.add("selected"));
    } else if (e.ctrlKey) {
      row.node.classList.toggle("selected");
      anchor = at;
    } else {
      for (const r of rows) r.node.classList.toggle("selected", r === row);
      anchor = at;
    }
  };

  const append = (items) => {
    const follow = atBottom();
    for (const item of items) {
      const row = { time: Describe.clock(item.ts), text: item.text, kind: Describe.logKind(item.text) };
      row.node = el("div", { class: "log-row" + (row.kind ? " " + row.kind : ""), role: "option" },
        el("span", { class: "time mono" }, row.time), el("span", { class: "text mono" }, row.text));
      row.node.addEventListener("click", (e) => pick(row, e));
      row.node.classList.toggle("hidden", !matches(row));
      rows.push(row);
      list.append(row.node);
    }
    while (rows.length > MAX) rows.shift().node.remove();
    refresh();
    if (follow) list.scrollTop = list.scrollHeight;
  };

  const clear = () => {
    rows = [];
    anchor = -1;
    list.replaceChildren();
    refresh();
  };

  Panel.tab({
    id: "log",
    title: "Log",
    icon: Kit.navIcon("log", Kit.TINTS.log),
    order: 60,
    mount(page) {
      page.classList.add("fill");
      filter = el("input", { type: "search", placeholder: "Filter", "aria-label": "Filter", spellcheck: "false",
        title: "Connections, permissions, revocations. Command contents are never logged." });
      filter.addEventListener("input", refresh);
      const box = el("label", { class: "search" }, Kit.icon("search"), filter);
      list = el("div", { class: "log-list" + (Clak.mac ? " selectable" : ""), role: "listbox", "aria-label": "Log", "aria-multiselectable": "true", tabindex: "0" });
      empty = el("div", { class: "log-empty" }, "Empty");
      // Ctrl+C copies the selected rows, with the time, as they read.
      list.addEventListener("keydown", (e) => {
        if (Clak.mac || !(e.ctrlKey && e.key.toLowerCase() === "c")) return;
        const picked = rows.filter((r) => r.node.classList.contains("selected"));
        if (!picked.length) return;
        Kit.copy(picked.map((r) => `${r.time}  ${r.text}`).join("\r\n"));
        e.preventDefault();
      });
      page.append(Kit.title("Log", box, Clak.mac ? null : Kit.iconButton("trash", "Clear", clear)),
        el("div", { class: "card group log" }, list, empty));
    },
    shown() {
      list.scrollTop = list.scrollHeight;
    },
    log(items, initial) {
      append(items);
      if (initial) list.scrollTop = list.scrollHeight;
    },
  });
})();
