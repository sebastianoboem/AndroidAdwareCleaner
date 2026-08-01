#!/usr/bin/env node
/**
 * Carica latest.json sul path stabile SourceForge (sovrascrive).
 *
 * Remoto:
 *   sebastianoboem@frs.sourceforge.net:/home/frs/project/androidadwarecleaner/releases/latest.json
 *
 * Endpoint pubblico (updater):
 *   https://sourceforge.net/projects/androidadwarecleaner/files/releases/latest.json/download
 *
 * Esempio:
 *   node scripts/publish-sourceforge-latest.mjs
 *   node scripts/publish-sourceforge-latest.mjs ./latest.json
 *
 * Richiede SSH verso frs.sourceforge.net (chiave già configurata).
 */
import { spawnSync } from "node:child_process";
import { existsSync } from "node:fs";
import { resolve } from "node:path";

const local = resolve(process.argv[2] || "latest.json");
const remoteHost = "sebastianoboem@frs.sourceforge.net";
const remotePath =
  "/home/frs/project/androidadwarecleaner/releases/latest.json";
const remote = `${remoteHost}:${remotePath}`;

if (!existsSync(local)) {
  console.error(`File non trovato: ${local}`);
  process.exit(1);
}

function run(cmd, args) {
  const result = spawnSync(cmd, args, { stdio: "inherit" });
  if (result.error) {
    return { ok: false, missing: result.error.code === "ENOENT" };
  }
  return { ok: result.status === 0, missing: false, status: result.status };
}

console.log(`Upload ${local} → ${remote}`);

const rsync = run("rsync", ["-avz", "-e", "ssh", local, remote]);
if (rsync.ok) {
  console.log("OK (rsync)");
  process.exit(0);
}

if (!rsync.missing) {
  console.error("rsync fallito, provo scp…");
}

const scp = run("scp", [local, remote]);
if (scp.ok) {
  console.log("OK (scp)");
  process.exit(0);
}

console.error(
  "Upload fallito. Verifica SSH verso frs.sourceforge.net e i path.",
);
process.exit(scp.status ?? 1);
