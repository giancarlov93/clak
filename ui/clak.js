// What all the pages use: the system they run on, the app's commands, the events.
"use strict";

const Clak = (() => {
  const ua = navigator.userAgent;
  const mac = /Macintosh|Mac OS X/.test(ua);
  document.documentElement.classList.add(mac ? "mac" : "win");

  const tauri = window.__TAURI__;
  const invoke = (cmd, args) => tauri.core.invoke(cmd, args || {});
  const listen = (event, handler) => tauri.event.listen(event, (e) => handler(e.payload));

  const el = (tag, props, ...children) => {
    const node = document.createElement(tag);
    for (const [key, value] of Object.entries(props || {})) {
      if (value === undefined || value === null || value === false) continue;
      if (key === "class") node.className = value;
      else if (key.startsWith("on")) node.addEventListener(key.slice(2), value);
      else node.setAttribute(key, value === true ? "" : value);
    }
    for (const child of children.flat()) {
      if (child === undefined || child === null || child === false) continue;
      node.append(child instanceof Node ? child : document.createTextNode(String(child)));
    }
    return node;
  };

  const plural = (n, one, many) => `${n} ${n === 1 ? one : many}`;

  /// The real icon of a program or an agent, read from its executable by the app, or of an
  /// MCP server or a REST API (`mcp:<id>`, `api:<id>`, server_icon.rs), or Clak's own (`clak`): a `data:` URL, or null for
  /// one without an icon of its own. Asked once per path and size.
  const SERVER = /^(mcp|api):|^clak$/;
  const icons = new Map();
  const appIcon = (path, size) => {
    if (!path) return Promise.resolve(null);
    const pixels = Math.ceil(size * (window.devicePixelRatio || 1));
    const key = `${pixels}|${path}`;
    const ask = SERVER.test(path)
      ? () => invoke("server_icon", { source: path, size: pixels })
      : () => invoke("app_icon", { path, size: pixels });
    if (!icons.has(key)) icons.set(key, ask().catch(() => null));
    return icons.get(key);
  };

  /// What a tile showed before its icon: put back if the icon goes away.
  const glyphs = new WeakMap();

  const putIcon = (node, path, size) =>
    appIcon(path, size).then((url) => {
      if (!url || node.dataset.icon !== path) return;
      node.classList.add("image");
      // A server's icon is often a full square: rounded like the app icons around it.
      node.classList.toggle("site", SERVER.test(path) && path !== "clak");
      node.replaceChildren(el("img", { src: url, alt: "", width: size, height: size, draggable: "false" }));
    });

  /// Puts the icon of `path` into `node` once it arrives; until then (and without one) the
  /// node keeps what it shows.
  const withIcon = (node, path, size) => {
    if (!path) return node;
    glyphs.set(node, [...node.childNodes]);
    node.dataset.icon = path;
    node.dataset.iconSize = size;
    putIcon(node, path, size);
    return node;
  };

  // Server icons turned on or off in Settings: the tiles on screen ask again.
  listen("server-icons", () => {
    for (const key of [...icons.keys()]) if (SERVER.test(key.split("|")[1])) icons.delete(key);
    for (const node of document.querySelectorAll("[data-icon]")) {
      if (!SERVER.test(node.dataset.icon)) continue;
      node.classList.remove("image", "site");
      node.replaceChildren(...(glyphs.get(node) || []));
      putIcon(node, node.dataset.icon, Number(node.dataset.iconSize));
    }
  });

  return { mac, invoke, listen, el, plural, appIcon, withIcon };
})();
