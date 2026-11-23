// Clak's web page: the hub's questions and what is allowed now. Everything that comes from
// the hub (and so from agents and programs) goes into the page as text, never as markup.
"use strict";

const $ = (id) => document.getElementById(id);

const el = (tag, props, ...children) => {
  const node = document.createElement(tag);
  for (const [key, value] of Object.entries(props || {})) {
    if (value === undefined || value === null || value === false) continue;
    if (key === "class") node.className = value;
    else if (key.startsWith("on")) node.addEventListener(key.slice(2), value);
    else node.setAttribute(key, value === true ? "" : value);
  }
  for (const child of children.flat(Infinity)) {
    if (child === undefined || child === null || child === false) continue;
    node.append(child instanceof Node ? child : document.createTextNode(String(child)));
  }
  return node;
};

/// Only what this page itself sends carries the header: the server refuses the rest.
const post = (path, body) =>
  fetch(path, {
    method: "POST",
    headers: { "content-type": "application/json", "x-clak": "1" },
    body: JSON.stringify(body || {}),
    credentials: "same-origin",
  });

const ago = (since, now) => {
  const s = Math.max(0, Math.round(now - since));
  if (s < 60) return `${s} s ago`;
  if (s < 3600) return `${Math.round(s / 60)} min ago`;
  if (s < 86400) return `${Math.round(s / 3600)} h ago`;
  return `${Math.round(s / 86400)} days ago`;
};

/// The question the link points to: `/q/<id>`.
const wanted = decodeURIComponent((location.pathname.match(/^\/q\/([^/]+)$/) || [])[1] || "");
let wantedSeen = false;

/// The questions on screen, by id: kept across refreshes, so a ticked box and the moment
/// the buttons light up stay as they are.
const shown = new Map();

const notice = (text) => {
  const node = $("notice");
  node.textContent = text || "";
  node.hidden = !text;
};

const showLogin = (message) => {
  $("app").hidden = true;
  $("logout").hidden = true;
  $("login").hidden = false;
  $("login-error").textContent = message || "";
  $("password").focus();
};

const lines = (items) =>
  items && items.length ? el("dl", { class: "lines" }, items.map((l) => [el("dt", {}, l.label), el("dd", {}, l.value)])) : null;

/// An MCP server's question halfway through a command: its form, sent once it fits; its page,
/// a link that opens in a tab of its own; a plain confirmation. Typed values stay across
/// refreshes, since the card does.
const serverCard = (q) => {
  const filled = q.form ? ClakForm.render(q.form, el) : null;
  const page = q.page;
  const problem = el("p", { class: "field-error", role: "alert" });
  const left = el("p", { class: "meta" });
  let armed = false;
  let waiting = !!q.opened;

  const send = async (action) => {
    if (!armed) return;
    let content;
    if (action === "accept" && filled && !filled.empty) {
      const read = filled.read();
      if (read.error) {
        problem.textContent = read.error;
        return;
      }
      content = read.content;
    }
    problem.textContent = "";
    const response = await post("/api/answer", { id: q.id, action, content });
    if (response.status === 401) return showLogin("The session is over: log in again.");
    const body = await response.json().catch(() => ({}));
    if (response.status === 400) {
      problem.textContent = body.error || "The answer didn't go through.";
      return;
    }
    if (!response.ok) notice(body.error || "The answer didn't go through.");
    else if (body.waits) {
      waiting = true;
      // "Continue" takes the link's place: a second tap meant for it doesn't say the user is done.
      arm();
      draw();
      return;
    } else notice(action === "decline" || action === "cancel" ? `Declined: ${q.title}` : `Answered: ${q.title}`);
    refresh();
  };

  const arm = () => {
    armed = false;
    accept.disabled = decline.disabled = true;
    opening?.classList.add("disabled");
    filled?.disable(true);
    setTimeout(() => {
      armed = true;
      accept.disabled = decline.disabled = false;
      opening?.classList.remove("disabled");
      filled?.disable(false);
    }, 1000);
  };
  const accept = el("button", { class: "primary", disabled: true }, filled && !filled.empty ? "Send" : "Confirm");
  const decline = el("button", { class: "deny", disabled: true }, filled && !filled.empty ? "Decline" : "Cancel");
  accept.addEventListener("click", () => send("accept"));
  decline.addEventListener("click", () => send("decline"));
  // The page: a link to a tab of its own, which the browser keeps apart from this one.
  const opening = page
    ? el("a", { class: "button primary disabled", href: page.url, target: "_blank", rel: "noopener noreferrer" }, "Open in browser")
    : null;
  opening?.addEventListener("click", (event) => {
    if (!armed) return event.preventDefault();
    send("open");
  });
  const state = el("p", { class: "note" });
  const actions = el("div", { class: "actions" });
  const draw = () => {
    if (page && !waiting) {
      state.textContent = page.wait ? "It opens in a new tab. When you're done there, come back here and choose Continue." : "It opens in a new tab.";
      decline.textContent = "Decline";
      actions.replaceChildren(opening, decline);
    } else if (page) {
      state.textContent = "Finish in the other tab, then choose Continue. If the server says you're done, this goes by itself.";
      accept.textContent = "Continue";
      decline.textContent = "Cancel";
      actions.replaceChildren(accept, decline);
    } else actions.replaceChildren(accept, decline);
  };
  draw();

  const card = el(
    "div",
    { class: "card", id: `q-${q.id}` },
    el("p", { class: "title" }, q.title),
    lines(q.who),
    lines(q.what),
    q.warning ? el("p", { class: "warning" }, q.warning) : null,
    el("p", { class: "question" }, q.question),
    page ? [el("div", { class: "page" }, ClakForm.address(page, el)), state] : null,
    filled && !filled.empty ? filled.node : null,
    problem,
    q.note ? el("p", { class: "note" }, q.note) : null,
    el("div", { class: "row" }, actions, left),
  );
  // A moment before anything works: a tap meant for something else doesn't answer.
  arm();
  return { card, left, expires: q.expires };
};

