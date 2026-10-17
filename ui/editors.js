// What the MCP and APIs tabs and their dialogs have in common, besides panel-kit.js and
// describe.js (which the dialogs don't load): the dialog icons, the automatic name for
// the command line, the cards' "More" menu and the window the page runs in.
"use strict";

const Editors = (() => {
  const { el } = Clak;

  // The icons, drawn on 16 px like the Fluent glyphs and SF symbols they replace.
  const paths = {
    server: '<rect x="2.5" y="2.5" width="11" height="4.5" rx="1.2"/><rect x="2.5" y="9" width="11" height="4.5" rx="1.2"/><circle cx="5" cy="4.75" r=".6" fill="currentColor"/><circle cx="5" cy="11.25" r=".6" fill="currentColor"/>',
    key: '<circle cx="5.5" cy="10.5" r="3"/><path d="m7.6 8.4 5.4-5.4M11 5l1.5 1.5M9.5 6.5 11 8"/>',
    api: '<circle cx="3.5" cy="8" r="1.8"/><circle cx="12.5" cy="3.5" r="1.8"/><circle cx="12.5" cy="12.5" r="1.8"/><path d="M5.2 7.2 10.8 4.3M5.2 8.8l5.6 2.9"/>',
    plus: '<path d="M8 3v10M3 8h10"/>',
    more: '<circle cx="3.5" cy="8" r=".9" fill="currentColor"/><circle cx="8" cy="8" r=".9" fill="currentColor"/><circle cx="12.5" cy="8" r=".9" fill="currentColor"/>',
    edit: '<path d="M10.5 2.5 13.5 5.5 5.5 13.5H2.5V10.5z"/>',
    trash: '<path d="M3 4.5h10M6.5 4.5V3h3v1.5M4.5 4.5l.6 8.5h5.8l.6-8.5"/>',
    people: '<circle cx="6" cy="5.5" r="2.3"/><path d="M1.8 13c.5-2.3 2.2-3.5 4.2-3.5s3.7 1.2 4.2 3.5"/><path d="M10.5 3.4a2.2 2.2 0 0 1 0 4.2M11.6 9.6c1.4.4 2.3 1.6 2.6 3.4"/>',
    folder: '<path d="M2 4.5V12.5H14V6H8L6.5 4.5z"/>',
    test: '<path d="M6 2.5v4L2.8 12.3a.8.8 0 0 0 .7 1.2h9a.8.8 0 0 0 .7-1.2L10 6.5v-4M5 2.5h6M4.5 9.5h7"/>',
    lock: '<rect x="3" y="7.5" width="10" height="6.5" rx="1.2"/><path d="M5.5 7.5V5a2.5 2.5 0 0 1 4.8-1"/>',
    link: '<path d="M7 9a2.5 2.5 0 0 0 3.5 0l2-2a2.5 2.5 0 0 0-3.5-3.5l-.7.7M9 7a2.5 2.5 0 0 0-3.5 0l-2 2A2.5 2.5 0 0 0 7 12.5l.7-.7"/>',
  };

  const icon = (name, size = 16) =>
    `<svg viewBox="0 0 16 16" width="${size}" height="${size}" fill="none" stroke="currentColor" stroke-width="1.3" stroke-linecap="round" stroke-linejoin="round" aria-hidden="true">${paths[name] || ""}</svg>`;

  /// An element with an icon inside (the SVG is ours: nothing that comes from the hub).
  const glyph = (name, cls, size) => {
    const node = el("span", { class: "ed-glyph" + (cls ? " " + cls : "") });
    node.innerHTML = icon(name, size);
    return node;
  };

  // --- the sentences -------------------------------------------------------

  const plural = (n, one, many) => `${n} ${n === 1 ? one : many}`;

  const RESERVED = ["list", "status", "help", "version", "settings"];

  /// The automatic name for the command line (CliNames.From): "Notion (work)" → "notion-work".
  const cliFrom = (name) => {
    let slug = String(name || "").normalize("NFD").replace(/\p{Mn}/gu, "").toLowerCase()
      .replace(/[^a-z0-9]+/g, "-").replace(/^-+|-+$/g, "");
    if (slug.length > 24) slug = slug.slice(0, 24).replace(/-+$/, "");
    if (slug.length === 0 || RESERVED.includes(slug)) slug = `app-${slug}`.replace(/-+$/, "");
    return slug;
  };

  // --- the tab pieces (next to those of panel-kit.js) --------------------------

  // On Windows the Segoe Fluent Icons glyphs; on the Mac the drawings above.
  const FLUENT = { key: "\ue8d7", connector: "\ue968", api: "\ue774", more: "\ue712", plus: "\ue710" };
  const SVG = { key: "key", connector: "server", api: "api", more: "more", plus: "plus" };

  const fluent = (name) => {
    const node = el("span", { class: "glyph", "aria-hidden": "true" });
    if (Clak.mac) node.innerHTML = icon(SVG[name]);
    else node.textContent = FLUENT[name];
    return node;
  };

  /// The tile of a server or an API: colored on the Mac, faint with the glyph on Windows.
  const tile = (name, tint) => el("span", { class: "tile", style: `--size:36px;--tint:${tint}` }, fluent(name));

  /// The icon in front of a button's text ("＋ Add…").
  const buttonIcon = (name) => {
    const node = fluent(name);
    node.classList.add("ed-button-glyph");
    return node;
  };

  // --- the "More" menu -------------------------------------------------------

  let openMenu = null;
  const closeMenu = () => {
    if (!openMenu) return;
    openMenu.remove();
    openMenu = null;
  };
  document.addEventListener("mousedown", (e) => {
    if (openMenu && !openMenu.contains(e.target)) closeMenu();
  }, true);
  document.addEventListener("keydown", (e) => {
    if (e.key === "Escape") closeMenu();
  });
  window.addEventListener("blur", closeMenu);
  window.addEventListener("resize", closeMenu);

  /// The menu below a button: [{ label, run, danger }] or "-" for a separator.
  const menu = (anchor, items) => {
    closeMenu();
    const node = el("div", { class: "ed-menu", role: "menu" });
    for (const item of items) {
      if (item === "-") {
        node.append(el("div", { class: "ed-sep", role: "separator" }));
        continue;
      }
      node.append(el("button", {
        class: "ed-item" + (item.danger ? " danger" : ""),
        role: "menuitem",
        onclick: () => {
          closeMenu();
          item.run();
        },
      }, item.label));
    }
    document.body.append(node);
    const box = anchor.getBoundingClientRect();
    const width = node.offsetWidth;
    const height = node.offsetHeight;
    const left = Math.max(8, Math.min(box.right - width, innerWidth - width - 8));
    const below = box.bottom + 4 + height <= innerHeight - 8;
    node.style.left = `${left}px`;
    node.style.top = `${below ? box.bottom + 4 : Math.max(8, box.top - height - 4)}px`;
    openMenu = node;
    node.querySelector("button")?.focus();
    node.addEventListener("keydown", (e) => {
      const buttons = [...node.querySelectorAll("button")];
      const at = buttons.indexOf(document.activeElement);
      if (e.key === "ArrowDown") buttons[(at + 1) % buttons.length].focus();
      else if (e.key === "ArrowUp") buttons[(at - 1 + buttons.length) % buttons.length].focus();
      else return;
      e.preventDefault();
    });
  };

  /// A card's "More" button; the items are requested on click, with the data of that moment.
  const moreButton = (items) => {
    const button = el("button", { class: "icon-button", title: "More", "aria-label": "More", "aria-haspopup": "menu" }, fluent("more"));
    button.addEventListener("click", () => menu(button, items()));
    return button;
  };

  // --- the page's window ----------------------------------------------------

  /// The events sent to this window only.
  const listenHere = (event, handler) =>
    window.__TAURI__.webviewWindow.getCurrentWebviewWindow().listen(event, (e) => handler(e.payload));

  /// The window takes the height of the content, and appears.
  const fitToContent = () => {
    const fit = () => Clak.invoke("editor_fit", { height: Math.ceil(document.body.scrollHeight) });
    new ResizeObserver(fit).observe(document.body);
    fit();
  };

  /// An invoke error as text (empty: nothing to say).
  const message = (error) => (typeof error === "string" ? error : error?.message || String(error || ""));

  return { icon, glyph, plural, cliFrom, tile, buttonIcon, menu, closeMenu, moreButton, listenHere, fitToContent, message };
})();
