# Release e aggiornamenti automatici

L'app usa il plugin **Tauri Updater** con un solo endpoint:

**GitHub Releases** — `latest.json` su `releases/latest/download/`

```text
Build firmata → test updater locale (http://127.0.0.1)
     │
     │  se OK
     ▼
GitHub Release  (asset + latest.json)
     ▲
App updater ── check ──► GitHub latest.json
```

Pipeline obbligatoria: **test locale → se OK → publish GitHub**. Non pubblicare `latest.json` di test in produzione.

## Versioning

La versione è definita in:

- `src-tauri/tauri.conf.json` → `version`
- `package.json`
- `Cargo.toml` (workspace + `src-tauri/Cargo.toml`)

Per allineare tutto:

```bash
node scripts/bump-version.mjs 0.2.1
```

Poi commit, tag e push (dopo il test locale OK):

```bash
git tag v0.2.1
git push origin main --tags
```

## Chiavi di firma

- **Pubblica**: in `src-tauri/tauri.conf.json` → `plugins.updater.pubkey`
- **Privata**: `src-tauri/.tauri-signing.key` (mai committare)

Rigenerare solo se persa (gli utenti già installati non potranno aggiornarsi con la nuova chiave):

```bash
CI=true npm run tauri signer generate -- -w src-tauri/.tauri-signing.key -f -p ""
```

## Build release firmate

```bash
export TAURI_SIGNING_PRIVATE_KEY="$(cat src-tauri/.tauri-signing.key)"
export TAURI_SIGNING_PRIVATE_KEY_PASSWORD=""

# macOS arm64
npm run tauri build

# macOS Intel
npm run tauri build -- --target x86_64-apple-darwin

# Windows x64
npm run tauri build -- --runner cargo-xwin --target x86_64-pc-windows-msvc
```

Con `createUpdaterArtifacts: true` vengono creati anche `.sig` e (su Mac) `.app.tar.gz`.

## Pipeline (ordine operativo)

### 1. Build + test updater locale (obbligatorio)

Serve a verificare download, firma e install/relaunch **senza** pubblicare una release.

1. Builda la versione **nuova** (es. `0.2.1`) con endpoint GitHub di produzione (come in `tauri.conf.json`).
2. Copia gli artefatti firmati in `release/staging-local/` (`.app.tar.gz` + `.sig`, e/o setup Windows + `.sig`).
3. Genera un `latest.json` con base URL locale — la versione nel manifest deve essere **maggiore** di quella del client di test:

```bash
npm run release:manifest -- \
  --version 0.2.1 \
  --notes "test locale" \
  --base-url http://127.0.0.1:8765 \
  --darwin-aarch64 release/staging-local/AndroidAdwareCleaner_0.2.1_aarch64.app.tar.gz.sig \
  --darwin-x86_64 release/staging-local/AndroidAdwareCleaner_0.2.1_x64.app.tar.gz.sig \
  --windows-x86_64 release/staging-local/AndroidAdwareCleaner_0.2.1_x64-setup.exe.sig

mv latest.json release/staging-local/
```

4. Servi la cartella (lascia il server acceso durante il test):

```bash
cd release/staging-local && python3 -m http.server 8765
```

5. Builda un client **più vecchio** (es. `0.2.0`) con endpoint temporanei **solo in locale, non committare**:

```json
"endpoints": ["http://127.0.0.1:8765/latest.json"],
"dangerousInsecureTransportProtocol": true
```

`dangerousInsecureTransportProtocol` è obbligatorio: in release Tauri rifiuta `http://` e l'app crasha all'avvio.

Usa un bundle installato/copiato (es. `release/staging-local/AndroidAdwareCleaner-0.2.0-test.app`), non `npm run tauri dev` — l'updater non simula bene install/relaunch in dev.

6. Nell'app di test: **Cerca aggiornamenti** → **Aggiorna ora** → dopo relaunch verifica la nuova versione in UI.

Smoke test rete senza UI (non verifica firma/install):

```bash
curl -fsSL http://127.0.0.1:8765/latest.json | jq .
curl -L -o /tmp/upd.bin "http://127.0.0.1:8765/<artifact>"
```

### 2. Se OK → GitHub Release

1. Ripristina `tauri.conf.json` (endpoint GitHub only, **senza** `dangerousInsecureTransportProtocol`).
2. Builda tutte le piattaforme firmate (arm64 / x64 / Windows) con la versione di release.
3. Genera `latest.json` con URL GitHub:

```bash
npm run release:manifest -- \
  --version 0.2.1 \
  --notes "Descrizione release" \
  --base-url https://github.com/sebastianoboem/AndroidAdwareCleaner/releases/download/v0.2.1 \
  --darwin-aarch64 path/to/AndroidAdwareCleaner_0.2.1_aarch64.app.tar.gz.sig \
  --darwin-x86_64 path/to/AndroidAdwareCleaner_0.2.1_x64.app.tar.gz.sig \
  --windows-x86_64 path/to/AndroidAdwareCleaner_0.2.1_x64-setup.exe.sig
```

4. Crea la release `v0.2.1` e carica:

- `AndroidAdwareCleaner_*.app.tar.gz` + `.sig` (arm64 e x64)
- `AndroidAdwareCleaner_*_x64-setup.exe` + `.sig`
- `.dmg` / installatori manuali (opzionale)
- `latest.json`

Endpoint updater in produzione:

```text
https://github.com/sebastianoboem/AndroidAdwareCleaner/releases/latest/download/latest.json
```

### 3. Smoke-test URL (obbligatorio)

```bash
curl -fsSL \
  "https://github.com/sebastianoboem/AndroidAdwareCleaner/releases/latest/download/latest.json" \
  | head -c 200
```

`curl -fsSL` deve uscire 0 e il body deve iniziare con `{`.

## Endpoint in `tauri.conf.json`

```json
"endpoints": [
  "https://github.com/sebastianoboem/AndroidAdwareCleaner/releases/latest/download/latest.json"
]
```

## Utenti già installati (≤ 0.2.0)

Le build già installate possono ancora avere SourceForge come primo endpoint nel binario. Diventa effettivo (solo GitHub) dalla **prossima** release installata. Chi è su ≤ 0.2.0 continua a funzionare finché SF o il fallback GitHub rispondono; dopo l'update a questa pipeline userà solo GitHub.

## Controllo manuale

In **Impostazioni** → **Cerca aggiornamenti**, oppure automaticamente dopo l'avvio della schermata principale.