const questionCard = (q) => {
  if (q.form || q.page) return serverCard(q);
  // The scopes of a command's question (this call by default), "always" where the question
  // offers it, and the block.
  const choices = q.choices;
  const scopeList = choices?.scopes || [];
  let scope = scopeList[0]?.value || "once";
  const always = choices?.always ? el("input", { type: "checkbox" }) : null;
  const alwaysRow = always ? el("label", { class: "always" }, always, choices.always) : null;
  const option = (choice) => {
    const input = el("input", { type: "radio", name: `scope-${q.id}`, value: choice.value });
    input.checked = choice.value === scope;
    input.addEventListener("change", () => { scope = choice.value; });
    return el("label", { class: "scope" }, input, choice.label);
  };
  const scopes = scopeList.length ? el("div", { class: "scopes" }, scopeList.map(option)) : null;
  const allow = el("button", { class: "primary", disabled: true }, "Allow");
  const deny = el("button", { class: "deny", disabled: true }, "Deny");
  // Apart from Allow and Deny, as a link: a tap meant for Deny doesn't close the program.
  const block = choices?.block ? el("button", { class: "link-danger", disabled: true }, choices.block) : null;
  const left = el("p", { class: "meta" });
  const card = el(
    "div",
    { class: "card", id: `q-${q.id}` },
    el("p", { class: "title" }, q.title),
    lines(q.who),
    lines(q.what),
    q.tools && q.tools.length
      ? el("ul", { class: "tools" }, q.tools.map((t) => el("li", {}, t.note ? `${t.name} (${t.note})` : t.name, t.description ? ` — ${t.description}` : "", lines(t.changes))))
      : null,
    q.arguments && q.arguments.length ? [el("p", { class: "meta" }, "Arguments, sent by the agent:"), lines(q.arguments)] : null,
    q.warning ? el("p", { class: "warning" }, q.warning) : null,
    q.note ? el("p", { class: "note" }, q.note) : null,
    el("p", { class: "question" }, q.question),
    scopes,
    alwaysRow,
    el("div", { class: "row" }, el("div", { class: "actions" }, allow, deny), left),
    block ? el("div", { class: "block-row" }, block) : null,
  );
  const answer = async (yes, blocked = false) => {
    allow.disabled = deny.disabled = true;
    if (block) block.disabled = true;
    const response = await post("/api/answer", {
      id: q.id,
      allow: yes,
      scope,
      always: !!(yes && always && always.checked),
      block: blocked,
    });
    if (response.status === 401) return showLogin("The session is over: log in again.");
    if (!response.ok) notice((await response.json().catch(() => ({}))).error || "The answer didn't go through.");
    else notice(blocked ? `Blocked: ${q.title}` : yes ? `Allowed: ${q.title}` : `Denied: ${q.title}`);
    refresh();
  };
  allow.addEventListener("click", () => answer(true));
  deny.addEventListener("click", () => answer(false));
  block?.addEventListener("click", () => answer(false, true));
  // A moment before the buttons work: a tap meant for something else doesn't answer.
  setTimeout(() => {
    allow.disabled = deny.disabled = false;
    if (block) block.disabled = false;
  }, scopeList.length ? 500 : 1000);
  return { card, left, expires: q.expires };
};

