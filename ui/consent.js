// The hub's question. The buttons light up after a moment, and neither of them is the
// default: an Enter meant for another window must not answer here.
//
// Up front only what decides the answer: who is asking (with its real icon and who signed
// it), what it wants, the commands by name. Paths, pids and the process chain are under
// "Details".
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

  const kind = request.kind;
  const who = request.who || [];
  const what = request.what || [];
  const tools = request.tools || [];
  const value = (lines, label) => lines.find((l) => l.label === label)?.value;
  const file = (path) => String(path || "").split(/[\\/]/).pop();
  const isPath = (text) => /^(\/|[A-Za-z]:\\)/.test(text || "");

  // The question arrives in its window as the window opens.
  Clak.enter(document.body, { rise: 6 });
  $("heading").textContent = request.title;
  $("question").textContent = request.question;
  if (request.warning) {
    $("warning").textContent = request.warning;
    $("warning").classList.remove("hidden");
  }

  // The signature as a badge: who signed, in green; anything else, in amber.
  const signature = (text) => {
    if (!text) return null;
    const signer = text.startsWith("signed by ") ? text.slice("signed by ".length) : null;
    const good = signer || text === "signed";
    return el("span", { class: "sig " + (good ? "ok" : "warn"), title: good ? "Signed by" : "Signature" }, signer || text);
  };

  /// One party of the question: its icon, its name, the signature and one line about it.
  /// A program that runs in Java or Python says so: the signature is the runtime's.
  const party = (caption, name, exe, sig, detail, tint) => {
    // Until the real icon arrives, and for whoever has none: the initial on a colored tile.
    const icon = el("span", { class: "party-icon", style: `--tint:${tint}` }, String(name || "?").trim().charAt(0).toUpperCase());
    if (isPath(exe)) Clak.withIcon(icon, exe, 32);
    const runtime = isPath(exe) ? Clak.runtime(exe) : null;
    return el("div", { class: "party" },
      icon,
      el("div", { class: "party-lines" },
        el("div", { class: "caption" }, caption),
        el("div", { class: "party-name" }, el("strong", { class: "ellipsis" }, name),
          runtime ? el("span", { class: "sig runtime", title: `Runs in ${runtime}` }, runtime) : null, signature(sig)),
        detail ? el("div", { class: "quiet ellipsis", title: detail }, detail) : null));
  };

  const agentExe = value(who, "Executable");
  const folder = value(who, "Folder")?.replace(/ \(declared\)$/, "");
  const agentRow = () =>
    party("Agent", value(who, "Agent") || "Agent", agentExe, value(who, "Signature"),
      folder ? `in ${folder}` : isPath(agentExe) ? file(agentExe) : agentExe, "#ff9500");

  const rows = [];
  if (kind === "app") {
    // A program connecting: it is the one asking.
    rows.push(party("Program", value(what, "Program") || request.title, agentExe, value(who, "Signature"),
      isPath(agentExe) ? file(agentExe) : agentExe, "#007aff"));
  } else {
    if (who.length) rows.push(agentRow());
    if (kind === "change") {
      // The change itself, line by line: it is what is being decided.
      const dl = el("dl", { class: "lines change" });
      for (const line of what) dl.append(el("dt", null, line.label), el("dd", { class: "selectable" }, line.value));
      if (what.length) rows.push(dl);
    } else if (value(what, "Program")) {
      const service = value(what, "Service");
      const exe = value(what, "Executable");
      rows.push(party(kind === "confirm" ? "Asking" : "Wants to use", value(what, "Program"), exe, null,
        service || (isPath(exe) ? file(exe) : exe), "#007aff"));
    }
  }
  $("summary").replaceChildren(...rows);
  $("summary").classList.toggle("hidden", rows.length === 0);

  // The header shows who is asking, with its own icon when it has one.
  if (isPath(agentExe)) Clak.withIcon($("icon"), agentExe, 52);

  // The commands: few, or new and changed ones, in full; many, as names to tap.
  $("tools-block").classList.toggle("hidden", tools.length === 0);
  if (kind === "change") $("tools-heading").textContent = "Endpoints";
  const full = tools.length <= 2 || tools.some((t) => t.note);
  if (full) {
    $("tools").className = "card tools-full";
    $("tools").replaceChildren(...tools.map((t) =>
      el("div", { class: "tool" },
        el("div", null, el("span", { class: "name" }, t.name), t.note ? el("span", { class: "note" }, t.note) : null),
        t.description ? el("div", { class: "description" }, t.description) : null)));
  } else {
    $("tools-heading").append(el("span", { class: "count" }, String(tools.length)));
    const text = el("div", { class: "chip-text quiet hidden" });
    let open = null;
    const chips = tools.map((t) => {
      const chip = el("button", { class: "chip", type: "button", title: t.description || "" }, t.name);
      chip.addEventListener("click", () => {
        open?.classList.remove("on");
        if (open === chip) {
          open = null;
          text.classList.add("hidden");
          return;
        }
        open = chip;
        chip.classList.add("on");
        text.textContent = t.description || "No description.";
        text.classList.remove("hidden");
      });
      return chip;
    });
    $("tools").className = "tools-chips";
    $("tools").replaceChildren(el("div", { class: "chips" }, chips), text);
  }

  // Everything the hub said, for whoever wants to check.
  const lines = (list, items) => {
    $(list).replaceChildren(...items.flatMap((l) => [el("dt", null, l.label), el("dd", { class: "selectable" }, l.value)]));
    $(list).classList.toggle("hidden", items.length === 0);
  };
  lines("who", who);
  lines("what", kind === "change" ? [] : what);
  $("details").classList.toggle("hidden", who.length === 0 && (kind === "change" || what.length === 0));

  $("note").textContent = request.note || "";

  if (kind === "confirm") {
    // Not a permission: the answer to a server's question, for this command only.
    document.title = "Clak — confirmation";
    $("allow").textContent = "Confirm";
    $("deny").textContent = "Cancel";
  } else if (kind === "change") {
    // A settings change proposed by an agent: it applies to that change alone.
    document.title = "Clak — settings";
    $("allow").textContent = "Apply";
    $("deny").textContent = "Cancel";
  } else {
    document.title = "Clak — permission";
  }

  // A yes that holds from now on: offered by the hub only where it can recognize the agent
  // and the program next time. Never ticked from the start.
  if (request.always) {
    $("always-text").textContent = request.always;
    $("always-row").classList.remove("hidden");
  }

  // Several questions at once: which one this is.
  const showQueue = (q) => {
    const many = q && q.total > 1;
    $("queue").textContent = many ? `${q.position} of ${q.total} · ` : "";
    $("queue").classList.toggle("hidden", !many);
  };
  showQueue(request.queue);
  Clak.listen("consent-queue", (q) => q?.id === id && showQueue(q));

  let answered = false;
  const answer = (allow) => {
    if (answered) return;
    answered = true;
    invoke("consent_answer", { id, allow, always: allow && $("always").checked });
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
    $("allow").disabled = $("deny").disabled = $("always").disabled = !armed || answered;
    const left = Math.max(0, Math.ceil(request.expires - Date.now() / 1000));
    $("countdown").textContent = `Withdrawn in ${left} s`;
  };
  tick();
  setInterval(tick, 250);
})();
