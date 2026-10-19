// The Settings tab, in three parts: General (when the app starts, how it notifies, where the
// hub lives), Integration (how the agents reach Clak: panel-setup.js) and About (the version
// and the updates). No setting touches the permissions, which are given only in the consent
// windows. It is re-read on every opening: startup also changes from Task Manager (on the Mac
// from Login Items).
"use strict";

(() => {
  const el = Clak.el;
  // The app icon (icons/icon.svg), for "About".
  const LOGO = '<svg viewBox="0 0 32 32" aria-hidden="true"><line x1="16" y1="2.5" x2="16" y2="7" stroke="#2F5BEA" stroke-width="2.6" stroke-linecap="round"/><circle cx="16" cy="2.6" r="2.2" fill="#2F5BEA"/><rect x="2" y="6" width="28" height="25" rx="7" fill="#2F5BEA"/><polyline points="8.5,13 13.5,18 8.5,23" fill="none" stroke="#fff" stroke-width="3.2" stroke-linecap="round" stroke-linejoin="round"/><line x1="16.5" y1="23.5" x2="23.5" y2="23.5" stroke="#fff" stroke-width="3.2" stroke-linecap="round"/></svg>';

  let page = null;
  let pivot = null;
  /// The parts: General and About drawn here, Integration by Setup.
  const parts = {};
  let part = "general";
  let data = null;
  let state = Panel.state();
  let message = "";

  const refresh = async () => {
    try {
      data = await Clak.invoke("settings_status");
    } catch (e) {
      message = String(e);
    }
    draw();
  };

  const open = (target) => Clak.invoke("settings_open", { target }).catch((e) => {
    message = String(e);
    draw();
  });

  const setAutostart = async (on) => {
    try {
      await Clak.invoke("prefs_set", { name: "autostart", value: on });
      message = "";
    } catch (e) {
      message = `Couldn't turn it ${on ? "on" : "off"}: ${e}`;
    }
    await refresh();
  };

  const setPreference = (name) => (on) => Clak.invoke("prefs_set", { name, value: on }).catch((e) => {
    message = String(e);
    refresh();
  });

  /// A row: icon, title, the detail below, and the control on the right.
  const setting = (glyph, tint, title, detail, control, options = {}) =>
    el("div", { class: "setting", title: options.help },
      Clak.mac ? Kit.tile(glyph, 28, tint) : Kit.icon(glyph, "lead"),
      el("div", { class: "lines" }, el("div", null, title),
        detail ? el("div", { class: "quiet selectable" + (options.mono ? " mono" : "") }, detail) : null,
        options.below || null),
      control ? el("div", { class: "actions" }, control) : null);

  const startup = () => {
    const a = data.autostart;
    const disabled = a.state === "disabled";
    const line = Clak.mac
      ? disabled ? "Waiting for approval"
        : a.state === "elsewhere" ? `Another copy starts instead: ${a.registered}`
        : a.installed ? "In the menu bar, without the panel" : "In the menu bar, without the panel · this copy, not the one in /Applications"
      : disabled ? "Disabled in Task Manager › Startup apps"
        : a.state === "elsewhere" ? `Another copy starts instead: ${a.registered}`
        : a.installed ? "In the tray, without the panel" : "In the tray, without the panel · this copy, not the installed one";
    const on = a.state === "on" || (Clak.mac && disabled);
    const below = [
      !Clak.mac && disabled ? Kit.link("Open Startup apps", null, () => open("startup")) : null,
      !Clak.mac && message ? el("div", { class: "quiet danger" }, message) : null,
    ];
    return [
      el("div", { class: "card" },
        setting("power", "#34c759", Clak.mac ? "Open Clak when you log in" : "Open Clak when you sign in to Windows", line,
          Kit.toggle(on, Clak.mac ? "Open Clak when you log in" : "Open at sign-in", setAutostart), { below }),
        Clak.mac && disabled
          ? Kit.bar("warn", "Needs approval in Login Items", el("button", { onclick: () => open("startup") }, "Open Login Items"))
          : Clak.mac && message ? Kit.bar("warn", message) : null),
    ];
  };

  const alerts = () =>
    el("div", { class: "card group settings" },
      setting("bell", "#ff3b30", "Notifications", null, Kit.toggle(data.notifications, "Notifications", setPreference("notifications")),
        { help: "When a program connects, and when an agent starts or stops using one" }),
      setting("speaker", "#ff2d55", "Sound for consent windows", null, Kit.toggle(data.consent_sound, "Sound for consent windows", setPreference("consent_sound"))),
      setting("speaker", "#ff2d55", "Sound when an agent connects", null, Kit.toggle(data.connection_sound, "Sound when an agent connects", setPreference("connection_sound"))),
      Clak.mac ? null : el("div", { class: "setting" }, Kit.icon("options", "lead"),
        Kit.link("Windows notification settings", null, () => open("notifications"), "If Windows notifications are off, tray alerts don't show")));

  const hub = () => {
    const broker = !state.offline && state.broker;
    const show = el("button", { onclick: () => open("home") }, Clak.mac ? "Show in Finder" : "Show in File Explorer");
    const rows = [setting("folder", "#007aff", "Folder", data.home, show, { mono: true, help: "CLAK_HOME moves it, for testing" })];
    if (broker && Clak.mac) {
      rows.push(setting("program", "#30b0c7", "Programs", `ws://127.0.0.1:${broker.ws_port}`, null, { mono: true }));
      rows.push(setting("agents", "#ff9500", "Agents", broker.control ?? "?", null, { mono: true }));
    } else if (broker) {
      rows.push(el("div", { class: "setting top" }, Kit.icon("status", "lead"),
        el("div", { class: "mono quiet selectable lines" }, el("div", null, `Programs: ws://127.0.0.1:${broker.ws_port}`), el("div", null, `Agents: ${broker.control ?? "?"}`))));
    }
    rows.push(setting("api", "#34aadc", "Server icons", null,
      Kit.toggle(data.server_icons, "Server icons", setPreference("server_icons")),
      { help: "MCP servers and APIs: their icons come from their websites" }));
    return el("div", { class: "card group settings" }, rows);
  };


  const download = () => Clak.invoke("update_open").catch((e) => {
    message = String(e);
    draw();
  });

  /// The update being installed: { version, downloaded, total }, { installing }, { error }.
  let progress = null;

  const install = () => {
    progress = { version: data.update?.newer?.version, downloaded: 0 };
    draw();
    // It returns only if the user said no, or on an error: otherwise Clak starts again.
    Clak.invoke("update_install").then(() => {
      if (progress && !progress.error) progress = null;
      draw();
    }, (e) => {
      progress = { error: String(e) };
      draw();
    });
  };

  const installing = () => {
    if (progress.error) return Kit.bar("warn", `Couldn't install the update: ${progress.error}`, el("button", { onclick: download }, "Download"));
    if (progress.installing) return Kit.bar("info", `Installing Clak ${progress.version}…`);
    const percent = progress.total ? Math.round((100 * progress.downloaded) / progress.total) : null;
    return Kit.bar("info", el("div", null, `Downloading Clak ${progress.version ?? ""}${percent === null ? "…" : `… ${percent}%`}`,
      el("div", { class: "meter" }, el("span", { style: `width: ${percent ?? 0}%` }))));
  };

  /// When GitHub last answered: the time today, the date before.
  const when = (seconds) => {
    const d = new Date(seconds * 1000);
    return d.toDateString() === new Date().toDateString()
      ? d.toLocaleTimeString([], { hour: "2-digit", minute: "2-digit" })
      : d.toLocaleString([], { day: "numeric", month: "short", hour: "2-digit", minute: "2-digit" });
  };

  /// Why Check now could not look at all (Clak inside another app's package).
  let checkError = null;

  const checkNow = () => {
    checkError = null;
    data.update = { ...data.update, checking: true };
    draw();
    Clak.invoke("update_check_now").then((u) => { data.update = u; }, (e) => {
      checkError = String(e);
      data.update = { ...data.update, checking: false };
    }).finally(draw);
  };

  /// Clak, its version and where its updates stand, with Check now: as in Windows Update.
  const about = () => {
    const u = data.update || {};
    const error = u.checking ? null : checkError || u.error;
    const logo = el("span", { class: "logo" });
    logo.innerHTML = LOGO;
    const line = u.checking ? "Checking for updates…"
      : u.newer && u.checked ? `Checked ${when(u.checked)}`
      : error ? "Couldn't check for updates"
      : u.checked ? `Up to date · checked ${when(u.checked)}`
      : null;
    const head = el("div", { class: "about" }, logo,
      el("div", { class: "lines" }, el("strong", null, `Clak ${data.version}`),
        line ? el("div", { class: "quiet" + (error ? " danger" : "") }, line) : null),
      el("div", { class: "actions" },
        u.checking ? el("span", { class: "spinner", "aria-label": "Checking" }) : null,
        el("button", { onclick: checkNow, disabled: u.checking }, "Check now")));
    let status = null;
    if (progress) status = installing();
    else if (u.newer) {
      status = Kit.bar("info", `Clak ${u.newer.version} is available`, el("div", { class: "actions" },
        el("button", { onclick: download }, "Download"),
        el("button", { class: "accent", onclick: install }, "Install and restart")));
    } else if (error) status = Kit.bar("warn", error);
    return el("div", { class: "card about-card" }, head, status);
  };

  const updates = () =>
    el("div", { class: "card group settings" },
      setting("refresh", "#5856d6", "Check for updates automatically", "Every hour",
        Kit.toggle(data.update_check, "Check for updates automatically", setPreference("update_check"))),
      data.update_check
        ? setting("refresh", "#34c759", "Install updates automatically", "When no agent is connected",
          Kit.toggle(data.auto_update, "Install updates automatically", setPreference("auto_update")))
        : null);

  const PARTS = [["general", "General"], ["integration", "Integration"], ["about", "About"]];

  const choose = (id) => {
    part = PARTS.some(([p]) => p === id) ? id : "general";
    for (const [id, button] of Object.entries(pivot)) {
      button.classList.toggle("selected", id === part);
      button.setAttribute("aria-selected", String(id === part));
    }
    for (const [id, node] of Object.entries(parts)) node.classList.toggle("hidden", id !== part);
    if (part === "integration") Setup.refresh();
  };

  const draw = () => {
    if (!page || !data) return;
    Kit.replace(parts.general,
      Kit.section("Startup"), startup(),
      Kit.section("Alerts"), alerts(),
      Kit.section("Hub"), hub());
    Kit.replace(parts.about, about(), updates());
  };

  Panel.tab({
    id: "settings",
    title: "Settings",
    icon: Kit.navIcon("settings", Kit.TINTS.settings),
    order: 70,
    mount(section) {
      page = section;
      pivot = Object.fromEntries(PARTS.map(([id, title]) =>
        [id, el("button", { role: "tab", onclick: () => choose(id) }, title)]));
      parts.general = el("div", { role: "tabpanel" });
      parts.integration = el("div", { role: "tabpanel" }, Setup.mount());
      parts.about = el("div", { role: "tabpanel" });
      page.append(Kit.title("Settings"), el("div", { class: "pivot", role: "tablist" }, Object.values(pivot)),
        parts.general, parts.integration, parts.about);
      choose(part);
      window.addEventListener("focus", () => {
        if (page.classList.contains("hidden")) return;
        refresh();
        if (part === "integration") Setup.refresh();
      });
      Clak.listen("update", (update) => {
        if (!data) return;
        data.update = update;
        draw();
      });
      Clak.listen("update-progress", (p) => {
        progress = p;
        draw();
      });
      refresh();
    },
    shown() {
      refresh();
      if (part === "integration") Setup.refresh();
    },
    /// "settings:about", "settings:integration": a part of the tab.
    route(where) {
      choose(where);
    },
    render(s) {
      state = s;
      draw();
    },
  });
})();
