// The Agents tab: the agents to launch, their instructions and the presets (panel-launch.js).
// How the agents reach Clak (the command line, the skills, the MCP clients) is in Settings ›
// Integration (panel-setup.js). It is re-read on every opening and when the window comes back
// to the foreground: an agent installed meanwhile shows up.
"use strict";

(() => {
  let page = null;

  Panel.tab({
    id: "agents",
    title: "Agents",
    icon: Kit.navIcon("agents", Kit.TINTS.agents),
    order: 20,
    mount(section) {
      page = section;
      // The title is drawn by Launch: on an agent's conversations it becomes the way back.
      page.append(...Launch.mount());
      window.addEventListener("focus", () => !page.classList.contains("hidden") && Launch.refresh());
      Launch.refresh();
    },
    shown() {
      Launch.refresh();
    },
    /// The presets' programs: connected or not.
    render() {
      Launch.draw();
    },
  });
})();
