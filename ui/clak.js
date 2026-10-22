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

  /// The runtime a program runs in, when its executable is one (`javaw.exe`,
  /// `python.exe`…): the signature is the runtime's, not the program's.
  const RUNTIMES = [
    [/^javaw?$/, "Java"], [/^pythonw?[\d.]*$|^pyw?$/, "Python"], [/^node$/, "Node.js"], [/^bun$/, "Bun"],
    [/^deno$/, "Deno"], [/^dotnet$/, ".NET"], [/^rubyw?$/, "Ruby"], [/^perl$/, "Perl"], [/^pwsh$|^powershell$/, "PowerShell"],
    [/^[wc]script$/, "Windows Script Host"], [/^osascript$/, "AppleScript"], [/^electron$/, "Electron"],
  ];
  const runtime = (exe) => {
    const name = String(exe || "").split(/[\\/]/).pop().toLowerCase().replace(/\.exe$/, "");
    return RUNTIMES.find(([pattern]) => pattern.test(name))?.[1] || null;
  };

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

  /// Motion, as WinUI and AppKit move: what arrives decelerates into place. None when the
  /// system asks for less. Web Animations, not classes: the markup stays the same, and a
  /// redraw (Kit.replace) neither sees a difference nor plays it again.
  const still = matchMedia("(prefers-reduced-motion: reduce)");
  const animate = (node, frames, options) =>
    still.matches || !node?.animate ? null : node.animate(frames, { easing: "cubic-bezier(0, 0, 0, 1)", fill: "backwards", ...options });

  /// What arrives: it rises into place (or drops, with a negative rise) as it appears.
  const enter = (node, { rise = 8, duration = mac ? 200 : 250, delay = 0 } = {}) =>
    animate(node, [{ opacity: 0, transform: `translateY(${rise}px)` }, { opacity: 1, transform: "none" }], { duration, delay });

  /// Whether the node, or what holds it, is already arriving: then its content needn't.
  const arriving = (node) => {
    for (let n = node; n; n = n.parentElement) if (n.getAnimations?.().some((a) => a.playState === "running")) return true;
    return false;
  };

  // An expander opened by hand: what it holds drops into place. One opened by a redraw is just there.
  document.addEventListener("click", (e) => {
    const summary = e.target.closest?.("summary");
    const details = summary?.parentElement;
    if (details?.tagName !== "DETAILS") return;
    requestAnimationFrame(() => {
      if (details.open) for (const child of details.children) if (child !== summary) enter(child, { rise: -6, duration: 167 });
    });
  });

  return { mac, invoke, listen, el, plural, runtime, appIcon, withIcon, animate, enter, arriving };
})();
