// The Settings tab: when the app starts, how it notifies, where the hub lives. No setting
// touches the permissions, which are given only in the consent windows. It is re-read on every
// opening: startup also changes from Task Manager (on the Mac from Login Items).
"use strict";

(() => {
  const el = Clak.el;
  // The app icon (icons/icon.svg), for "About".
  const LOGO = '<svg viewBox="0 0 32 32" aria-hidden="true"><line x1="16" y1="2.5" x2="16" y2="7" stroke="#2F5BEA" stroke-width="2.6" stroke-linecap="round"/><circle cx="16" cy="2.6" r="2.2" fill="#2F5BEA"/><rect x="2" y="6" width="28" height="25" rx="7" fill="#2F5BEA"/><polyline points="8.5,13 13.5,18 8.5,23" fill="none" stroke="#fff" stroke-width="3.2" stroke-linecap="round" stroke-linejoin="round"/><line x1="16.5" y1="23.5" x2="23.5" y2="23.5" stroke="#fff" stroke-width="3.2" stroke-linecap="round"/></svg>';

  let page = null;
  let body = null;
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

  const about = () => {
    const logo = el("span", { class: "logo" });
    logo.innerHTML = LOGO;
    return Clak.mac
      ? el("div", { class: "about" }, logo, el("div", null, el("strong", null, "Clak"), el("div", { class: "quiet" }, `Version ${data.version}`)))
      : el("div", { class: "card about" }, logo, el("div", null, `Clak ${data.version}`));
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

  const updates = () => {
    const u = data.update || {};
    const error = u.checking ? null : checkError || u.error;
    const line = u.checking ? "Checking…"
      : !u.checked ? null
      : u.newer || error ? `Checked ${when(u.checked)}`
      : `Up to date · checked ${when(u.checked)}`;
    const rows = [setting("refresh", "#5856d6", "Check for updates", line, [
      el("button", { onclick: checkNow, disabled: u.checking }, "Check now"),
      Kit.toggle(data.update_check, "Check for updates", setPreference("update_check"))])];
    if (data.update_check) {
      rows.push(setting("refresh", "#34c759", "Install updates automatically", "When no agent is connected",
        Kit.toggle(data.auto_update, "Install updates automatically", setPreference("auto_update"))));
    }
    if (progress) rows.push(installing());
    else if (u.newer) {
      rows.push(Kit.bar("info", `Clak ${u.newer.version} is available`, el("div", { class: "actions" },
        el("button", { onclick: download }, "Download"),
        el("button", { class: "accent", onclick: install }, "Install and restart"))));
    } else if (error) rows.push(Kit.bar("warn", `Couldn't check for updates: ${error}`));
    return el("div", { class: "card group settings" }, rows);
  };

  const draw = () => {
    if (!page || !data) return;
    Kit.replace(body,
      Kit.section("Startup"), startup(),
      Kit.section("Alerts"), alerts(),
      Kit.section("Hub"), hub(),
      Clak.mac ? null : Kit.section("About"), about(), updates());
  };

  Panel.tab({
    id: "settings",
    title: "Settings",
    icon: Kit.navIcon("settings", Kit.TINTS.settings),
    order: 70,
    mount(section) {
      page = section;
      body = el("div");
      page.append(Kit.title("Settings"), body);
      window.addEventListener("focus", () => !page.classList.contains("hidden") && refresh());
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
    },
    render(s) {
      state = s;
      draw();
    },
  });
})();
