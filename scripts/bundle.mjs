// The installers: per-user NSIS on Windows, .app and .dmg on the Mac.
//
//     node scripts/bundle.mjs
//     node scripts/bundle.mjs --target x86_64-apple-darwin
//
// Builds `clak` and `clak-shim`, puts them next to the app as Tauri external binaries (with
// the target name, as the bundler wants), then runs `cargo tauri build`. The build folder
// is CARGO_TARGET_DIR, by default outside the repo, in the user's cache: only the two copies
// in crates/clak-app/binaries stay in the repo (ignored by git). With `--target` it builds
// for another system (on an ARM Mac, the Intel Mac).
//
// On the Mac, signing and notarization: with APPLE_SIGNING_IDENTITY the app is signed with
// that certificate instead of ad hoc; with APPLE_API_KEY, APPLE_API_ISSUER and
// APPLE_API_KEY_PATH Tauri also notarizes it. In CI they come from the secrets
// (release.yml); here, if they aren't in the environment, from .secrets/apple.env
// (KEY=value lines, ignored by git).
//
// The updater files (the installer's .sig, on the Mac Clak.app.tar.gz and its .sig) are made
// only with the signing key: TAURI_SIGNING_PRIVATE_KEY and its password, in CI from the
// secrets, here from .secrets/updater.key and .secrets/updater.password.
import { execFileSync } from "node:child_process";
import { copyFileSync, mkdirSync, existsSync, readFileSync } from "node:fs";
import { join, dirname, isAbsolute } from "node:path";
import { fileURLToPath } from "node:url";
import { homedir } from "node:os";

const root = join(dirname(fileURLToPath(import.meta.url)), "..");
const app = join(root, "crates", "clak-app");
const windows = process.platform === "win32";
const exe = windows ? ".exe" : "";

if (!process.env.CARGO_TARGET_DIR) {
  const base = windows ? process.env.LOCALAPPDATA : join(homedir(), "Library", "Caches");
  process.env.CARGO_TARGET_DIR = join(base, "cargo-target", "clak");
}
const target = process.env.CARGO_TARGET_DIR;

// A secret missing in CI (a fork, a repository without them) arrives empty: as if absent.
for (const name of ["TAURI_SIGNING_PRIVATE_KEY", "TAURI_SIGNING_PRIVATE_KEY_PASSWORD", "APPLE_CERTIFICATE", "APPLE_CERTIFICATE_PASSWORD", "APPLE_SIGNING_IDENTITY", "APPLE_API_KEY", "APPLE_API_ISSUER", "APPLE_API_KEY_PATH"]) {
  if (process.env[name] === "") delete process.env[name];
}
const secrets = join(root, ".secrets", "apple.env");
if (process.platform === "darwin" && existsSync(secrets)) {
  for (const line of readFileSync(secrets, "utf8").split(/\r?\n/)) {
    const m = line.match(/^\s*([A-Z_][A-Z0-9_]*)\s*=\s*(.*?)\s*$/);
    if (!m || process.env[m[1]] !== undefined) continue; // the environment wins
    process.env[m[1]] = m[1].endsWith("_PATH") && !isAbsolute(m[2]) ? join(root, m[2]) : m[2];
  }
}
const updaterKey = join(root, ".secrets", "updater.key");
if (process.env.TAURI_SIGNING_PRIVATE_KEY === undefined && existsSync(updaterKey)) {
  process.env.TAURI_SIGNING_PRIVATE_KEY = readFileSync(updaterKey, "utf8").trim();
  const password = join(root, ".secrets", "updater.password");
  if (existsSync(password)) process.env.TAURI_SIGNING_PRIVATE_KEY_PASSWORD = readFileSync(password, "utf8").trim();
}
const updater = !!process.env.TAURI_SIGNING_PRIVATE_KEY;
console.log(`updater files: ${updater ? "yes" : "no (no signing key)"}`);
if (process.platform === "darwin") {
  const identity = process.env.APPLE_SIGNING_IDENTITY;
  const notarize = process.env.APPLE_API_KEY && process.env.APPLE_API_ISSUER && process.env.APPLE_API_KEY_PATH;
  console.log(`signing: ${identity || "ad hoc"}; notarization: ${notarize ? "yes" : "no"}`);
}

const run = (cmd, args, cwd = root) => execFileSync(cmd, args, { cwd, stdio: "inherit", env: process.env });
const at = process.argv.indexOf("--target");
const cross = at > 0 ? process.argv[at + 1] : null;
const triple = cross ?? execFileSync("rustc", ["-vV"], { encoding: "utf8" }).match(/^host: (.+)$/m)[1].trim();
const targetArgs = cross ? ["--target", cross] : [];
const release = cross ? join(target, cross, "release") : join(target, "release");

run("cargo", ["build", "--release", "-p", "clak-agent", ...targetArgs]);
mkdirSync(join(app, "binaries"), { recursive: true });
// With the target name appended, as the bundler looks for it.
for (const name of ["clak", "clak-shim"]) {
  const from = join(release, name + exe);
  if (!existsSync(from)) throw new Error(`missing ${from}`);
  copyFileSync(from, join(app, "binaries", `${name}-${triple}${exe}`));
}

// The licenses travel with the app: Clak's own and the notices of this target's dependencies.
run(process.execPath, [join(root, "scripts", "notices.mjs"), "--target", triple, "--out", join(app, "THIRD-PARTY-NOTICES.txt")]);

const bundles = windows ? "nsis" : process.platform === "darwin" ? "app,dmg" : "deb";
const updaterArgs = updater ? ["--config", JSON.stringify({ bundle: { createUpdaterArtifacts: true } })] : [];
run("cargo", ["tauri", "build", "--config", "tauri.bundle.json", ...updaterArgs, "--bundles", bundles, ...targetArgs], app);
console.log(`\ninstallers in ${join(release, "bundle")}`);
