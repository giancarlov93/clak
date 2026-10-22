// The Usage tab: what the agents' tokens would cost at the API's prices, read from the
// agents' own files (costs.rs). The totals of today, the week, the month and all time;
// Claude's five-hour window while it is open; the days of the span chosen, by harness or
// by kind of token; that span's costs by kind of token, grouped by harness, model or
// folder. Read again on every opening, when the
// window comes back to the foreground and every minute while the tab is shown: only the
// files that changed are read again.
"use strict";

(() => {
  const el = Clak.el;
  const MONTHS = ["Jan", "Feb", "Mar", "Apr", "May", "Jun", "Jul", "Aug", "Sep", "Oct", "Nov", "Dec"];
  const SPANS = [["today", "Today"], ["7", "7 days"], ["30", "30 days"], ["month", "This month"], ["all", "All"]];
  const SVG = "http://www.w3.org/2000/svg";
  let page = null;
  let body = null;
  let report = null;
  let failed = null;
  let busy = false;
  /// The chart's width, and the page's when it was measured: redrawn when the page changes.
  let width = 0;
  let measured = 0;
  /// What the user chose, kept in this window: the span, the chart's colors, the table's rows.
  const choice = { span: "30", colors: "agent", rows: "agent" };
  try {
    for (const k of Object.keys(choice)) choice[k] = localStorage.getItem(`costs.${k}`) || choice[k];
  } catch { /* no storage: the defaults */ }
  const choose = (k, v) => {
    choice[k] = v;
    try { localStorage.setItem(`costs.${k}`, v); } catch { /* not kept */ }
    draw();
    if (k !== "rows") grow();
  };
  /// The bars rise from the axis: when the chart shows something else, not when it refreshes.
  const grow = () => {
    const svg = body?.querySelector(".cost-chart svg");
    if (svg) Clak.animate(svg, [{ clipPath: "inset(100% 0 0 0)" }, { clipPath: "inset(0 0 0 0)" }], { duration: 450 });
  };
  /// The kinds of token, each at its own price: their colors after the harnesses'.
  const KINDS = [["input", "Input", 5], ["output", "Output", 6], ["cache_write", "Cache write", 7], ["cache_read", "Cache read", 8]];
  const KIND_HELP = {
    input: "What the model read without the cache. Claude Code puts almost all of it in the cache: there, most of what is read is cache read",
    output: "What the model wrote, its reasoning included",
    cache_write: "What was put in the cache, dearer than input (for an hour dearer still)",
    cache_read: "What was read again from the cache: a tenth of the price of input, or so",
  };

  // --- numbers and days ----------------------------------------------------------------

  const pad = (n) => String(n).padStart(2, "0");
  const dayOf = (d) => `${d.getFullYear()}-${pad(d.getMonth() + 1)}-${pad(d.getDate())}`;
  const today = () => dayOf(new Date());
  const daysAgo = (n) => {
    const d = new Date();
    d.setDate(d.getDate() - n);
    return dayOf(d);
  };
  const dateOf = (day) => {
    const [y, m, d] = day.split("-").map(Number);
    return new Date(y, m - 1, d || 1);
  };
  const dayLabel = (day) => `${MONTHS[Number(day.slice(5, 7)) - 1]} ${Number(day.slice(8, 10))}`;
  const monthLabel = (month) => `${MONTHS[Number(month.slice(5, 7)) - 1]} ${month.slice(0, 4)}`;
  const hm = (ms) => {
    const d = new Date(ms);
    return `${pad(d.getHours())}:${pad(d.getMinutes())}`;
  };

  /// "$12.34", "<$0.01", "$1,234.56".
  const money = (v) => v === 0 ? "$0" : v < 0.01 ? "<$0.01"
    : "$" + v.toLocaleString("en-US", { minimumFractionDigits: 2, maximumFractionDigits: 2 });
  /// On the chart's scale: "$5", "$12.5", "$1.2K".
  const axisMoney = (v) => v >= 1000 ? `$${+(v / 1000).toFixed(1)}K` : `$${+v.toFixed(v < 10 ? 2 : 0)}`;
  /// "999", "12K", "1.2M", "3.4B".
  const count = (n) => n >= 1e9 ? `${+(n / 1e9).toFixed(1)}B` : n >= 1e6 ? `${+(n / 1e6).toFixed(1)}M`
    : n >= 1e3 ? `${+(n / 1e3).toFixed(n >= 1e4 ? 0 : 1)}K` : String(n);

  const tokensOf = (r) => r.input + r.output + r.cache_write + r.cache_read;
  /// Rows added up: the tokens and the cost of each kind (`cost_input`…), the total cost
  /// with what agents worked out themselves (`own`: not split by kind).
  const total = (rows) => rows.reduce((t, r) => {
    t.cost += r.cost;
    t.tokens += tokensOf(r);
    for (const [k] of KINDS) {
      t[k] += r[k];
      t[`cost_${k}`] += r[`cost_${k}`] || 0;
    }
    t.own += r.cost - KINDS.reduce((sum, [k]) => sum + (r[`cost_${k}`] || 0), 0);
    t.requests += r.requests;
    t.unpriced += r.unpriced;
    return t;
  }, { cost: 0, tokens: 0, own: 0, requests: 0, unpriced: 0, ...Object.fromEntries(KINDS.flatMap(([k]) => [[k, 0], [`cost_${k}`, 0]])) });

  /// The first day of the span: "" for all of it.
  const from = (which) => which === "today" ? today() : which === "7" ? daysAgo(6) : which === "30" ? daysAgo(29)
    : which === "month" ? today().slice(0, 8) + "01" : "";
  const since = (rows, day) => rows.filter((r) => r.day >= day);

  const agentName = (id) => report.agents.find((a) => a.id === id)?.name || id;
  const series = (id) => Math.max(0, report.agents.findIndex((a) => a.id === id)) + 1;
  const swatch = (id) => el("span", { class: "swatch", style: `background:var(--series-${series(id)})` });

  /// The chart's series: the harnesses, or the kinds of token (and what agents worked out
  /// themselves, which has no kind). Each with its color and what a row gives it.
  const chartSeries = () => choice.colors === "kind"
    ? [...KINDS.map(([k, name, slot]) => ({ id: k, name, color: `var(--series-${slot})`, of: (r) => r[`cost_${k}`] || 0 })),
      { id: "own", name: "Agents' own cost", color: "var(--faint)", of: (r) => Math.max(0, r.cost - KINDS.reduce((s, [k]) => s + (r[`cost_${k}`] || 0), 0)) }]
    : report.agents.map((a) => ({ id: a.id, name: a.name, color: `var(--series-${series(a.id)})`, of: (r) => (r.agent === a.id ? r.cost : 0) }));
  const dot = (color) => el("span", { class: "swatch", style: `background:${color}` });

  // --- the pieces ----------------------------------------------------------------------

  const tiles = (rows) => {
    const tile = (label, day) => {
      const t = total(since(rows, day));
      return el("div", { class: "card stat cost-stat" },
        el("div", null, el("div", { class: "value" }, money(t.cost)),
          el("div", { class: "quiet" }, `${label} · ${count(t.tokens)} tokens`)));
    };
    return el("div", { class: "stats cost-stats" },
      tile("Today", today()), tile("7 days", daysAgo(6)), tile("This month", from("month")), tile("All time", ""));
  };

  /// Claude's usage limits count in windows of five hours: this one, while it is open.
  const windowCard = (w) => {
    const now = Date.now();
    const hours = Math.max(now - w.first, 60_000) / 3_600_000;
    const rate = w.cost / hours;
    const left = Math.max(w.end - now, 0);
    const by = w.cost + rate * (left / 3_600_000);
    const done = Math.min(100, Math.max(0, ((now - w.start) / (w.end - w.start)) * 100));
    const minutes = Math.round(left / 60_000);
    return el("div", { class: "card cost-window", title: "Claude's usage limits count in windows of five hours, from the first request after a pause" },
      el("div", { class: "head" },
        el("strong", null, "Claude Code · 5-hour window"),
        el("span", { class: "quiet" }, `${hm(w.start)}–${hm(w.end)} · ${minutes >= 60 ? `${Math.floor(minutes / 60)} h ${minutes % 60} min` : `${minutes} min`} left`)),
      el("div", { class: "line" },
        el("span", { class: "value" }, money(w.cost)),
        el("span", { class: "quiet" }, `${count(w.tokens)} tokens · ${Clak.plural(w.requests, "request", "requests")} · ${money(rate)} an hour · about ${money(by)} by ${hm(w.end)}`)),
      el("div", { class: "meter" }, el("span", { style: `width:${done.toFixed(1)}%` })));
  };

  /// The costs of each day of the span (of each month, past four months; of each hour, today),
  /// a harness or a kind of token per color: the first one at the bottom.
  const chart = (rows, order) => {
    const hourly = choice.span === "today";
    const first = from(choice.span) || rows.reduce((min, r) => (r.day < min ? r.day : min), today());
    const days = [];
    for (const d = dateOf(first); dayOf(d) <= today(); d.setDate(d.getDate() + 1)) days.push(dayOf(d));
    const monthly = days.length > 120;
    const keys = hourly ? [...Array(24).keys()] : monthly ? [...new Set(days.map((d) => d.slice(0, 7)))] : days;
    const buckets = new Map(keys.map((k) => [k, { key: k, cost: 0, parts: new Map() }]));
    for (const r of hourly ? report.hours || [] : rows) {
      const b = buckets.get(hourly ? r.hour : monthly ? r.day.slice(0, 7) : r.day);
      if (!b) continue;
      b.cost += r.cost;
      for (const s of order) b.parts.set(s.id, (b.parts.get(s.id) || 0) + s.of(r));
    }
    const all = [...buckets.values()];

    const W = Math.max(width, 320);
    const H = 200;
    const left = 52;
    const bottom = 22;
    const top = 8;
    const plotW = W - left - 4;
    const plotH = H - top - bottom;
    const highest = Math.max(...all.map((b) => b.cost), 0);
    const step = nice(highest / 3 || 1);
    const max = step * Math.max(1, Math.ceil(highest / step));
    const y = (v) => top + plotH - (v / max) * plotH;
    const slot = plotW / all.length;
    const bar = Math.max(2, Math.min(28, slot * 0.68));

    const svg = document.createElementNS(SVG, "svg");
    const add = (tag, attrs, parent = svg) => {
      const node = document.createElementNS(SVG, tag);
      for (const [k, v] of Object.entries(attrs)) node.setAttribute(k, String(v));
      parent.append(node);
      return node;
    };
    svg.setAttribute("viewBox", `0 0 ${W} ${H}`);
    svg.setAttribute("width", W);
    svg.setAttribute("height", H);
    svg.setAttribute("role", "img");
    svg.setAttribute("aria-label", `Costs per ${hourly ? "hour" : monthly ? "month" : "day"}`);
    const name = (key) => hourly ? `${pad(key)}:00` : monthly ? monthLabel(key) : dayLabel(key);
    for (let v = 0; v <= max + step / 2; v += step) {
      add("line", { x1: left, x2: W - 4, y1: y(v), y2: y(v), class: "grid" });
      add("text", { x: left - 8, y: y(v) + 4, class: "axis", "text-anchor": "end" }).textContent = axisMoney(v);
    }
    // A date every 70 pixels or so, the last one always: none too close to it.
    const every = Math.max(1, Math.ceil(70 / slot));
    const last = all.length - 1;
    const labelled = (i) => i === last || (i % every === 0 && last - i >= every);
    all.forEach((b, i) => {
      const x = left + slot * i + (slot - bar) / 2;
      const stack = order.filter((s) => (b.parts.get(s.id) || 0) > 0);
      let base = y(0);
      stack.forEach((s, n) => {
        const h = (b.parts.get(s.id) / max) * plotH;
        const gap = n > 0 && h > 3 ? 2 : 0;
        const segment = { x, y: base - h, w: bar, h: h - gap };
        if (segment.h > 0) {
          add("path", { d: n === stack.length - 1 ? rounded(segment, Math.min(4, bar / 2)) : square(segment), fill: s.color });
        }
        base -= h;
      });
      if (labelled(i)) {
        const anchor = i === 0 && last > 0 ? "start" : i === last ? "end" : "middle";
        const at = anchor === "start" ? left + slot * i : anchor === "end" ? Math.min(left + slot * (i + 1), W - 4) : left + slot * i + slot / 2;
        add("text", { x: at, y: H - 6, class: "axis", "text-anchor": anchor }).textContent = name(b.key);
      }
      // Wider than the bar: the hover finds it.
      const hit = add("rect", { x: left + slot * i, y: top, width: slot, height: plotH, class: "hit" });
      hit.dataset.index = i;
    });

    const tip = el("div", { class: "cost-tip hidden", role: "tooltip" });
    const box = el("div", { class: "cost-chart" }, svg, tip);
    svg.addEventListener("mousemove", (e) => {
      const i = e.target.dataset?.index;
      if (i === undefined) return tip.classList.add("hidden");
      const b = all[i];
      svg.querySelectorAll(".hit.over").forEach((n) => n.classList.remove("over"));
      e.target.classList.add("over");
      tip.replaceChildren(
        el("strong", null, hourly ? `${pad(b.key)}:00–${pad((b.key + 1) % 24)}:00` : monthly ? monthLabel(b.key) : dayLabel(b.key) + ", " + b.key.slice(0, 4)),
        el("div", { class: "tip-row" }, el("span", null, "Total"), el("span", { class: "strong" }, money(b.cost))),
        ...order.filter((s) => b.parts.get(s.id) > 0).reverse().map((s) =>
          el("div", { class: "tip-row" }, el("span", null, dot(s.color), s.name), el("span", null, money(b.parts.get(s.id))))));
      tip.classList.remove("hidden");
      const at = left + slot * Number(i) + slot / 2;
      tip.style.left = `${Math.min(Math.max(at - tip.offsetWidth / 2, 0), W - tip.offsetWidth)}px`;
    });
    svg.addEventListener("mouseleave", () => {
      tip.classList.add("hidden");
      svg.querySelectorAll(".hit.over").forEach((n) => n.classList.remove("over"));
    });
    return box;
  };

  /// 1, 2, 2.5 or 5 times a power of ten: the steps of the scale.
  const nice = (v) => {
    const power = 10 ** Math.floor(Math.log10(v));
    return [1, 2, 2.5, 5, 10].map((m) => m * power).find((s) => s >= v);
  };
  const square = (s) => `M${s.x},${s.y + s.h}V${s.y}H${s.x + s.w}V${s.y + s.h}Z`;
  const rounded = (s, r) => {
    const k = Math.min(r, s.h);
    return `M${s.x},${s.y + s.h}V${s.y + k}Q${s.x},${s.y} ${s.x + k},${s.y}H${s.x + s.w - k}Q${s.x + s.w},${s.y} ${s.x + s.w},${s.y + k}V${s.y + s.h}Z`;
  };

  const legend = (rows, order) => el("div", { class: "cost-legend" }, order.map((s) => {
    const cost = rows.reduce((sum, r) => sum + s.of(r), 0);
    const shown = s.id === "own" ? cost >= 0.005 : rows.some((r) => s.of(r) > 0 || (r.agent === s.id));
    return shown ? el("span", { class: "item" }, dot(s.color), el("span", null, s.name), el("span", { class: "strong" }, money(cost))) : null;
  }));

  /// A table: the header, then a row each; the first column says what, the others are numbers.
  const table = (columns, rows) =>
    el("div", { class: "card cost-table", style: `--columns:${columns.length - 1}`, role: "table" },
      el("div", { class: "head", role: "row" }, columns.map((c, i) => {
        const [text, help] = Array.isArray(c) ? c : [c];
        return el("span", { role: "columnheader", class: i ? "right" : "", title: help }, text);
      })),
      rows.map((cells) => el("div", { role: "row" }, cells.map((c, i) => el("span", { role: "cell", class: i ? "right" : "lines" }, c)))));

  /// A cost with its tokens under it.
  const both = (cost, tokens) => el("span", { class: "both" }, el("span", null, money(cost)), el("span", { class: "quiet" }, count(tokens)));

  /// The span's costs by kind of token, a row per harness, model or folder.
  const breakdown = (rows) => {
    const key = { agent: (r) => r.agent, model: (r) => `${r.agent}\n${r.model}`, folder: (r) => r.folder || "" }[choice.rows];
    const groups = new Map();
    for (const r of rows) {
      const g = groups.get(key(r)) || { rows: [], sessions: new Set(), models: new Set() };
      g.rows.push(r);
      g.sessions.add(`${r.agent}:${r.session}`);
      g.models.add(r.model);
      groups.set(key(r), g);
    }
    const sorted = [...groups].map(([k, g]) => [k, g, total(g.rows)]).sort((a, b) => b[2].cost - a[2].cost || b[2].tokens - a[2].tokens);
    const label = (k, g) => {
      if (choice.rows === "agent") {
        return [el("span", { class: "ellipsis strong" }, swatch(k), agentName(k)),
          el("span", { class: "quiet ellipsis" }, `${Clak.plural(g.sessions.size, "conversation", "conversations")} · ${Clak.plural(g.models.size, "model", "models")}`)];
      }
      if (choice.rows === "model") {
        const [agent, model] = k.split("\n");
        return [el("span", { class: "ellipsis mono", title: model }, model), el("span", { class: "quiet ellipsis" }, swatch(agent), agentName(agent))];
      }
      const name = k ? k.split(/[\\/]/).filter(Boolean).pop() || k : "No folder";
      return [el("span", { class: "ellipsis strong", title: k }, name),
        el("span", { class: "quiet ellipsis", title: k }, Clak.plural(g.sessions.size, "conversation", "conversations") + (k ? " · " + k : ""))];
    };
    const shown = choice.rows === "folder" ? sorted.slice(0, 15) : sorted;
    return table([{ agent: "Harness", model: "Model", folder: "Folder" }[choice.rows], "Requests", ...KINDS.map(([k, name]) => [name, KIND_HELP[k]]), "Total"],
      shown.map(([k, g, t]) => {
        const cost = t.unpriced === t.requests
          ? el("span", { class: "quiet-text", title: "No price for this model" }, "—")
          : el("span", { class: "strong", title: [t.unpriced ? `${Clak.plural(t.unpriced, "request", "requests")} without a price` : "",
            t.own >= 0.005 ? `${money(t.own)} worked out by the agent itself, not by kind` : ""].filter(Boolean).join("; ") },
          money(t.cost) + (t.unpriced || t.own >= 0.005 ? "*" : ""));
        return [label(k, g), count(t.requests), ...KINDS.map(([k2]) => both(t[`cost_${k2}`], t[k2])), cost];
      }));
  };

  const footnote = () => {
    const p = report.prices;
    const where = p.updated
      ? `Prices from LiteLLM's list, read ${Describe.ago(Math.floor(p.updated / 1000))}`
      : "Prices from the list that came with Clak: LiteLLM's couldn't be read yet";
    const own = total(report.rows).own >= 0.005 ? " OpenCode's own costs, for the models with no price, aren't split by kind (*)." : "";
    const unpriced = report.unpriced.length
      ? ` No price for ${report.unpriced.map((u) => u.model).join(", ")}: not counted.`
      : "";
    return el("p", { class: "quiet cost-note" },
      `${where}. With a subscription you pay a flat fee: these are the same tokens at the API's prices.${own}${unpriced}`);
  };

  // --- the page ------------------------------------------------------------------------

  /// Buttons of which one is on: `choice[k]` among `options` ([value, label]).
  const picker = (k, options, label) => el("div", { class: "usage-span", role: "group", "aria-label": label }, options.map(([id, text]) =>
    el("button", { class: "small" + (choice[k] === id ? " selected" : ""), "aria-pressed": String(choice[k] === id),
      onclick: () => choose(k, id) }, text)));

  const draw = () => {
    if (!body) return;
    if (failed && !report) return Kit.replace(body, Kit.bar("err", `Couldn't read the costs: ${failed}`));
    if (!report) return Kit.replace(body, el("div", { class: "quiet" }, "Reading the agents' files…"));
    const rows = report.rows;
    if (!rows.length) {
      return Kit.replace(body, Kit.empty("costs", "No tokens yet",
        "What Claude Code, Codex, Gemini CLI and OpenCode spend shows up here"), footnote());
    }
    const shown = since(rows, from(choice.span));
    const spanTotal = total(shown);
    const order = chartSeries();
    Kit.replace(body,
      tiles(rows),
      report.window ? windowCard(report.window) : null,
      Kit.section(SPANS.find(([id]) => id === choice.span)?.[1] || "Span", {
        help: "What each day cost; today, each hour",
        right: picker("span", SPANS, "Span"),
      }),
      el("div", { class: "card cost-span" },
        el("div", { class: "head" },
          el("div", null, el("div", { class: "value" }, money(spanTotal.cost)),
            el("div", { class: "quiet" }, `${count(spanTotal.tokens)} tokens · ${Clak.plural(spanTotal.requests, "request", "requests")}`)),
          picker("colors", [["agent", "Harness"], ["kind", "Token type"]], "Colors")),
        legend(shown, order),
        chart(shown, order)),
      shown.length ? [Kit.section("Breakdown", {
        help: "What each kind of token cost, with how many there were: the cache is written and read at its own price",
        right: picker("rows", [["agent", "Harness"], ["model", "Model"], ["folder", "Folder"]], "Group by"),
      }), breakdown(shown)] : null,
      footnote());
    // The chart is as wide as its card: measured once drawn, drawn again if it was off.
    measured = body.clientWidth;
    const fits = body.querySelector(".cost-chart")?.clientWidth;
    if (fits && Math.abs(fits - width) > 1) {
      width = fits;
      draw();
    }
  };

  const load = async (refresh) => {
    if (busy) return;
    busy = true;
    const first = !report;
    try {
      report = await Clak.invoke("costs_report", { refresh: !!refresh });
      failed = null;
    } catch (e) {
      failed = String(e);
    } finally {
      busy = false;
    }
    draw();
    if (first) grow();
  };

  const visible = () => page && !page.classList.contains("hidden") && document.visibilityState === "visible";

  Panel.tab({
    id: "usage",
    title: "Usage",
    icon: Kit.navIcon("costs", Kit.TINTS.costs),
    order: 65,
    mount(section) {
      page = section;
      page.classList.add("usage-costs");
      body = el("div", { class: "cost-body" });
      page.append(Kit.title("Usage", Kit.iconButton("refresh", "Read again, the prices included", () => load(true))), body);
      window.addEventListener("focus", () => visible() && load());
      setInterval(() => visible() && load(), 60_000);
      new ResizeObserver(() => {
        if (body.clientWidth && body.clientWidth !== measured) draw();
      }).observe(body);
    },
    shown() {
      draw();
      load();
    },
  });
})();
