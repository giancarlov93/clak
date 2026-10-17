// The hub's question. The buttons light up after a moment, and neither of them is the
// default: an Enter meant for another window must not answer here.
"use strict";

(async () => {
  const { invoke, el } = Clak;
  const id = new URLSearchParams(location.search).get("id");
  const $ = (x) => document.getElementById(x);
  const ARMING_MS = 800;

  const request = await invoke("consent_request", { id });
  if (!request) {
    await invoke("consent_answer", { id, allow: false });
    return;
  }

  $("heading").textContent = request.title;
  $("question").textContent = request.question;
  if (request.warning) {
    $("warning").textContent = request.warning;
    $("warning").classList.remove("hidden");
  }

  const lines = (list, heading, items) => {
    const show = items && items.length > 0;
    $(heading).classList.toggle("hidden", !show);
    $(list).classList.toggle("hidden", !show);
    for (const line of items || []) {
      $(list).append(el("dt", null, line.label), el("dd", { class: "selectable" }, line.value));
    }
  };
  lines("who", "who-heading", request.who);
  lines("what", "what-heading", request.what);
  const tools = request.tools || [];
  $("tools-heading").classList.toggle("hidden", tools.length === 0);
  $("tools").classList.toggle("hidden", tools.length === 0);
  for (const tool of tools) {
    $("tools").append(
      el("div", { class: "tool" },
        el("div", null, el("span", { class: "name" }, tool.name), tool.note ? el("span", { class: "note" }, tool.note) : null),
        el("div", { class: "description" }, tool.description)),
    );
  }
  $("note").textContent = request.note || "";

  if (request.kind === "confirm") {
    // Not a permission: the answer to a server's question, for this command only.
    document.title = "Clak — confirmation";
    $("who-heading").textContent = "Who ran the command";
    $("what-heading").textContent = "Who's asking";
    $("allow").textContent = "Confirm";
    $("deny").textContent = "Cancel";
  } else if (request.kind === "change") {
    // A settings change proposed by an agent: it applies to that change alone.
    document.title = "Clak — settings";
    $("what-heading").textContent = "Change";
    $("tools-heading").textContent = "Endpoints";
    $("allow").textContent = "Apply";
    $("deny").textContent = "Cancel";
  } else {
    document.title = "Clak — permission";
  }

  let answered = false;
  const answer = (allow) => {
    if (answered) return;
    answered = true;
    invoke("consent_answer", { id, allow });
  };
  $("allow").addEventListener("click", () => answer(true));
  $("deny").addEventListener("click", () => answer(false));

  // The window takes the height of the content.
  const fit = () => invoke("consent_resize", { id, height: Math.ceil(document.body.scrollHeight) + (Clak.mac ? 28 : 40) });
  new ResizeObserver(fit).observe(document.body);
  fit();

  const shown = Date.now();
  const tick = () => {
    const armed = Date.now() - shown >= ARMING_MS;
    $("allow").disabled = $("deny").disabled = !armed || answered;
    const left = Math.max(0, Math.ceil(request.expires - Date.now() / 1000));
    $("countdown").textContent = `Withdrawn in ${left} s`;
  };
  tick();
  setInterval(tick, 250);
})();
