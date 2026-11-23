// A command's question in full, in a window of its own, opened from the question's row: what
// changed since the user approved it, what the agent sends with it (as the hub cleaned it,
// secrets masked), what it does, what it takes. Nothing here answers: the question's window
// does, and this one closes with it.
"use strict";

(async () => {
  const { invoke, el } = Clak;
  const id = new URLSearchParams(location.search).get("id");
  const $ = (x) => document.getElementById(x);

  const request = await invoke("consent_request", { id });
  if (!request) {
    $("content").replaceChildren(el("p", { class: "quiet" }, "This question is no longer open."));
    return;
  }

  const tool = (request.tools || [])[0] || {};
  const value = (lines, label) => (lines || []).find((l) => l.label === label)?.value;
  const lines = (node, items) =>
    node.replaceChildren(...items.flatMap((l) => [el("dt", null, l.label), el("dd", { class: "selectable" }, l.value)]));
  const show = (block, on) => $(block).classList.toggle("hidden", !on);

  document.title = `Clak — ${tool.name || "command"}`;
  $("name").textContent = tool.name || "";
  if (tool.note) {
    $("tag").textContent = tool.note;
    $("tag").classList.remove("hidden");
  }
  const program = value(request.what, "Program");
  const agent = value(request.who, "Agent");
  $("context").textContent = [program && `in ${program}`, agent && `asked by ${agent}`].filter(Boolean).join(" · ");

  const changes = tool.changes || [];
  const args = request.arguments || [];
  const parameters = tool.parameters || [];
  lines($("changes"), changes);
  show("changes-block", changes.length > 0);
  lines($("arguments"), args);
  show("arguments-block", args.length > 0);
  $("description").textContent = tool.description || "";
  show("description-block", !!tool.description);
  lines($("parameters"), parameters);
  show("parameters-block", parameters.length > 0);

  Clak.enter($("content"), { rise: 6 });
})();
