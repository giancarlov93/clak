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
import { execFileSync } from "node:child_process";
import { copyFileSync, mkdirSync, existsSync } from "node:fs";
import { join, dirname } from "node:path";
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
run("cargo", ["tauri", "build", "--config", "tauri.bundle.json", "--bundles", bundles, ...targetArgs], app);
console.log(`\ninstallers in ${join(release, "bundle")}`);
