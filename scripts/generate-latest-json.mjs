#!/usr/bin/env node
/**
 * Genera latest.json per Tauri updater (GitHub Releases).
 *
 * Esempio:
 *   node scripts/generate-latest-json.mjs \
 *     --version 0.2.0 \
 *     --notes "Correzioni e miglioramenti" \
 *     --base-url https://github.com/sebastianoboem/AndroidAdwareCleaner/releases/download/v0.2.0 \
 *     --darwin-aarch64 target/release/bundle/macos/AndroidAdwareCleaner.app.tar.gz.sig \
 *     --darwin-x86_64 target/x86_64-apple-darwin/release/bundle/macos/AndroidAdwareCleaner.app.tar.gz.sig \
 *     --windows-x86_64 target/x86_64-pc-windows-msvc/release/bundle/nsis/AndroidAdwareCleaner_0.2.0_x64-setup.exe.sig
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

const platforms = {};
for (const [platform, sigPath] of Object.entries(opts.platforms)) {
  const artifact = artifactNames[platform];
  if (!artifact) {
    console.error(`Piattaforma sconosciuta: ${platform}`);
    process.exit(1);
  }
  const signature = readFileSync(sigPath, "utf8").trim();
  platforms[platform] = {
    signature,
    url: `${opts.baseUrl}/${artifact}`,
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
