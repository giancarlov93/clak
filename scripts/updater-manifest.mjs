// The updater's manifest, latest.json, for a release, from the files in a folder:
//
//     node scripts/updater-manifest.mjs <folder> <version> <owner/repo> [notes file]
//
// Each file with its .sig next to it becomes a platform: the Windows installer and the Mac
// app archives. Without signed files it writes nothing, and the release goes out without it.
import { readdirSync, readFileSync, writeFileSync, existsSync } from "node:fs";
import { join } from "node:path";

const [folder, version, repo, notesFile] = process.argv.slice(2);
if (!folder || !version || !repo) {
  console.error("usage: updater-manifest.mjs <folder> <version> <owner/repo> [notes file]");
  process.exit(2);
}

const PLATFORMS = [
  [/_x64-setup\.exe$/, "windows-x86_64"],
  [/_aarch64\.app\.tar\.gz$/, "darwin-aarch64"],
  [/_x64\.app\.tar\.gz$/, "darwin-x86_64"],
];

const platforms = {};
for (const name of readdirSync(folder)) {
  const platform = PLATFORMS.find(([pattern]) => pattern.test(name))?.[1];
  const sig = join(folder, `${name}.sig`);
  if (!platform || !existsSync(sig)) continue;
  platforms[platform] = {
    signature: readFileSync(sig, "utf8").trim(),
    url: `https://github.com/${repo}/releases/download/v${version}/${encodeURIComponent(name)}`,
  };
}

if (Object.keys(platforms).length === 0) {
  console.log("no signed files: no latest.json");
  process.exit(0);
}
const manifest = {
  version,
  notes: notesFile && existsSync(notesFile) ? readFileSync(notesFile, "utf8").trim() : "",
  pub_date: new Date().toISOString(),
  platforms,
};
writeFileSync(join(folder, "latest.json"), JSON.stringify(manifest, null, 2) + "\n");
console.log(`latest.json: ${Object.keys(platforms).join(", ")}`);
