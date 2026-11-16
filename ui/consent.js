// The hub's question. The buttons light up after a moment, and neither of them is the
// default: an Enter meant for another window must not answer here.
//
// The question to use a program offers "always" and the block. A command's question shows
// what the agent sends, and what changed since the user approved it, and asks how far a yes
// goes: this call only (the default), the command for the agent's session, or approved in
// Clak; it can block the program too.
//
// Up front only what decides the answer: who is asking (with its real icon and who signed
// it), what it wants, the commands by name. Paths, pids and the process chain are under
// "Details".
//
// An MCP server's question halfway through a command (`request.form` or `request.page`):
// its form, to fill in and send, or its page, which the app opens in the browser when the
// user says so (the address is the hub's, not this page's). A command that waits for the
// page then waits here too, with "Continue". These answers go with `consent_fill`.
"use strict";

(async () => {
  const { invoke, el } = Clak;
  const id = new URLSearchParams(location.search).get("id");
  const $ = (x) => document.getElementById(x);
  const ARMING_MS = 800;
  // A command asks often: the wait is shorter, still longer than a keypress.
  const CALL_ARMING_MS = 400;

  const request = await invoke("consent_request", { id });
  if (!request) {
    await invoke("consent_answer", { id, allow: false });
    return;
  }

  const kind = request.kind;
  const who = request.who || [];
  const what = request.what || [];
  const tools = request.tools || [];
  const args = request.arguments || [];
  const choices = request.choices;
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
      rows.push(party(kind === "confirm" ? "Asking" : kind === "call" ? "Wants to run a command of" : "Wants to use", value(what, "Program"), exe, null,
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
  if (kind === "call") $("tools-heading").textContent = "Command";
  // In full when few, when one is new or changed, and when approving them is offered: the
  // user approves what they read, parameters included.
  const full = tools.length <= 2 || tools.some((t) => t.note || (t.parameters || []).length) || !!choices?.always;
  if (full) {
    $("tools").className = "card tools-full";
    $("tools").replaceChildren(...tools.map((t) =>
      el("div", { class: "tool" },
        el("div", null, el("span", { class: "name" }, t.name), t.note ? el("span", { class: "note" }, t.note) : null),
        t.description ? el("div", { class: "description" }, t.description) : null,
        // What changed since the user approved it.
        (t.changes || []).length
          ? el("dl", { class: "lines changes" }, t.changes.flatMap((l) => [el("dt", null, l.label), el("dd", { class: "selectable" }, l.value)]))
          : null,
        // What it takes.
        (t.parameters || []).length
          ? el("dl", { class: "lines parameters" }, t.parameters.flatMap((l) => [el("dt", null, l.label), el("dd", { class: "selectable" }, l.value)]))
          : null)));
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

  // What the agent sends with the command, as the hub cleaned it (secrets masked).
  if (args.length) {
    $("arguments").replaceChildren(...args.flatMap((l) => [el("dt", null, l.label), el("dd", { class: "selectable" }, l.value)]));
    $("arguments-block").classList.remove("hidden");
  }

  // A server's question: a form, a page, or a plain confirmation.
  const page = request.page;
  const filled = request.form ? ClakForm.render(request.form, el) : null;
  const asking = !!(page || filled);
  if (page) {
    document.title = "Clak — page to open";
    $("page").replaceChildren(ClakForm.address(page, el));
    $("page-state").textContent = page.wait
      ? "Clak opens it in your browser. When you're done there, come back and choose Continue."
      : "Clak opens it in your browser.";
    $("page-block").classList.remove("hidden");
    $("allow").textContent = "Open in browser";
    $("deny").textContent = "Decline";
  } else if (filled && !filled.empty) {
    document.title = "Clak — information requested";
    $("form").replaceChildren(filled.node);
    $("form-block").classList.remove("hidden");
    $("allow").textContent = "Send";
    $("deny").textContent = "Decline";
  } else if (kind === "confirm") {
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

  // How far a yes goes, for a command: the first choice (this call only), unless the user
  // picks more. "Always", for the question to use a program: a yes that holds from now on,
  // offered by the hub only where it can recognize the agent and the program next time,
  // never ticked from the start. The block closes the program to the agent.
  const scopes = choices?.scopes || [];
  let scope = scopes[0]?.value || "once";
  if (scopes.length) {
    const option = (choice) => {
      const input = el("input", { type: "radio", name: "scope", value: choice.value });
      input.checked = choice.value === scope;
      input.addEventListener("change", () => { scope = choice.value; });
      return el("label", null, input, el("span", null, choice.label));
    };
    $("scope").append(...scopes.map(option));
    $("scope").classList.remove("hidden");
  }
  if (choices?.always) {
    $("always-text").textContent = choices.always;
    $("always-row").classList.remove("hidden");
  }
  if (choices?.block) {
    $("block").textContent = choices.block;
    $("block").classList.remove("hidden");
  }

  // Several questions at once: which one this is.
  // When the buttons start counting toward working.
  let shown = Date.now();
  // Moved in the stack (another question answered): the buttons wait again, so a second
  // click meant for the one that went doesn't answer this one.
  let position = request.queue?.position;
  const showQueue = (q) => {
    if (q && position !== undefined && q.position !== position) shown = Date.now();
    if (q) position = q.position;
    const many = q && q.total > 1;
    $("queue").textContent = many ? `${q.position} of ${q.total} · ` : "";
    $("queue").classList.toggle("hidden", !many);
  };
  showQueue(request.queue);
  Clak.listen("consent-queue", (q) => q?.id === id && showQueue(q));

  let answered = false;
  const answer = (allow, block = false) => {
    if (answered) return;
    answered = true;
    invoke("consent_answer", { id, allow, always: allow && !!choices?.always && $("always").checked, scope, block });
  };

  // A server's question. A form that doesn't fit stays, with what's wrong.
  const problem = (text) => {
    $("form-error").textContent = text || "";
  };
  let waiting = false;
  const reply = async (action) => {
    if (answered) return;
    let content;
    if (action === "accept" && filled && !filled.empty) {
      const read = filled.read();
      if (read.error) {
        problem(read.error);
        return;
      }
      content = read.content;
    }
    problem("");
    answered = true;
    tick();
    try {
      await invoke("consent_fill", { id, action, content });
    } catch (error) {
      answered = false;
      problem(String(error));
    }
  };
  const open = async () => {
    if (answered) return;
    answered = true;
    tick();
    try {
      if (await invoke("consent_open", { id })) {
        // The command waits for the user to finish in the browser.
        waiting = true;
        answered = false;
        shown = Date.now();
        $("page-state").textContent = "Finish in your browser, then choose Continue. If the server says you're done, this goes by itself.";
        $("allow").textContent = "Continue";
        $("deny").textContent = "Cancel";
        // A second click meant for "Open" doesn't say the user is done.
        tick();
      }
    } catch (error) {
      answered = false;
      $("page-state").textContent = String(error);
    }
  };

  $("allow").addEventListener("click", () => {
    if (!asking) answer(true);
    else if (page && !waiting) open();
    else reply("accept");
  });
  $("deny").addEventListener("click", () => (asking ? reply("decline") : answer(false)));
  $("block").addEventListener("click", () => answer(false, true));

  // The window appears once the page has taken its measure: the buttons count from then.
  await Clak.listen("consent-shown", (shownId) => {
    if (shownId === id) shown = Date.now();
  });

  // As tall as the content wants: the window is capped to the screen, and the content scrolls.
  // Measured on the sheet, not on the box that scrolls it: that box takes the window's
  // height, so the window could only grow. Around it, what the window has besides (the
  // buttons, the borders) is what isn't the box.
  const main = $("content");
  const sheet = $("sheet");
  const fit = () => {
    const box = getComputedStyle(main);
    const content = sheet.getBoundingClientRect().height + parseFloat(box.paddingTop) + parseFloat(box.paddingBottom);
    invoke("consent_resize", { id, height: Math.ceil(innerHeight - main.clientHeight + content) });
  };
  const resized = new ResizeObserver(fit);
  resized.observe(sheet);
  resized.observe(document.querySelector("footer"));
  fit();

  const arming = kind === "call" ? CALL_ARMING_MS : ARMING_MS;
  function tick() {
    const armed = Date.now() - shown >= arming;
    $("allow").disabled = $("deny").disabled = $("always").disabled = $("block").disabled = !armed || answered;
    filled?.disable(!armed || answered);
    for (const input of $("scope").querySelectorAll("input")) input.disabled = answered;
    const left = Math.max(0, Math.ceil(request.expires - Date.now() / 1000));
    $("countdown").textContent = `Withdrawn in ${left} s`;
  }
  tick();
  setInterval(tick, 250);
})();
