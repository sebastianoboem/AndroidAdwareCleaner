#!/usr/bin/env node
/**
 * Genera latest.json per Tauri updater.
 *
 * Pipeline: test locale → se OK → GitHub Release (vedi docs/RELEASE.md).
 *
 * Esempio (produzione — GitHub):
 *   node scripts/generate-latest-json.mjs \
 *     --version 0.2.1 \
 *     --notes "Correzioni e miglioramenti" \
 *     --base-url https://github.com/sebastianoboem/AndroidAdwareCleaner/releases/download/v0.2.1 \
 *     --darwin-aarch64 path/to/AndroidAdwareCleaner_0.2.1_aarch64.app.tar.gz.sig \
 *     --darwin-x86_64 path/to/AndroidAdwareCleaner_0.2.1_x64.app.tar.gz.sig \
 *     --windows-x86_64 path/to/AndroidAdwareCleaner_0.2.1_x64-setup.exe.sig
 *
 * Esempio (test locale):
 *   ... --base-url http://127.0.0.1:8765
 */
import { readFileSync, writeFileSync } from "node:fs";

const args = process.argv.slice(2);
const opts = {
  version: "",
  notes: "",
  baseUrl: "",
  platforms: {},
};

for (let i = 0; i < args.length; i++) {
  const arg = args[i];
  if (arg === "--version") opts.version = args[++i];
  else if (arg === "--notes") opts.notes = args[++i];
  else if (arg === "--base-url") opts.baseUrl = args[++i].replace(/\/$/, "");
  else if (arg.startsWith("--")) {
    const key = arg.slice(2);
    opts.platforms[key] = args[++i];
  }
}

if (!opts.version || !opts.baseUrl) {
  console.error("Richiesti --version e --base-url");
  process.exit(1);
}

const artifactNames = {
  "darwin-aarch64": `AndroidAdwareCleaner_${opts.version}_aarch64.app.tar.gz`,
  "darwin-x86_64": `AndroidAdwareCleaner_${opts.version}_x64.app.tar.gz`,
  "windows-x86_64": `AndroidAdwareCleaner_${opts.version}_x64-setup.exe`,
};

const isSourceForge = opts.baseUrl.includes("sourceforge.net");

const platforms = {};
for (const [platform, sigPath] of Object.entries(opts.platforms)) {
  const artifact = artifactNames[platform];
  if (!artifact) {
    console.error(`Piattaforma sconosciuta: ${platform}`);
    process.exit(1);
  }
  const signature = readFileSync(sigPath, "utf8").trim();
  const url = isSourceForge
    ? `${opts.baseUrl}/${artifact}/download`
    : `${opts.baseUrl}/${artifact}`;
  platforms[platform] = {
    signature,
    url,
  };
}

const manifest = {
  version: opts.version,
  notes: opts.notes || `AndroidAdwareCleaner ${opts.version}`,
  pub_date: new Date().toISOString(),
  platforms,
};

const out = "latest.json";
writeFileSync(out, `${JSON.stringify(manifest, null, 2)}\n`);
console.log(`Scritto ${out}`);
