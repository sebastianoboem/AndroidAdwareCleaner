# Release e aggiornamenti automatici

L'app usa il plugin **Tauri Updater**: all'avvio controlla un manifest `latest.json` su GitHub Releases e propone l'aggiornamento se la versione remota è più recente.

## Versioning

La versione è definita in:

- `src-tauri/tauri.conf.json` → `version`
- `package.json`
- `Cargo.toml` (workspace + `src-tauri/Cargo.toml`)

Per allineare tutto:

```bash
node scripts/bump-version.mjs 0.2.0
```

Poi commit, tag e release:

```bash
git tag v0.2.0
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

## Pubblicare su GitHub Releases

1. Crea release `v0.2.0` su GitHub.
2. Carica gli artefatti:
   - `AndroidAdwareCleaner.app.tar.gz` + `.sig` (arm64 e x64)
   - `AndroidAdwareCleaner_0.2.0_x64-setup.exe` + `.sig`
   - `.dmg` / `.app` per installazione manuale (opzionale)
3. Genera e carica `latest.json` nella release (o come asset `latest download`):

```bash
node scripts/generate-latest-json.mjs \
  --version 0.2.0 \
  --notes "Descrizione release" \
  --base-url https://github.com/sebastianoboem/AndroidAdwareCleaner/releases/download/v0.2.0 \
  --darwin-aarch64 target/release/bundle/macos/AndroidAdwareCleaner.app.tar.gz.sig \
  --darwin-x86_64 target/x86_64-apple-darwin/release/bundle/macos/AndroidAdwareCleaner.app.tar.gz.sig \
  --windows-x86_64 target/x86_64-pc-windows-msvc/release/bundle/nsis/AndroidAdwareCleaner_0.2.0_x64-setup.exe.sig
```

4. Assicurati che l'URL in `tauri.conf.json` → `plugins.updater.endpoints` punti al repo corretto:

```json
"https://github.com/sebastianoboem/AndroidAdwareCleaner/releases/latest/download/latest.json"
```

`releases/latest/download/` serve sempre l'ultima release che contiene un asset `latest.json`.

## Controllo manuale

In **Impostazioni** → **Cerca aggiornamenti**, oppure automaticamente dopo l'avvio della schermata principale.
