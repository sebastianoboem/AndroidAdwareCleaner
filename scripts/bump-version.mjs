#!/usr/bin/env node
import { readFileSync, writeFileSync } from "node:fs";
import { fileURLToPath } from "node:url";
import { dirname, join } from "node:path";

const version = process.argv[2];
if (!version || !/^\d+\.\d+\.\d+(-[\w.]+)?$/.test(version)) {
  console.error("Usage: node scripts/bump-version.mjs <semver>  (es. 0.2.0)");
  process.exit(1);
}

const root = join(dirname(fileURLToPath(import.meta.url)), "..");

function patchJson(path, fn) {
  const data = JSON.parse(readFileSync(join(root, path), "utf8"));
  fn(data);
  writeFileSync(join(root, path), `${JSON.stringify(data, null, 2)}\n`);
}

function patchToml(path, pattern, replacement) {
  const full = join(root, path);
  const text = readFileSync(full, "utf8");
  if (!pattern.test(text)) {
    throw new Error(`pattern not found in ${path}`);
  }
  writeFileSync(full, text.replace(pattern, replacement));
}

patchJson("package.json", (d) => {
  d.version = version;
});
patchJson("src-tauri/tauri.conf.json", (d) => {
  d.version = version;
});
patchToml("Cargo.toml", /^version = ".*"$/m, `version = "${version}"`);
patchToml("src-tauri/Cargo.toml", /^version = ".*"$/m, `version = "${version}"`);

console.log(`Version bumped to ${version}`);
