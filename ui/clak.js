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

  /// The real icon of a program or an agent, read from its executable by the app: a
  /// `data:` URL, or null for one without an icon of its own. Asked once per path and size.
  const icons = new Map();
  const appIcon = (path, size) => {
    if (!path) return Promise.resolve(null);
    const pixels = Math.ceil(size * (window.devicePixelRatio || 1));
    const key = `${pixels}|${path}`;
    if (!icons.has(key)) icons.set(key, invoke("app_icon", { path, size: pixels }).catch(() => null));
    return icons.get(key);
  };

  /// Puts the icon of `path` into `node` once it arrives; until then (and without one) the
  /// node keeps what it shows.
  const withIcon = (node, path, size) => {
    appIcon(path, size).then((url) => {
      if (!url) return;
      node.classList.add("image");
      node.replaceChildren(el("img", { src: url, alt: "", width: size, height: size, draggable: "false" }));
    });
    return node;
  };

  return { mac, invoke, listen, el, plural, appIcon, withIcon };
})();
