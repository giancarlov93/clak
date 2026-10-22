// The sentences with which the panel describes the state, starting from the replies to `describe` and
// `history`. Here, and not in the tabs, because they all use them.
//
// They receive the objects as they arrive from the hub (snake_case: `last_call`, `has_token`…).
// Times are seconds since the epoch; `now` is in milliseconds (Date.now()), as in JavaScript.
"use strict";

const Describe = (() => {
  const plural = (n, one, many) => `${n} ${n === 1 ? one : many}`;
  const seconds = (now) => (now ?? Date.now()) / 1000;
  const file = (path) => String(path || "").split(/[\\/]/).pop();
  const pad = (n) => String(n).padStart(2, "0");
  const MONTHS = ["Jan", "Feb", "Mar", "Apr", "May", "Jun", "Jul", "Aug", "Sep", "Oct", "Nov", "Dec"];

  const label = (s) => s?.label || s?.name || "agent";
  const sessionOf = (state, grant) => (state?.sessions || []).find((s) => s.id === grant.session_id);
  /// The agent name of a permission, or "an agent" if it has already gone.
  const agentOf = (state, grant) => {
    const session = sessionOf(state, grant);
    return session ? label(session) : "an agent";
  };
  const resourceOf = (state, id) => (state?.resources || []).find((r) => r.id === id);

  /// A program with a window of its own, which can be brought to the front: one connected
  /// from a process. MCP servers, APIs and Clak's settings have none.
  const showable = (r) => !!r && !r.origin?.kind && r.origin?.pid > 0;

  /// The MCP server or REST API behind a resource, for its icon: `mcp:<id>` or `api:<id>`.
  const iconSource = (state, resourceId) => {
    const c = (state?.connectors || []).find((x) => x.resource_id && x.resource_id === resourceId);
    if (c) return `mcp:${c.id}`;
    const a = (state?.apis || []).find((x) => x.resource_id && x.resource_id === resourceId);
    return a ? `api:${a.id}` : null;
  };
  const info = (r) => r?.resource || { app: "?", title: "?" };

  // --- time -------------------------------------------------------------------

  /// How many calendar days before `now` the moment fell: 0 today, 1 yesterday.
  const daysBefore = (epoch, now) => {
    const at = new Date(epoch * 1000);
    const today = new Date(now ?? Date.now());
    return Math.round(
      (new Date(today.getFullYear(), today.getMonth(), today.getDate()) - new Date(at.getFullYear(), at.getMonth(), at.getDate())) / 86400000);
  };

  /// "just now", "5 min ago", "3 hours ago", "yesterday", "4 days ago"; past a week the date:
  /// "Oct 12", with the year if it isn't this one.
  const ago = (epoch, now) => {
    const s = seconds(now) - epoch;
    if (s < 45) return "just now";
    if (s < 90) return "a minute ago";
    if (s < 3600) return `${Math.round(s / 60)} min ago`;
    if (s < 5400) return "an hour ago";
    if (s < 86400) return `${Math.round(s / 3600)} hours ago`;
    const day = daysBefore(epoch, now);
    if (day <= 1) return "yesterday";
    if (day < 7) return `${day} days ago`;
    const at = new Date(epoch * 1000);
    const year = at.getFullYear() === new Date(now ?? Date.now()).getFullYear() ? "" : `, ${at.getFullYear()}`;
    return `${MONTHS[at.getMonth()]} ${at.getDate()}${year}`;
  };

  const clock = (epoch) => {
    const at = new Date(epoch * 1000);
    return `${pad(at.getHours())}:${pad(at.getMinutes())}:${pad(at.getSeconds())}`;
  };

  /// A history row: "14:32:05", "yesterday 09:10:00", "Oct 12 18:00:00".
  const when = (epoch, now) => {
    const at = new Date(epoch * 1000);
    const day = daysBefore(epoch, now);
    if (day === 0) return clock(epoch);
    if (day === 1) return `yesterday ${clock(epoch)}`;
    return `${MONTHS[at.getMonth()]} ${at.getDate()} ${clock(epoch)}`;
  };

  const duration = (s) => {
    if (s < 1) return `${Math.round(s * 1000)} ms`;
    if (s < 60) return `${Math.round(s * 10) / 10} s`;
    return `${Math.floor(s / 60)} min ${Math.floor(s % 60)} s`;
  };

  // --- agents and programs ------------------------------------------------------

  const signature = (signed, signer) => {
    if (signed === "valid") return signer ? `signed by ${signer}` : "signed";
    if (signed === "invalid") return "INVALID SIGNATURE";
    if (signed === "unsigned") return "unsigned";
    return "signature can't be verified";
  };

  const sessionTitle = (s) => label(s);

  const sessionDetail = (s, now) => {
    const parts = [];
    if (s.exe) parts.push(`${file(s.exe)} (pid ${s.pid})`);
    parts.push(signature(s.signed, s.signer));
    parts.push(s.via === "mcp" ? "via MCP" : "via command line");
    parts.push(`connected ${ago(s.since, now)}`);
    return parts.join(" · ");
  };

  const resourceTitle = (r) => `${info(r).app} — ${info(r).title}`;

  /// For the tray: "Revit", not "Revit — Revit", when program and document have the same name.
  const resourceShort = (r) =>
    String(info(r).app).toLowerCase() === String(info(r).title).toLowerCase() ? info(r).app : resourceTitle(r);

  const resourceDetail = (r, now) =>
    `clak ${r.name} · ${r.status ? `${r.status} · ` : ""}${plural((r.tools || []).length, "command", "commands")} · connected ${ago(r.since, now)}`;

  /// Who connected, according to the operating system and not to the program.
  const origin = (o, publisher) => {
    let text;
    if (o?.kind === "hub") text = "Clak itself";
    else if (o?.kind === "connector" && o.transport === "stdio") text = `MCP server, local program: ${o.command}`;
    else if (o?.kind === "connector")
      text = `${o.protocol === "rest" ? "REST API" : "MCP server"}, ${o.url}${o.secure === false && o.loopback === false ? ", unencrypted" : ""}`;
    else if (!o?.exe) text = "unidentified process";
    else {
      const runtime = Clak.runtime(o.exe);
      const exe = runtime ? `${file(o.exe)} (${runtime})` : file(o.exe);
      if (o.signed === "valid" && o.signer) text = `${exe}, signed by ${o.signer}`;
      else if (o.signed === "invalid") text = `${exe}, INVALID SIGNATURE (${o.exe})`;
      else text = `${exe}, unsigned (${o.exe})`;
    }
    return publisher ? `${text}; claims to be “${publisher}”` : text;
  };

  /// For the tray: "166 · just now", or "idle" if it hasn't called anything yet.
  const grantBrief = (g, now) => (g.calls === 0 ? "idle" : `${g.calls} · ${ago(g.last_call ?? g.since, now)}`);

  /// A command in the last few moments: in the tray, the green dot.
  const recent = (g, now) => g.last_call != null && seconds(now) - g.last_call < 45;

  const grantDetail = (g, now) =>
    [
      g.calls === 0
        ? "no commands yet"
        : plural(g.calls, "command run", "commands run") +
          (g.errors > 0 ? `, ${plural(g.errors, "error", "errors")}` : "") +
          `, last ${ago(g.last_call ?? g.since, now)}`,
      `allowed ${ago(g.since, now)}`,
    ].join(" · ");

  const allowedAppDetail = (app, now) =>
    `${app.exe} · ${app.signer ? `${Clak.runtime(app.exe) ? `${Clak.runtime(app.exe)} ` : ""}signed by ${app.signer}` : "unsigned: valid for this version of the file"} · allowed ${ago(app.since, now)}`;

  /// Who is using a program: "Claude Code · repo, Codex", or "No agent is using it".
  const users = (state, resourceId, none = "No agent is using it") => {
    const names = (state?.grants || []).filter((g) => g.resource_id === resourceId).map((g) => agentOf(state, g));
    return names.length === 0 ? none : names.join(", ");
  };

  const headline = (state) =>
    [
      plural((state.sessions || []).length, "agent", "agents"),
      plural((state.resources || []).length, "program", "programs"),
      plural((state.grants || []).length, "permission", "permissions"),
    ].join(" · ");

  // --- a permission's history -------------------------------------------------

  /// A `history` reply: the calls (most recent first) and the counts per command.
  const history = (m) => {
    const usage = (m.stats || []).map((s) => ({
      tool: s.tool ?? "?", calls: s.calls || 0, errors: s.errors || 0, seconds: s.seconds || 0,
      get errorRate() { return this.calls === 0 ? 0 : this.errors / this.calls; },
      get averageSeconds() { return this.calls === 0 ? 0 : this.seconds / this.calls; },
    }));
    const calls = (m.calls || []).map((c) => ({
      id: c.id || "", tool: c.tool ?? "?", at: c.at || 0, seconds: c.seconds || 0, outcome: c.outcome ?? "?",
      // Whether the hub still has what was sent and received (only the latest calls).
      detail: !!c.detail,
      // The program's fault: an error, no reply in time, closed halfway.
      failed: c.outcome === "error" || c.outcome === "timeout" || c.outcome === "closed",
    }));
    const byName = (a, b) => (a.tool < b.tool ? -1 : a.tool > b.tool ? 1 : 0);
    return {
      grantId: m.grant_id || "",
      found: !!m.found,
      limit: m.limit || 0,
      calls,
      usage,
      /// The most used commands; on a tie, those with fewer errors.
      mostUsed: (count = 5) =>
        usage.filter((u) => u.calls > 0).sort((a, b) => b.calls - a.calls || a.errors - b.errors || byName(a, b)).slice(0, count),
      /// The commands that fail most in proportion; on a tie, those with more errors. Only those with errors.
      mostFailing: (count = 5) =>
        usage.filter((u) => u.errors > 0).sort((a, b) => b.errorRate - a.errorRate || b.errors - a.errors || byName(a, b)).slice(0, count),
    };
  };

  /// "12 times · 1 error · 0.4 s on average".
  const usageLine = (u) =>
    [u.calls === 1 ? "once" : `${u.calls} times`, u.errors === 0 ? null : plural(u.errors, "error", "errors"), `${duration(u.averageSeconds)} on average`]
      .filter(Boolean).join(" · ");

  /// "3 errors out of 4 (75%)".
  const failureLine = (u) => `${plural(u.errors, "error", "errors")} out of ${u.calls} (${Math.round(u.errorRate * 100)}%)`;

  /// The outcome of a call, in words.
  const outcome = (o) =>
    ({ ok: "done", error: "error", timeout: "no answer in time", closed: "closed during the command", interrupted: "the agent didn't wait" })[o] ?? o;

  // --- a program and its commands ---------------------------------------------

  const text = (v) => (typeof v === "string" ? v : null);

  /// A command of the `tools` reply, read from its schema: name, description, parameters.
  const tool = (t) => {
    const schema = t.input_schema && typeof t.input_schema === "object" ? t.input_schema : null;
    const required = new Set((schema?.required || []).filter((r) => typeof r === "string"));
    const parameters = Object.entries(schema?.properties || {})
      .map(([name, p]) => {
        p = p && typeof p === "object" ? p : {};
        const type = Array.isArray(p.type) ? p.type.filter((x) => typeof x === "string").join(" | ") : text(p.type) ?? (Array.isArray(p.enum) ? "choice" : null);
        return {
          name,
          type,
          required: required.has(name),
          description: text(p.description)?.trim() || "",
          choices: (Array.isArray(p.enum) ? p.enum : []).map((c) => (typeof c === "string" ? c : JSON.stringify(c))),
          default: p.default === undefined ? "" : typeof p.default === "string" ? p.default : JSON.stringify(p.default),
        };
      })
      .sort((a, b) => Number(b.required) - Number(a.required));
    const description = text(t.description)?.trim() || "";
    const first = description.split("\n").map((l) => l.trim()).find((l) => l) || "";
    return {
      name: text(t.name) ?? "?",
      description,
      /// The first line of the description: for the list.
      summary: first.length <= 200 ? first : first.slice(0, 199) + "…",
      parameters,
      schema: schema ? JSON.stringify(schema, null, 2) : "",
    };
  };

  /// A `tools` reply.
  const tools = (m) => ({ resourceId: m.resource_id || "", found: !!m.found, tools: (m.tools || []).map(tool) });

  const placeholder = (type) =>
    ({ string: "TEXT", integer: "N", number: "NUMBER", boolean: "", object: "JSON", array: "JSON" })[type] ?? "VALUE";

  /// How the command line runs it: `clak jarvis issues_list --project TEXT [--limit N]`.
  const usage = (program, t) =>
    [`clak ${program} ${t.name}`, ...t.parameters.map((p) => {
      const flag = `--${p.name} ${placeholder(p.type)}`.trimEnd();
      return p.required ? flag : `[${flag}]`;
    })].join(" ");

  /// The type of a parameter, in words.
  const parameterType = (p) =>
    p.type == null ? "any" : ({ string: "text", integer: "integer", number: "number", boolean: "yes/no", object: "JSON object", array: "JSON list" })[p.type] ?? p.type;

  // --- connectors and APIs ------------------------------------------------------

  const isOAuth = (c) => c.auth === "oauth";

  /// ok | warn | err | off: the color of a connector's dot.
  const connectorKind = (c) => {
    if (!c.enabled) return "off";
    if (c.state === "active") return "ok";
    if (c.state === "token rejected" || c.state === "sign-in required") return "err";
    return "warn";
  };

  const connectorState = (c, now) => {
    if (!c.enabled) return "off: agents don't see it";
    const state = c.state ?? "?";
    const parts = [state === "active" && c.active_since != null ? `active ${ago(c.active_since, now)}` : state];
    if (state === "active" || c.tools > 0) parts.push(plural(c.tools || 0, "command", "commands"));
    if (c.transport === "stdio") {
      parts.push(c.env?.length ? plural(c.env.length, "environment variable", "environment variables") : "local program");
      return parts.join(" · ");
    }
    parts.push(isOAuth(c)
      ? c.has_token ? `signed in${c.signin_host ? ` at ${c.signin_host}` : ""}` : "not signed in"
      : c.has_token ? "with token" : "no token");
    return parts.join(" · ");
  };

  /// How a connected program has been answering the agents, in one line: "15 commands · 2
  /// failed · 0.4 s on average · last 3 min ago", and whether the last one failed. Null if it
  /// hasn't run a command yet, or isn't connected.
  const health = (state, resourceId, now) => {
    const h = (state?.resources || []).find((r) => r.id === resourceId)?.health;
    if (!h) return null;
    const parts = [plural(h.calls, "command", "commands")];
    if (h.errors) parts.push(`${h.errors} failed`);
    parts.push(`${duration(h.average)} on average`);
    const failed = h.last_outcome === "error" || h.last_outcome === "timeout" || h.last_outcome === "closed";
    parts.push(`last ${ago(h.last_at, now)}${failed ? `, ${outcome(h.last_outcome)}` : ""}`);
    return { text: parts.join(" · "), failed };
  };

  /// An API in one line: how many endpoints, how it signs in, and whether the secret is there.
  const apiState = (a) => {
    if (!a.enabled) return "off: agents don't see it";
    const parts = [plural(a.endpoints || 0, "endpoint", "endpoints")];
    if (a.auth === "none") parts.push("no authentication");
    else if (a.auth === "oauth" || a.auth === "oauth-app")
      parts.push(a.state === "sign-in required" || !a.has_secret ? "not signed in" : `signed in at ${a.signin_host}`);
    else parts.push(`${a.auth_label}${a.has_secret ? "" : ", not set"}`);
    if (a.source) parts.push("from OpenAPI");
    return parts.join(" · ");
  };

  /// An `http` address to another computer: token and data travel in the clear.
  const unencrypted = (url) => {
    try {
      const u = new URL(url);
      const host = u.hostname.replace(/^\[|\]$/g, "");
      const loopback = host === "localhost" || host === "::1" || /^127\./.test(host);
      return u.protocol === "http:" && !loopback;
    } catch {
      return false;
    }
  };

  // --- the log ------------------------------------------------------------------

  const BAD = ["denied", "refused", "error", "unreachable", "rejected", "failed"];
  const ENDED = ["revoked", "disconnected", "closed", "forgot"];

  /// err | warn | "": the noes and the errors stand out at once.
  const logKind = (line) => {
    const lower = String(line).toLowerCase();
    return BAD.some((w) => lower.includes(w)) ? "err" : ENDED.some((w) => lower.includes(w)) ? "warn" : "";
  };

  return {
    plural, ago, clock, when, duration, signature, label, sessionOf, agentOf, resourceOf, showable, iconSource,
    sessionTitle, sessionDetail, resourceTitle, resourceShort, resourceDetail, origin,
    grantBrief, recent, grantDetail, allowedAppDetail, users, headline,
    history, usageLine, failureLine, outcome,
    tool, tools, usage, parameterType,
    connectorKind, connectorState, apiState, health, unencrypted, logKind,
  };
})();