const renderQuestions = (questions, now) => {
  const box = $("questions");
  const ids = new Set(questions.map((q) => q.id));
  for (const [id, item] of shown) {
    if (!ids.has(id)) {
      item.card.remove();
      shown.delete(id);
    }
  }
  for (const q of questions) {
    if (!shown.has(q.id)) {
      const item = questionCard(q);
      shown.set(q.id, item);
      box.append(item.card);
    }
    const item = shown.get(q.id);
    const seconds = Math.max(0, Math.round(item.expires - now));
    item.left.textContent = `${seconds} s to answer`;
  }
  box.querySelector(".empty")?.remove();
  if (!questions.length) box.append(el("p", { class: "empty" }, "No questions waiting."));

  if (wanted && !wantedSeen) {
    const item = shown.get(wanted);
    if (item) {
      wantedSeen = true;
      item.card.classList.add("focus");
      item.card.scrollIntoView({ block: "center" });
    } else {
      wantedSeen = true;
      notice("That question is no longer open: answered, withdrawn or expired.");
    }
  }
};

const list = (box, items, empty, render) => {
  box.replaceChildren(...(items.length ? items.map(render) : [el("p", { class: "empty" }, empty)]));
};

const takeAway = (kind, id, what) =>
  el("button", {
    class: "deny",
    onclick: async () => {
      const response = await post("/api/revoke", { kind, id });
      if (response.status === 401) return showLogin("The session is over: log in again.");
      notice(response.ok ? (kind === "block" ? `Unblocked: ${what}` : `Taken away: ${what}`) : "It couldn't be done.");
      refresh();
    },
  }, kind === "agent" ? "Disconnect" : kind === "block" ? "Unblock" : "Revoke");

const render = (state) => {
  const now = state.now;
  renderQuestions(state.questions || [], now);
  const programs = [...(state.commands || [])].sort((a, b) => (b.to_review > 0) - (a.to_review > 0));
  $("review-section").hidden = !programs.length;
  // A card is drawn again only when what it shows changed: the focus and a click on its
  // buttons survive the refresh every two seconds.
  const box = $("review");
  const keep = new Map([...box.children].map((n) => [n.dataset.sign, n]));
  box.replaceChildren(...programs.map((p) => {
    const sign = JSON.stringify([p, reviewing.get(p.key) || null]);
    const old = keep.get(sign);
    if (old) return old;
    const card = reviewCard(p);
    card.dataset.sign = sign;
    return card;
  }));
  list($("grants"), state.grants || [], "No agent is using a program.", (g) =>
    el("div", { class: "card row" },
      el("div", {},
        el("p", { class: "title" }, `${g.agent || "An agent"} → ${g.program || "a program"}`),
        el("p", { class: "meta" }, `since ${ago(g.since, now)} · ${g.calls || 0} calls · ${g.tools && g.tools.length ? `for the session: ${g.tools.join(", ")}` : "approved commands only"}`)),
      takeAway("grant", g.id, `${g.agent} → ${g.program}`)));
  // Blocks: refused at once until the agent exits, or until lifted here.
  $("blocks-section").hidden = !(state.blocks || []).length;
  list($("blocks"), state.blocks || [], "", (b) =>
    el("div", { class: "card row" },
      el("div", {},
        el("p", { class: "title" }, `${b.agent || "An agent"} ⊘ ${b.program || "a program"}`),
        el("p", { class: "meta" }, `blocked ${ago(b.since, now)} · its calls are refused until it exits`)),
      takeAway("block", b.id, `${b.agent} → ${b.program}`)));
  list($("agents"), state.agents || [], "No agent connected.", (a) =>
    el("div", { class: "card row" },
      el("div", {},
        el("p", { class: "title" }, a.label),
        el("p", { class: "meta" }, [a.exe, a.cwd ? `in ${a.cwd}` : null, `since ${ago(a.since, now)}`].filter(Boolean).join(" · "))),
      takeAway("agent", a.id, a.label)));
  list($("apps"), state.apps || [], "No application allowed.", (a) =>
    el("div", { class: "card row" },
      el("div", {},
        el("p", { class: "title" }, a.label || a.name),
        el("p", { class: "meta" }, [a.exe, a.signer ? `signed by ${a.signer}` : "unsigned", `since ${ago(a.since, now)}`].join(" · "))),
      takeAway("app", a.key, a.label || a.name)));
};

