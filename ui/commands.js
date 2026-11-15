// A program's commands as Clak keeps them, and what the user decided about each: approved
// (it runs, as it is, for every agent the user lets use the program), asking every time, or
// off. A command that changed since it was approved shows what changed. Shared by the
// panel's pages; the hub answers `commands` with the list.
//
// Approving, or turning a command back on, is only for the app hosting the hub
// (`canApprove`): on the channel any process could ask, an agent included.
"use strict";

const Commands = (() => {
  const { el } = Clak;
  const D = Describe;

  const STATUS = {
    approved: ["Approved", "ok"],
    changed: ["Changed", "warn"],
    new: ["New", "accent"],
    ask: ["Asks every time", "off"],
    off: ["Off", "err"],
  };

  const badge = (c) => {
    if (c.missing_since) return Kit.badge("No longer offered", "off");
    const [text, tone] = STATUS[c.status] || [c.status, "off"];
    return Kit.badge(text, tone);
  };

  const lines = (items) =>
    el("dl", { class: "lines changes" }, items.flatMap((l) => [el("dt", null, l.label), el("dd", { class: "selectable" }, l.value)]));

  const approvedLine = (c, now) => {
    if (!c.approved) return null;
    const how = { question: "from a question", panel: "here", connect: "when it connected", restored: "from a backup", inherited: "from the previous version" }[c.approved.how] || "";
    return el("div", { class: "quiet" }, `Approved ${D.ago(c.approved.at, now)} ${how}`.trim());
  };

  /// One command: its state, what it does, what changed, and what can be done about it.
  const row = (c, key, options) => {
    const { send, canApprove, now } = options;
    // Approving names the version on screen: one that changed meanwhile isn't approved.
    const set = (state) => send({ type: "command_state", key, name: c.name, state, digest: c.digest });
    const buttons = [];
    if (!c.missing_since) {
      if (canApprove && c.status !== "approved" && c.state !== "off") {
        buttons.push(el("button", { class: "small accent", title: "It runs, as it is, for every agent you let use the program", onclick: () => set("approved") }, c.status === "changed" ? "Approve the change" : "Approve"));
      }
      if (canApprove && c.state !== "ask" && c.status !== "new") {
        buttons.push(el("button", { class: "small", title: "Each call asks you", onclick: () => set("ask") }, c.state === "off" ? "Turn on" : "Ask each time"));
      }
      if (c.state !== "off") {
        buttons.push(el("button", { class: "small", title: "Agents can't run it, nor see it", onclick: () => set("off") }, "Turn off"));
      }
    }
    return el("div", { class: "command-review", "data-id": `command:${c.name}` },
      el("div", { class: "name" }, el("span", { class: "mono strong" }, c.name), badge(c)),
      c.description ? el("div", { class: "quiet selectable" }, c.description) : null,
      (c.changes || []).length ? [el("div", { class: "caption" }, "What changed"), lines(c.changes)] : null,
      approvedLine(c, now),
      buttons.length ? el("div", { class: "buttons" }, buttons) : null);
  };

  /// The page's content: the counts and the actions for all, then each command, those to
  /// review first.
  const page = (data, options) => {
    if (!data) return [el("div", { class: "quiet" }, "Loading…")];
    const { send, canApprove } = options;
    const key = data.key;
    const list = [...(data.commands || [])];
    const order = { changed: 0, new: 1, ask: 2, approved: 3, off: 4 };
    list.sort((a, b) => (a.missing_since ? 9 : order[a.status] ?? 5) - (b.missing_since ? 9 : order[b.status] ?? 5));
    const offered = list.filter((c) => !c.missing_since);
    const approved = offered.filter((c) => c.status === "approved").length;
    const toReview = offered.filter((c) => c.status === "new" || c.status === "changed").length;
    const missing = list.length - offered.length;
    const actions = [
      canApprove && toReview
        ? el("button", { class: "accent", title: "Every command shown here, as it is", onclick: () => send({ type: "approve_all", key, commands: offered.map((c) => ({ name: c.name, digest: c.digest })) }) }, "Approve all")
        : null,
      approved ? el("button", { title: "Every command asks again", onclick: () => send({ type: "reset_commands", key }) }, "Reset approvals") : null,
      missing ? el("button", { onclick: () => send({ type: "forget_missing", key }) }, "Forget the ones no longer offered") : null,
    ].filter(Boolean);
    return [
      el("div", { class: "commands-summary" },
        el("div", { class: "quiet" },
          `${D.plural(offered.length, "command", "commands")} · ${approved} approved` + (toReview ? ` · ${toReview} to review` : "")),
        el("div", { class: "quiet hint" },
          "An approved command runs, as it is, for every agent you let use the program; if it changes, it asks again. Which agents may use the program is asked on its own."),
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
