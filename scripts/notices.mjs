// The dependency notices: their licenses require them to travel with the binaries.
//
//     node scripts/notices.mjs [--target <triple>] [--out <file>]
//
// From `cargo metadata`: the crates that actually end up in ClakApp, clak and clak-shim on
// that target (neither dev ones nor build-only ones), each with its license and the license
// files it carries. Each text is written only once, with the list of crates that carry it.
import { execFileSync } from "node:child_process";
import { readdirSync, readFileSync, writeFileSync } from "node:fs";
import { createHash } from "node:crypto";
import { join, dirname } from "node:path";
import { fileURLToPath } from "node:url";

const root = join(dirname(fileURLToPath(import.meta.url)), "..");
const option = (name) => {
  const at = process.argv.indexOf(name);
  return at > 0 ? process.argv[at + 1] : null;
};
const triple =
  option("--target") ?? execFileSync("rustc", ["-vV"], { encoding: "utf8" }).match(/^host: (.+)$/m)[1].trim();

const metadata = JSON.parse(
  execFileSync("cargo", ["metadata", "--format-version", "1", "--filter-platform", triple], {
    cwd: root,
    encoding: "utf8",
    maxBuffer: 256 * 1024 * 1024,
  }),
);
const packages = new Map(metadata.packages.map((p) => [p.id, p]));
const nodes = new Map(metadata.resolve.nodes.map((n) => [n.id, n]));
const procMacro = (p) => p.targets.some((t) => t.kind.includes("proc-macro"));

// From the crates that ship, along normal dependencies only.
const shipped = new Set();
const queue = metadata.packages.filter((p) => ["clak-app", "clak-agent"].includes(p.name)).map((p) => p.id);
while (queue.length) {
  const id = queue.pop();
  if (shipped.has(id)) continue;
  shipped.add(id);
  for (const dep of nodes.get(id)?.deps ?? []) {
    if (dep.dep_kinds.some((k) => k.kind === null) && !procMacro(packages.get(dep.pkg))) queue.push(dep.pkg);
  }
}

const LICENSE_FILE = /^(licen[cs]e|copying|copyright|notice|unlicense)([-._].*)?$/i;
const groups = new Map(); // text fingerprint → { text, crates }
const bare = []; // crates without a license file: the expression remains
for (const id of shipped) {
  const p = packages.get(id);
  if (p.source === null) continue; // Clak's own code: covered by LICENSE
  const dir = dirname(p.manifest_path);
  const files = readdirSync(dir)
    .filter((f) => LICENSE_FILE.test(f))
    .sort();
  if (p.license_file && !files.includes(p.license_file)) files.push(p.license_file);
  const label = `${p.name} ${p.version} (${p.license ?? "see its license file"})${p.repository ? ` ${p.repository}` : ""}`;
  if (!files.length) {
    bare.push(label);
    continue;
  }
  for (const file of files) {
    const text = readFileSync(join(dir, file), "utf8").replace(/\r\n/g, "\n").trim();
    const key = createHash("sha256").update(text).digest("hex");
    if (!groups.has(key)) groups.set(key, { text, crates: [] });
    groups.get(key).crates.push(label);
  }
}

const rule = "=".repeat(80);
const out = [
  "Clak includes the following third-party software. Clak itself is under the MIT License",
  "(the LICENSE file next to this one). Each section below lists components and the license",
  "texts they are distributed with.",
  "",
];
for (const group of groups.values()) group.crates.sort();
const sorted = [...groups.values()].sort((a, b) => a.crates[0].localeCompare(b.crates[0]));
for (const group of sorted) {
  out.push(rule, ...group.crates, rule, "", group.text, "", "");
}
if (bare.length) {
  out.push(rule, "Distributed under the license named next to each, without a separate license file:", rule, "");
  out.push(...bare.sort(), "");
}

const text = out.join("\n");
const target = option("--out");
if (target) {
  writeFileSync(target, text);
  console.log(`${shipped.size - [...shipped].filter((id) => packages.get(id).source === null).length} crates -> ${target}`);
} else {
  process.stdout.write(text);
}
