// A pretend backend for the app's pages, with example data: what the README's screenshots
// show. shoot.mjs injects it into every page before the page's own scripts run, as
// `window.__TAURI__`; nothing here comes from a real hub.
//
// `window.__DEMO_ICONS__` (set by shoot.mjs, when the programs are installed): the real icons
// of the example programs, by executable path, as data: URLs.
"use strict";

(() => {
  const now = Date.now() / 1000;
  const ago = (minutes) => now - minutes * 60;
  const icons = () => window.__DEMO_ICONS__ || {};

  const BLENDER = "/Applications/Blender.app/Contents/MacOS/Blender";
  const RHINO = "/Applications/Rhino 8.app/Contents/MacOS/Rhinoceros";
  const QGIS = "/Applications/QGIS.app/Contents/MacOS/QGIS";
  const CLAUDE = "/Users/demo/.local/bin/claude";
  const CODEX = "/opt/homebrew/bin/codex";

  const program = (id, app, title, name, origin, tools, since) => ({
    id, name, base: name, suffix: "", resource: { app, title, instance_id: id, publisher: null },
    origin, key: null, status: null, since: ago(since), tools, cache: null, health: null,
  });
  const signedExe = (exe, signer, pid) => ({ exe, signed: "valid", signer, pid });

  const state = {
    type: "state",
    broker: { pid: 4127, since: ago(185), ws_port: 7779, control: "", can_ask: true },
    sessions: [
      { id: "s1", label: "Claude Code · harbor-pavilion", name: "Claude Code", exe: CLAUDE, pid: 8812,
        signed: "valid", signer: "Anthropic PBC", cwd: "~/Projects/harbor-pavilion", via: "cli", since: ago(42) },
      { id: "s2", label: "Codex · site-survey", name: "Codex", exe: CODEX, pid: 9034,
        signed: "valid", signer: "OpenAI", cwd: "~/Projects/site-survey", via: "cli", since: ago(17) },
    ],
    resources: [
      program("r1", "Blender", "pavilion.blend", "blender", signedExe(BLENDER, "Stichting Blender Foundation", 5120),
        ["scene_info", "list_objects", "set_material", "render_view", "export_gltf"], 120),
      program("r2", "Rhino 8", "pavilion-structure.3dm", "rhino", signedExe(RHINO, "Robert McNeel & Associates", 5304),
        ["document_info", "list_layers", "select_objects", "boolean_union", "bake_grasshopper", "export_step"], 96),
      program("r3", "QGIS", "site-survey.qgz", "qgis", signedExe(QGIS, "QGIS.org", 6011),
        ["list_layers", "add_layer", "reproject", "run_processing", "export_map"], 30),
      program("r4", "GitHub", "api.githubcopilot.com", "github",
        { kind: "connector", protocol: "mcp", transport: "http", url: "https://api.githubcopilot.com/mcp/", secure: true, loopback: false },
        ["search_issues", "create_issue", "get_pull_request", "create_pull_request", "list_commits"], 185),
      program("r5", "Open-Meteo", "api.open-meteo.com", "weather",
        { kind: "connector", protocol: "rest", url: "https://api.open-meteo.com/v1", secure: true, loopback: false },
        ["forecast", "historical"], 185),
      program("r0", "Clak", "Settings", "settings", { kind: "hub" }, ["get", "set"], 185),
    ],
    grants: [
      { id: "g1", resource_id: "r1", session_id: "s1", since: ago(40), tools: ["render_view"], calls: 23, errors: 0, finished: false, last_call: ago(1) },
      { id: "g2", resource_id: "r2", session_id: "s1", since: ago(31), tools: [], calls: 8, errors: 0, finished: false, last_call: ago(6) },
      { id: "g3", resource_id: "r4", session_id: "s1", since: ago(12), tools: [], calls: 3, errors: 0, finished: false, last_call: ago(9) },
      { id: "g4", resource_id: "r3", session_id: "s2", since: ago(15), tools: ["reproject"], calls: 11, errors: 1, finished: false, last_call: ago(2) },
      { id: "g5", resource_id: "r5", session_id: "s2", since: ago(14), tools: [], calls: 2, errors: 0, finished: false, last_call: ago(13) },
    ],
    blocks: [],
    pending: [],
    apps: [
      { key: "a1", exe: BLENDER, name: "Blender", signer: "Stichting Blender Foundation", since: ago(60 * 24 * 9), label: null, command: null },
      { key: "a2", exe: RHINO, name: "Rhino 8", signer: "Robert McNeel & Associates", since: ago(60 * 24 * 9), label: null, command: null },
      { key: "a3", exe: QGIS, name: "QGIS", signer: "QGIS.org", since: ago(60 * 24 * 3), label: null, command: null },
    ],
    connectors: [
      { id: "c1", name: "GitHub", url: "https://api.githubcopilot.com/mcp/", transport: "http", auth: "oauth", enabled: true, state: "active", resource_id: "r4", cli: "github" },
      { id: "c2", name: "Linear", url: "https://mcp.linear.app/mcp", transport: "http", auth: "oauth", enabled: true, state: "active", resource_id: null, cli: "linear" },
    ],
    apis: [
      { id: "p1", name: "Open-Meteo", url: "https://api.open-meteo.com/v1", auth: "none", enabled: true, state: "active", endpoints: 2, resource_id: "r5", cli: "weather" },
    ],
    commands: [],
    commands_error: null,
  };

  // The question of the consent window: an agent wants to run a command it has no yes for.
  const question = {
    id: "q1",
    kind: "call",
    title: "Claude Code wants to run “export_gltf” in Blender",
    question: "Allow Claude Code to run “export_gltf” in Blender — pavilion.blend?",
    who: [
      { label: "Agent", value: "Claude Code" },
      { label: "Executable", value: CLAUDE },
      { label: "Signature", value: "signed by Anthropic PBC" },
      { label: "Process", value: "pid 8812" },
      { label: "Folder", value: "~/Projects/harbor-pavilion (declared)" },
    ],
    what: [
      { label: "Program", value: "Blender — pavilion.blend" },
      { label: "Executable", value: `${BLENDER}, signed by Stichting Blender Foundation` },
    ],
    tools: [{ name: "export_gltf", description: "Exports the scene, or the selected objects, as a glTF 2.0 file.", note: "new", changes: [], parameters: [] }],
    warning: "A command you haven't approved yet.",
    note: "A yes for the session holds while the agent's process stays open. An approved command runs, as it is, for every agent you let use the program; if it changes, it asks again.",
    arguments: [
      { label: "path", value: "~/Projects/harbor-pavilion/out/pavilion.glb" },
      { label: "selection_only", value: "false" },
    ],
    choices: {
      scopes: [
        { value: "once", label: "Only this call" },
        { value: "session", label: "“export_gltf” for this agent's session" },
        { value: "approve", label: "Approve “export_gltf”, as it is, for Blender" },
      ],
      always: null,
      block: "Block Blender for Claude Code",
    },
    form: null,
    page: null,
    queue: { id: "q1", position: 1, total: 1 },
    expires: now + 106,
  };

  // The question's lines carry more than the path ("…/Blender, signed by …"): the icon is
  // the one of the executable the line starts with.
  const iconOf = (path) => {
    const known = icons();
    const exe = Object.keys(known).find((p) => String(path).startsWith(p));
    return exe ? known[exe] : null;
  };

  const replies = {
    hub_hello: () => ({ mode: "local", home: "~/.clak", state, log: [] }),
    consent_request: () => question,
    // The window grows to what the question says: frame.html sizes it.
    consent_resize: ({ height }) => parent.postMessage({ clakConsentHeight: height }, "*"),
    app_icon: ({ path }) => iconOf(path),
    server_icon: () => null,
    prefs_set: () => null,
  };

  const invoke = (cmd, args) => {
    const reply = replies[cmd];
    return Promise.resolve(reply ? reply(args || {}) : null);
  };

  // The events never come: the pages keep what they drew first.
  const listen = () => Promise.resolve(() => {});
  const windowApi = { listen, setSize: () => Promise.resolve(), show: () => Promise.resolve() };

  // ?scroll=<section title>: the page opens scrolled to that section, for a picture of it.
  const scroll = new URLSearchParams(location.search).get("scroll");
  if (scroll) {
    window.addEventListener("load", () => setTimeout(() => {
      const heading = [...document.querySelectorAll(".section h2")].find((h) => h.textContent === scroll);
      heading?.parentElement.scrollIntoView({ block: "start" });
    }, 600));
  }

  window.__TAURI__ = {
    core: { invoke },
    event: { listen },
    webviewWindow: { getCurrentWebviewWindow: () => windowApi },
    window: { getCurrentWindow: () => windowApi },
  };
})();
