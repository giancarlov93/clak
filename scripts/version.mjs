// Clak's version lives in one place only, `[workspace.package]` in the root Cargo.toml:
// the crates inherit it and Tauri takes it from the app crate.
//
//     node scripts/version.mjs           prints it
//     node scripts/version.mjs 0.3.12    changes it
import { readFileSync, writeFileSync } from "node:fs";
import { join, dirname } from "node:path";
import { fileURLToPath } from "node:url";

const file = join(dirname(fileURLToPath(import.meta.url)), "..", "Cargo.toml");
const text = readFileSync(file, "utf8");
const pattern = /(\[workspace\.package\][^[]*?\nversion = ")([^"]+)(")/;
const found = text.match(pattern);
if (!found) throw new Error(`no [workspace.package] version in ${file}`);

const wanted = process.argv[2];
if (!wanted) {
  console.log(found[2]);
} else {
  if (!/^\d+\.\d+\.\d+$/.test(wanted)) throw new Error(`not a version: ${wanted}`);
  writeFileSync(file, text.replace(pattern, `$1${wanted}$3`));
  console.log(`${found[2]} -> ${wanted}`);
}
