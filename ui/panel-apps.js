// The Applications tab: the programs the user allowed once and that the hub
// remembers, by executable. "Forget" makes them ask again on the next connection.
"use strict";

(() => {
  const el = Clak.el;
  const D = Describe;
  let page = null;
  let list = null;
  let state = Panel.state();

  /// Starts the application; if it can't, the button says so for a moment, with the reason on hover.
  const launch = (app, button) =>
    Clak.invoke("app_launch", { key: app.key }).catch((e) => {
      button.textContent = "Couldn't open";
      button.title = String(e);
      setTimeout(() => { button.textContent = "Open"; button.title = `Starts ${app.name}`; }, 3000);
    });

  const row = (app, now) => {
    const signed = !!app.signer;
    // A program that runs in Java or Python: the signature is the runtime's.
    const runtime = Clak.runtime(app.exe);
    return el("div", { class: Clak.mac ? "row allowed" : "card allowed", "data-id": app.key },
      Kit.tile("apps", 36, "#007aff", app.exe),
      el("div", { class: "lines" },
        el("div", { class: "name" }, el("strong", null, app.name),
          runtime ? Kit.tag(runtime, { help: `Runs in ${runtime}` }) : null,
          signed ? el("span", { title: runtime ? `${runtime} signed by` : "Signed by" }, Kit.badge(app.signer, "ok")) : Kit.badge("unsigned", "warn")),
        el("div", { class: "mono quiet ellipsis selectable", title: app.exe }, app.exe),
        el("div", { class: "quiet" }, signed ? `Allowed ${D.ago(app.since, now)}` : `This version of the file only · allowed ${D.ago(app.since, now)}`)),
      el("div", { class: "actions" },
        el("button", { title: `Starts ${app.name}`, onclick: (e) => launch(app, e.currentTarget) }, "Open"),
        el("button", { title: "It will ask again the next time it connects", onclick: () => Panel.send({ type: "forget_app", key: app.key }) }, "Forget")));
  };

  const draw = () => {
    if (!page) return;
    const now = Date.now();
    const apps = [...(state.apps || [])].sort((a, b) => a.since - b.since);
    const rows = apps.map((a) => row(a, now));
    Kit.replace(list,
      Kit.section("Allowed", { count: apps.length, help: "Remembered by executable: same path and same developer or, if unsigned, same content" }),
      apps.length === 0 ? Kit.empty("apps", "None allowed") : Clak.mac ? el("div", { class: "card group rows" }, rows) : rows);
  };

  Panel.tab({
    id: "apps",
    title: "Applications",
    icon: Kit.navIcon("apps", Kit.TINTS.apps),
    order: 30,
    count: (s) => (s.apps || []).length,
    mount(section) {
      page = section;
      list = el("div");
      page.append(Kit.title("Applications"), list);
      setInterval(() => !page.classList.contains("hidden") && draw(), 30000);
    },
    render(s) {
      state = s;
      draw();
    },
  });
})();
