// The README's screenshots: the app's real pages (ui/) with the example data of demo.js,
// photographed in Chrome without a window, in the light and the dark appearance.
//
//   node docs/screenshots/shoot.mjs
//
// Writes docs/images/<shot>-<light|dark>.png. Needs Google Chrome (CHROME for another path)
// and Node 22 or later. On the Mac the example programs' real icons are used when they are
// installed; elsewhere their tiles show the glyph.

import { spawn, execFileSync } from "node:child_process";
import { existsSync, mkdtempSync, mkdirSync, readFileSync, rmSync, writeFileSync } from "node:fs";
import { tmpdir } from "node:os";
import { dirname, join } from "node:path";
import { fileURLToPath, pathToFileURL } from "node:url";

const here = dirname(fileURLToPath(import.meta.url));
const out = join(here, "..", "images");
const SHOTS = ["hero", "programs", "consent"];
const SCHEMES = ["light", "dark"];
const SCALE = 2;

const CHROME = process.env.CHROME || [
  "/Applications/Google Chrome.app/Contents/MacOS/Google Chrome",
  "C:\\Program Files\\Google\\Chrome\\Application\\chrome.exe",
  "/usr/bin/google-chrome",
  "/usr/bin/chromium",
].find(existsSync);
if (!CHROME) throw new Error("Google Chrome not found: set CHROME to its path");

/// The example programs' icons, from their bundles, as data: URLs (Mac only).
const icons = () => {
  if (process.platform !== "darwin") return {};
  const apps = {
    "/Applications/Blender.app/Contents/MacOS/Blender": "/Applications/Blender.app",
    "/Applications/Rhino 8.app/Contents/MacOS/Rhinoceros": "/Applications/Rhino 8.app",
    "/Applications/QGIS.app/Contents/MacOS/QGIS": "/Applications/QGIS.app",
  };
  const scratch = mkdtempSync(join(tmpdir(), "clak-icons-"));
  const found = {};
  for (const [exe, bundle] of Object.entries(apps)) {
    try {
      const plist = join(bundle, "Contents", "Info.plist");
      let name = execFileSync("/usr/libexec/PlistBuddy", ["-c", "Print :CFBundleIconFile", plist], { encoding: "utf8" }).trim();
      if (!name.endsWith(".icns")) name += ".icns";
      const png = join(scratch, `${Object.keys(found).length}.png`);
      execFileSync("sips", ["-s", "format", "png", "-Z", "128", join(bundle, "Contents", "Resources", name), "--out", png], { stdio: "ignore" });
      found[exe] = "data:image/png;base64," + readFileSync(png).toString("base64");
    } catch {
      // Not installed: the tile keeps its glyph.
    }
  }
  rmSync(scratch, { recursive: true, force: true });
  return found;
};

/// Chrome DevTools Protocol over the WebSocket of a page.
const connect = (url) =>
  new Promise((resolve, reject) => {
    const socket = new WebSocket(url);
    let next = 0;
    const waiting = new Map();
    socket.onmessage = (event) => {
      const message = JSON.parse(event.data);
      const pending = waiting.get(message.id);
      if (!pending) return;
      waiting.delete(message.id);
      message.error ? pending.reject(new Error(message.error.message)) : pending.resolve(message.result);
    };
    socket.onerror = reject;
    socket.onopen = () =>
      resolve({
        send: (method, params = {}) =>
          new Promise((ok, fail) => {
            const id = ++next;
            waiting.set(id, { resolve: ok, reject: fail });
            socket.send(JSON.stringify({ id, method, params }));
          }),
        close: () => socket.close(),
      });
  });

const sleep = (ms) => new Promise((resolve) => setTimeout(resolve, ms));

const profile = mkdtempSync(join(tmpdir(), "clak-chrome-"));
const PORT = 9333;
const chrome = spawn(CHROME, [
  "--headless=new", `--remote-debugging-port=${PORT}`, `--user-data-dir=${profile}`,
  "--allow-file-access-from-files", "--hide-scrollbars", "--force-color-profile=srgb",
  "--no-first-run", "--no-default-browser-check", "about:blank",
], { stdio: "ignore" });

try {
  let target;
  for (let i = 0; i < 50 && !target; i++) {
    await sleep(200);
    target = await fetch(`http://127.0.0.1:${PORT}/json/list`).then((r) => r.json()).then((list) => list.find((t) => t.type === "page")).catch(() => null);
  }
  if (!target) throw new Error("Chrome didn't start");
  const page = await connect(target.webSocketDebuggerUrl);
  await page.send("Page.enable");
  await page.send("Runtime.enable");
  const demo = `window.__DEMO_ICONS__ = ${JSON.stringify(icons())};\n` + readFileSync(join(here, "demo.js"), "utf8");
  await page.send("Page.addScriptToEvaluateOnNewDocument", { source: demo });
  await page.send("Emulation.setDefaultBackgroundColorOverride", { color: { r: 0, g: 0, b: 0, a: 0 } });
  // A Mac's user agent, whatever runs this: the pages draw the Mac's look.
  await page.send("Emulation.setUserAgentOverride", { userAgent: "Mozilla/5.0 (Macintosh; Intel Mac OS X 10_15_7) AppleWebKit/605.1.15 (KHTML, like Gecko)" });
  await page.send("Emulation.setEmulatedMedia", { features: [{ name: "prefers-reduced-motion", value: "reduce" }] });
  mkdirSync(out, { recursive: true });

  for (const scheme of SCHEMES) {
    await page.send("Emulation.setEmulatedMedia", {
      features: [{ name: "prefers-color-scheme", value: scheme }, { name: "prefers-reduced-motion", value: "reduce" }],
    });
    for (const shot of SHOTS) {
      await page.send("Emulation.setDeviceMetricsOverride", { width: 1600, height: 1000, deviceScaleFactor: SCALE, mobile: false });
      const url = pathToFileURL(join(here, "frame.html")).href + `?shot=${shot}`;
      await page.send("Page.navigate", { url });
      await sleep(2500); // the pages draw, the icons arrive, the question's buttons light up
      const { result } = await page.send("Runtime.evaluate", { expression: "JSON.stringify(window.__size)", returnByValue: true });
      const size = JSON.parse(result.value);
      await page.send("Emulation.setDeviceMetricsOverride", { width: size.width, height: size.height, deviceScaleFactor: SCALE, mobile: false });
      await sleep(400);
      const { data } = await page.send("Page.captureScreenshot", { format: "png", clip: { x: 0, y: 0, width: size.width, height: size.height, scale: 1 } });
      const file = join(out, `${shot}-${scheme}.png`);
      writeFileSync(file, Buffer.from(data, "base64"));
      console.log(file);
    }
  }
  page.close();
} finally {
  chrome.kill();
  await sleep(300);
  rmSync(profile, { recursive: true, force: true });
}
