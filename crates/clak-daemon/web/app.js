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

const questionCard = (q) => {
  const always = q.always ? el("input", { type: "checkbox" }) : null;
  const allow = el("button", { class: "primary", disabled: true }, "Allow");
  const deny = el("button", { class: "deny", disabled: true }, "Deny");
  const left = el("p", { class: "meta" });
  const card = el(
    "div",
    { class: "card", id: `q-${q.id}` },
    el("p", { class: "title" }, q.title),
    lines(q.who),
    lines(q.what),
    q.tools && q.tools.length
      ? el("ul", { class: "tools" }, q.tools.map((t) => el("li", {}, t.note ? `${t.name} (${t.note})` : t.name, t.description ? ` — ${t.description}` : "")))
      : null,
    q.warning ? el("p", { class: "warning" }, q.warning) : null,
    q.note ? el("p", { class: "note" }, q.note) : null,
    el("p", { class: "question" }, q.question),
    always ? el("label", { class: "always" }, always, q.always) : null,
    el("div", { class: "row" }, el("div", { class: "actions" }, allow, deny), left),
  );
  const answer = async (yes) => {
    allow.disabled = deny.disabled = true;
    const response = await post("/api/answer", { id: q.id, allow: yes, always: !!(yes && always && always.checked) });
    if (response.status === 401) return showLogin("The session is over: log in again.");
    if (!response.ok) notice((await response.json().catch(() => ({}))).error || "The answer didn't go through.");
    else notice(yes ? `Allowed: ${q.title}` : `Denied: ${q.title}`);
    refresh();
  };
  allow.addEventListener("click", () => answer(true));
  deny.addEventListener("click", () => answer(false));
  // A moment before the buttons work: a tap meant for something else doesn't answer.
  setTimeout(() => (allow.disabled = deny.disabled = false), 1000);
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
      notice(response.ok ? `Taken away: ${what}` : "It couldn't be taken away.");
      refresh();
    },
  }, kind === "agent" ? "Disconnect" : "Revoke");

const render = (state) => {
  const now = state.now;
  renderQuestions(state.questions || [], now);
  list($("grants"), state.grants || [], "No agent is using a program.", (g) =>
    el("div", { class: "card row" },
      el("div", {},
        el("p", { class: "title" }, `${g.agent || "An agent"} → ${g.program || "a program"}`),
        el("p", { class: "meta" }, `since ${ago(g.since, now)} · ${g.calls || 0} calls${g.tools && g.tools.length ? ` · ${g.tools.join(", ")}` : ""}`)),
      takeAway("grant", g.id, `${g.agent} → ${g.program}`)));
  list($("agents"), state.agents || [], "No agent connected.", (a) =>
    el("div", { class: "card row" },
      el("div", {},
        el("p", { class: "title" }, a.label),
        el("p", { class: "meta" }, [a.exe, a.cwd ? `in ${a.cwd}` : null, `since ${ago(a.since, now)}`].filter(Boolean).join(" · "))),
      takeAway("agent", a.id, a.label)));
  list($("rules"), state.rules || [], "No standing rule.", (r) =>
    el("div", { class: "card row" },
      el("div", {},
        el("p", { class: "title" }, `${r.agent} → ${r.program}`),
        el("p", { class: "meta" }, [r.folder ? `in ${r.folder}` : "in any folder", `since ${ago(r.since, now)}`].join(" · "))),
      takeAway("rule", r.id, `${r.agent} → ${r.program}`)));
  list($("apps"), state.apps || [], "No application allowed.", (a) =>
    el("div", { class: "card row" },
      el("div", {},
        el("p", { class: "title" }, a.label || a.name),
        el("p", { class: "meta" }, [a.exe, a.signer ? `signed by ${a.signer}` : "unsigned", `since ${ago(a.since, now)}`].join(" · "))),
      takeAway("app", a.key, a.label || a.name)));
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
