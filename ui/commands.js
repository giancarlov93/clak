// A program's commands as Clak keeps them, and what the user decided about each: off, asking
// every time, or always approved (it runs, as it is, for every agent the user lets use the
// program). One row per command: its name, which opens what it does and what changed since
// it was approved, and the three states to choose from. Shared by the panel's pages; the hub
// answers `commands` with the list.
//
// Approving, or turning a command back on, is only for the app hosting the hub
// (`canApprove`): on the channel any process could ask, an agent included.
"use strict";

const Commands = (() => {
  const { el } = Clak;
  const D = Describe;

  /// The three states, as the hub names them: the icon, what the row says to a screen reader
  /// and on hover.
  const STATES = [
    ["off", "deny", "Disabled", "Disabled: agents can't run it, nor see it"],
    ["ask", "ask", "Ask every time", "Ask every time: each call asks you"],
    ["approved", "approve", "Always approved", "Always approved: it runs without asking, as it is, for every agent you let use the program. If it changes, it asks again"],
  ];

  /// What needs the user, next to the name: the rest is said by the state chosen.
  const NOTE = { changed: ["Changed", "warn"], new: ["New", "accent"] };

  /// The rows opened by hand, and the order the page was first drawn in: a command whose
  /// state changes stays where it is.
  const opened = new Set();
  let order = { key: null, names: [] };

  const lines = (items) =>
    el("dl", { class: "lines changes" }, items.flatMap((l) => [el("dt", null, l.label), el("dd", { class: "selectable" }, l.value)]));

  const approvedLine = (c, now) => {
    if (!c.approved) return null;
    const how = { question: "from a question", panel: "here", connect: "when it connected", restored: "from a backup", inherited: "from the previous version" }[c.approved.how] || "";
    // Approved once but asking now (or changed since): when it was, not that it is.
    const word = c.status === "approved" ? "Approved" : "Last approved";
    return el("div", { class: "quiet" }, `${word} ${D.ago(c.approved.at, now)} ${how}`.trim());
  };

  /// What a command does now: one that is new, or changed since it was approved, asks.
  const effective = (c) => (c.status === "approved" ? "approved" : c.status === "off" ? "off" : "ask");

  /// The three states, the one in force chosen. Approving names the version on screen: one
  /// that changed meanwhile isn't approved.
  const selector = (c, key, options) => {
    const { send, canApprove } = options;
    const now = effective(c);
    return el("div", { class: "segmented states", role: "radiogroup", "aria-label": `${c.name}: what agents may do` },
      STATES.map(([state, glyph, label, help]) => {
        const chosen = state === now;
        // Choosing "ask" again settles one that changed: it asks by the user's choice.
        const same = chosen && !(state === "ask" && c.status === "changed");
        const locked = state !== "off" && !canApprove;
        const title = locked ? `${label}: only in the Clak app where the hub runs`
          : state === "approved" && c.status === "changed" ? "Approve the change: it runs without asking, as it is now"
            : help;
        return el("button", {
          class: state + (chosen ? " on" : ""), type: "button", role: "radio", "aria-checked": String(chosen), "aria-label": label, title,
          disabled: locked && !chosen,
          onclick: () => !same && !locked && send({ type: "command_state", key, name: c.name, state, digest: c.digest }),
        }, Kit.icon(glyph));
      }));
  };

  /// One command: its name, which opens what it does, and its state.
  const row = (c, key, options) => {
    const id = `${key}/${c.name}`;
    const open = opened.has(id);
    const note = c.missing_since ? ["No longer offered", "off"] : NOTE[c.status];
    const changes = c.changes || [];
    const head = el("button", { class: "command-name", type: "button", "aria-expanded": String(open), title: "What it does" },
      Kit.icon("chevron", "command-chevron"),
      el("span", { class: "mono strong ellipsis" }, c.name),
      note ? Kit.badge(note[0], note[1]) : null);
    const node = el("div", { class: "command-row" + (open ? " open" : ""), "data-id": `command:${c.name}` },
      el("div", { class: "command-head" }, head, c.missing_since ? null : selector(c, key, options)),
      el("div", { class: "command-more" },
        c.description ? el("div", { class: "quiet selectable" }, c.description) : el("div", { class: "quiet" }, "No description"),
        changes.length ? [el("div", { class: "caption" }, "What changed"), lines(changes)] : null,
        approvedLine(c, options.now)));
    // Opening doesn't redraw: the list stays where it is.
    head.addEventListener("click", () => {
      const opening = !opened.has(id);
      if (opening) opened.add(id);
      else opened.delete(id);
      node.classList.toggle("open", opening);
      head.setAttribute("aria-expanded", String(opening));
    });
    return node;
  };

  /// The commands in the order the page opened with: those to review first, then as the
  /// program lists them. New ones go last; the page is asked again when it opens.
  const ordered = (key, list) => {
    if (order.key !== key) {
      const rank = (c) => (c.missing_since ? 2 : c.status === "changed" || c.status === "new" ? 0 : 1);
      order = { key, names: list.map((c, i) => [rank(c), i, c.name]).sort((a, b) => a[0] - b[0] || a[1] - b[1]).map((x) => x[2]) };
    }
    const at = new Map(order.names.map((name, i) => [name, i]));
    for (const c of list) if (!at.has(c.name)) at.set(c.name, order.names.push(c.name) - 1);
    return [...list].sort((a, b) => at.get(a.name) - at.get(b.name));
  };

  /// The page's content: the counts and the actions for all, then each command.
  const page = (data, options) => {
    if (!data) {
      order = { key: null, names: [] };
      return [el("div", { class: "quiet" }, "Loading…")];
    }
    const { send, canApprove } = options;
    const key = data.key;
    const list = ordered(key, data.commands || []);
    const offered = list.filter((c) => !c.missing_since);
    const count = (state) => offered.filter((c) => effective(c) === state).length;
    const review = offered.filter((c) => c.status === "new" || c.status === "changed");
    const approved = count("approved");
    const missing = list.length - offered.length;
    const actions = [
      canApprove && review.length
        ? el("button", { class: "accent", title: "The new and changed commands shown here, as they are", onclick: () => send({ type: "approve_all", key, commands: review.map((c) => ({ name: c.name, digest: c.digest })) }) }, "Approve the new and changed")
        : null,
      approved ? el("button", { title: "Every approved command asks again", onclick: () => send({ type: "reset_commands", key }) }, "Reset approvals") : null,
      missing ? el("button", { onclick: () => send({ type: "forget_missing", key }) }, "Forget the ones no longer offered") : null,
    ].filter(Boolean);
    const counts = [`${approved} approved`, count("ask") ? `${count("ask")} asking` : null, count("off") ? `${count("off")} disabled` : null,
      review.length ? `${review.length} to review` : null].filter(Boolean);
    return [
      el("div", { class: "commands-summary" },
        el("div", { class: "quiet" }, `${D.plural(offered.length, "command", "commands")} · ${counts.join(" · ")}`),
        actions.length ? el("div", { class: "buttons" }, actions) : null),
      list.length
        ? el("div", { class: "card group commands-review" }, list.map((c) => row(c, key, options)))
        : el("div", { class: "quiet" }, "No commands seen yet"),
    ];
  };

  /// The programs with commands to review, from the hub's state.
  const toReview = (state) => (state.commands || []).filter((p) => p.to_review > 0);

  return { page, toReview };
})();