/// Each program's commands as Clak keeps them, those with new or changed commands first:
/// each opens to its commands, to approve, ask each time, turn off or on. Kept open across
/// refreshes. An approval names the version on screen.
const reviewing = new Map();

const commandsOf = async (key) => {
  const response = await post("/api/commands", { key });
  if (response.status === 401) return showLogin("The session is over: log in again.");
  if (!response.ok) notice("The commands couldn't be read.");
  return response.ok ? response.json() : null;
};

const decide = async (key, body) => {
  const response = await post("/api/commands", { key, ...body });
  if (response.status === 401) return showLogin("The session is over: log in again.");
  reviewing.set(key, response.ok ? await response.json() : null);
  refresh();
};

const STATUS = { approved: "approved", changed: "changed", new: "new", ask: "asks every time", off: "off" };

const commandRow = (key, c) => {
  const buttons = [];
  if (!c.missing_since) {
    if (c.status !== "approved" && c.state !== "off") buttons.push(el("button", { class: "primary", onclick: () => decide(key, { action: "approve", name: c.name, digest: c.digest }) }, c.status === "changed" ? "Approve the change" : "Approve"));
    if (c.state !== "ask" && c.status !== "new") buttons.push(el("button", { onclick: () => decide(key, { action: "ask", name: c.name }) }, c.state === "off" ? "Turn on" : "Ask each time"));
    if (c.state !== "off") buttons.push(el("button", { class: "deny", onclick: () => decide(key, { action: "off", name: c.name }) }, "Turn off"));
  }
  return el("div", { class: "review" },
    el("p", {}, `${c.name} (${c.missing_since ? "no longer offered" : STATUS[c.status] || c.status})`, c.description ? ` — ${c.description}` : ""),
    lines(c.changes),
    buttons.length ? el("div", { class: "actions" }, buttons) : null);
};

const reviewCard = (program) => {
  const open = reviewing.get(program.key);
  const list = open?.commands || [];
  const offered = list.filter((c) => !c.missing_since);
  const missing = list.length - offered.length;
  return el("div", { class: "card" },
    el("p", { class: "title" }, `${program.label}: ${program.approved} approved` + (program.to_review ? ` · ${program.to_review} new or changed` : "")),
    open
      ? [
        list.map((c) => commandRow(program.key, c)),
        el("div", { class: "actions" },
          program.to_review ? el("button", { class: "primary", onclick: () => decide(program.key, { action: "approve_all", commands: offered.map((c) => ({ name: c.name, digest: c.digest })) }) }, "Approve all") : null,
          program.approved ? el("button", { onclick: () => decide(program.key, { action: "reset" }) }, "Reset approvals") : null,
          missing ? el("button", { onclick: () => decide(program.key, { action: "forget_missing" }) }, "Forget the ones no longer offered") : null,
          el("button", { onclick: () => { reviewing.delete(program.key); refresh(); } }, "Close")),
      ]
      : el("div", { class: "actions" },
        el("button", { onclick: async () => { reviewing.set(program.key, await commandsOf(program.key)); refresh(); } }, "Commands")));
};

let timer = null;

const refresh = async () => {
  clearTimeout(timer);
  try {
    const response = await fetch("/api/state", { credentials: "same-origin" });
    if (response.status === 401) return showLogin();
    if (!response.ok) throw new Error(String(response.status));
    const state = await response.json();
    $("login").hidden = true;
    $("app").hidden = false;
    $("logout").hidden = false;
    render(state);
  } catch {
    notice("The hub doesn't answer: is clak-daemon still running?");
  }
  timer = setTimeout(refresh, 2000);
};

$("login-form").addEventListener("submit", async (event) => {
  event.preventDefault();
  const response = await post("/api/login", { password: $("password").value });
  $("password").value = "";
  if (!response.ok) return showLogin((await response.json().catch(() => ({}))).error || "It didn't work.");
  refresh();
});

$("logout").addEventListener("click", async () => {
  await post("/api/logout");
  clearTimeout(timer);
  for (const item of shown.values()) item.card.remove();
  shown.clear();
  showLogin();
});

refresh();
