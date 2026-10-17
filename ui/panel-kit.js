// The pieces the panel tabs are drawn with: icons, cards, sections, pills,
// switches, alert bars. On Windows the icons are the Segoe Fluent Icons glyphs,
// on the Mac drawings in the style of SF symbols.
"use strict";

const Kit = (() => {
  const el = Clak.el;

  // The Segoe Fluent Icons glyphs (Segoe MDL2 Assets on Windows 10).
  const GLYPHS = {
    status: "", agents: "", apps: "", mcp: "", api: "", log: "",
    settings: "", program: "", connector: "", terminal: "", chat: "",
    people: "", folder: "", chevron: "", back: "", copy: "", skill: "",
    client: "", file: "", refresh: "", search: "", trash: "", power: "",
    bell: "", speaker: "", options: "", calls: "", warn: "", stopwatch: "",
    shield: "", ok: "", err: "", info: "", off: "", minus: "", plus: "",
  };

  // On the Mac: 16 px strokes, like SF symbols at regular weight.
  const filled = (shape, mark) => `${shape}<path d="${mark}" stroke="#fff" stroke-width="1.6"/>`;
  const PATHS = {
    status: '<circle cx="8" cy="8" r="1.6" fill="currentColor" stroke="none"/><path d="M5.2 5.2a4 4 0 0 0 0 5.6M10.8 5.2a4 4 0 0 1 0 5.6M3.1 3.1a7 7 0 0 0 0 9.8M12.9 3.1a7 7 0 0 1 0 9.8"/>',
    agents: '<path d="M7 1.5l1.3 3.7L12 6.5 8.3 7.8 7 11.5 5.7 7.8 2 6.5l3.7-1.3zM12.5 10l.6 1.4 1.4.6-1.4.6-.6 1.4-.6-1.4-1.4-.6 1.4-.6z" fill="currentColor" stroke="none"/>',
    apps: '<rect x="2" y="2" width="12" height="12" rx="3"/><path d="M5.2 8.2l2 2 3.8-4"/>',
    log: '<rect x="2.5" y="1.5" width="11" height="13" rx="2"/><path d="M5 5h6M5 8h6M5 11h4"/>',
    settings: '<circle cx="8" cy="8" r="2.2"/><path d="M8 1.5v2M8 12.5v2M1.5 8h2M12.5 8h2M3.4 3.4l1.4 1.4M11.2 11.2l1.4 1.4M3.4 12.6l1.4-1.4M11.2 4.8l1.4-1.4"/>',
    program: '<rect x="1.5" y="2.5" width="13" height="11" rx="2"/><path d="M1.5 5.5h13"/>',
    connector: '<rect x="2" y="2.5" width="12" height="4.5" rx="1.2"/><rect x="2" y="9" width="12" height="4.5" rx="1.2"/><path d="M4.5 4.75h.5M4.5 11.25h.5"/>',
    mcp: '<rect x="2" y="2.5" width="12" height="4.5" rx="1.2"/><rect x="2" y="9" width="12" height="4.5" rx="1.2"/><path d="M4.5 4.75h.5M4.5 11.25h.5"/>',
    api: '<circle cx="8" cy="8" r="6"/><path d="M2 8h12M8 2c2 2 2 10 0 12M8 2c-2 2-2 10 0 12"/>',
    terminal: '<rect x="1.5" y="2.5" width="13" height="11" rx="2"/><path d="M4.5 6l2 2-2 2M8 10.5h3.5"/>',
    chat: '<path d="M2 3.5A1.5 1.5 0 0 1 3.5 2h9A1.5 1.5 0 0 1 14 3.5v6a1.5 1.5 0 0 1-1.5 1.5H7l-3 3v-3h-.5A1.5 1.5 0 0 1 2 9.5z"/>',
    people: '<circle cx="6" cy="5.5" r="2.3"/><path d="M1.8 13.5c.5-2.5 2.2-3.8 4.2-3.8s3.7 1.3 4.2 3.8"/><circle cx="11.5" cy="6" r="1.8"/><path d="M11.8 9.6c1.5.2 2.4 1.4 2.7 3"/>',
    folder: '<path d="M1.5 4A1.5 1.5 0 0 1 3 2.5h3L7.5 4H13a1.5 1.5 0 0 1 1.5 1.5V12a1.5 1.5 0 0 1-1.5 1.5H3A1.5 1.5 0 0 1 1.5 12z"/>',
    chevron: '<path d="M6 3l5 5-5 5"/>',
    back: '<path d="M10 3L5 8l5 5"/>',
    copy: '<rect x="5" y="5" width="9" height="9" rx="1.5"/><path d="M11 5V3.5A1.5 1.5 0 0 0 9.5 2h-6A1.5 1.5 0 0 0 2 3.5v6A1.5 1.5 0 0 0 3.5 11H5"/>',
    skill: '<path d="M2 3c2-1 4-1 6 .5 2-1.5 4-1.5 6-.5v10c-2-1-4-1-6 .5-2-1.5-4-1.5-6-.5zM8 3.5v10"/>',
    client: '<path d="M5.5 1.5v3M10.5 1.5v3M3.5 4.5h9V7a4.5 4.5 0 0 1-9 0zM8 11.5v3"/>',
    file: '<path d="M4 1.5h5l3.5 3.5v9.5H4zM9 1.5V5h3.5"/>',
    refresh: '<path d="M13.5 8a5.5 5.5 0 1 1-1.6-3.9M13.5 2.5v3h-3"/>',
    search: '<circle cx="7" cy="7" r="4.5"/><path d="M10.3 10.3L14 14"/>',
    trash: '<path d="M2.5 4h11M6 4V2.5h4V4M4 4l.7 9.5h6.6L12 4"/>',
    power: '<path d="M8 1.5v6M4.5 3.8a5.5 5.5 0 1 0 7 0"/>',
    bell: '<path d="M4 11V7a4 4 0 0 1 8 0v4l1.5 1.5h-11zM6.5 14a1.5 1.5 0 0 0 3 0"/>',
    speaker: '<path d="M2 6h2.5L8 3v10l-3.5-3H2z"/><path d="M10.5 5.5a3.5 3.5 0 0 1 0 5M12.5 3.5a6.5 6.5 0 0 1 0 9"/>',
    options: '<path d="M2 4h7M12 4h2M2 12h2M7 12h7"/><circle cx="10.5" cy="4" r="1.5"/><circle cx="5.5" cy="12" r="1.5"/>',
    calls: '<path d="M2 5.5h11M10 2.5l3 3-3 3M14 10.5H3M6 7.5l-3 3 3 3"/>',
    stopwatch: '<circle cx="8" cy="9" r="5.5"/><path d="M8 9V6M6.5 1.5h3"/>',
    shield: '<path d="M8 1.5l5.5 2v4c0 3.5-2.4 6-5.5 7-3.1-1-5.5-3.5-5.5-7v-4z"/><path d="M5.5 8l1.8 1.8 3.5-3.5"/>',
    ok: filled('<circle cx="8" cy="8" r="6.5" fill="currentColor" stroke="none"/>', "M5 8.2l2 2 4-4"),
    warn: filled('<path d="M8 1.5l6.8 12H1.2z" fill="currentColor" stroke="none"/>', "M8 6v3.5M8 11.3v.4"),
    err: filled('<circle cx="8" cy="8" r="6.5" fill="currentColor" stroke="none"/>', "M5.7 5.7l4.6 4.6M10.3 5.7l-4.6 4.6"),
    info: filled('<circle cx="8" cy="8" r="6.5" fill="currentColor" stroke="none"/>', "M8 7.3v4M8 4.6v.4"),
    minus: filled('<circle cx="8" cy="8" r="6.5" fill="currentColor" stroke="none"/>', "M5 8h6"),
    off: '<circle cx="8" cy="8" r="6" stroke-dasharray="2.2 2"/>',
    plus: '<path d="M8 2.5v11M2.5 8h11"/>',
  };

  const svg = (name) =>
    `<svg viewBox="0 0 16 16" fill="none" stroke="currentColor" stroke-width="1.4" stroke-linecap="round" stroke-linejoin="round" aria-hidden="true">${PATHS[name] || PATHS.program}</svg>`;

  /// An icon, as an element.
  const icon = (name, cls) => {
    const node = el("span", { class: "glyph" + (cls ? " " + cls : ""), "aria-hidden": "true" });
    if (Clak.mac) node.innerHTML = svg(name);
    else node.textContent = GLYPHS[name] || GLYPHS.program;
    return node;
  };

  /// The icon of a navigation item, as text for `Panel.tab({ icon })`. On the Mac in
  /// a colored tile, as in the System Settings sidebar.
  const navIcon = (name, tint) =>
    Clak.mac
      ? `<span class="tile" style="--tint:${tint}">${svg(name)}</span>`
      : `<span class="glyph">${GLYPHS[name]}</span>`;

  /// A tile with a symbol; with `exe`, the program's own icon takes its place once read.
  const tile = (name, size, tint, exe) => {
    const node = el("span", { class: "tile", style: `--size:${size || 32}px${tint ? `;--tint:${tint}` : ""}` }, icon(name));
    return exe ? Clak.withIcon(node, exe, size || 32) : node;
  };

  const TINTS = { status: "#34c759", agents: "#ff9500", apps: "#007aff", log: "#8e8e93", settings: "#8e8e93" };

  /// The page title, with what it needs on the right (a filter, a button).
  const title = (text, ...right) => el("header", { class: "title" }, el("h1", null, text), right.length ? el("div", { class: "actions" }, right) : null);

  /// A section title, with the small count and what goes on the right.
  const section = (text, options = {}) =>
    el("div", { class: "section" },
      el("h2", { title: options.help }, text),
      options.count === undefined ? null : el("span", { class: "count" }, String(options.count)),
      options.right ? el("div", { class: "actions" }, options.right) : null);

  const card = (...children) => el("div", { class: "card" }, children);

  /// "None", with the icon above: an empty list.
  const empty = (name, text, help) =>
    el("div", { class: "empty", title: help }, name ? icon(name) : null, el("strong", null, text));

  /// The pill of a state: tone is ok | warn | err | off | accent.
  const badge = (text, tone) => el("span", { class: "badge " + (tone || "off") }, text);

  /// A label: `clak notion`, "12 commands".
  const tag = (text, options = {}) =>
    el("span", { class: "tag" + (options.accent ? " accent" : "") + (options.tone ? " " + options.tone : "") + (options.mono ? " mono" : ""), title: options.help }, text);

  /// An alert bar (info | warn | err | ok), with a button on the right if needed.
  const bar = (tone, text, button) =>
    el("div", { class: "bar " + tone }, icon(tone === "warn" ? "warn" : tone === "err" ? "err" : tone === "ok" ? "ok" : "info"),
      el("div", { class: "text" }, text), button || null);

  const iconButton = (name, label, onclick) =>
    el("button", { class: "icon-button", title: label, "aria-label": label, onclick }, icon(name));

  /// A button that looks like a link, with the icon in front.
  const link = (text, name, onclick, help) =>
    el("button", { class: "link", title: help, onclick }, name ? icon(name) : null, el("span", null, text));

  /// A switch. On Windows with "On"/"Off" next to it, as in WinUI.
  const toggle = (checked, label, onchange, disabled) => {
    // Also as an attribute: so a value changed from outside redraws the row (see replace).
    const input = el("input", { type: "checkbox", role: "switch", "aria-label": label, disabled: !!disabled, checked: !!checked });
    input.checked = !!checked;
    const state = el("span", { class: "state" }, checked ? "On" : "Off");
    input.addEventListener("change", () => {
      state.textContent = input.checked ? "On" : "Off";
      onchange(input.checked);
    });
    return el("label", { class: "switch" }, state, input, el("span", { class: "track" }));
  };

  /// A row that opens on click, with the arrow on the right.
  const clickRow = (content, onclick, help, label) =>
    el("button", { class: "click-row", onclick, title: help, "aria-label": label }, content, icon("chevron", "chevron"));

  /// Replaces the content only if it really changed: a list that is recreated every
  /// time takes away the mouse hover and the selection.
  const replace = (container, ...nodes) => {
    const draft = document.createElement(container.tagName);
    draft.append(...nodes.flat(Infinity).filter((n) => n != null && n !== false));
    if (draft.innerHTML === container.innerHTML) return false;
    container.replaceChildren(...draft.childNodes);
    return true;
  };

  /// Copies to the clipboard.
  const copy = async (text) => {
    try {
      await navigator.clipboard.writeText(text);
      return true;
    } catch {
      const area = el("textarea", { style: "position:fixed;opacity:0" });
      area.value = text;
      document.body.append(area);
      area.select();
      const done = document.execCommand("copy");
      area.remove();
      return done;
    }
  };

  return { icon, navIcon, tile, TINTS, title, section, card, empty, badge, tag, bar, iconButton, link, toggle, clickRow, replace, copy };
})();
