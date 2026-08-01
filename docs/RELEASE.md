# Release e aggiornamenti automatici

L'app usa il plugin **Tauri Updater** con check **duale permanente**:

1. **SourceForge** (priorità) — `latest.json` sul path stabile
2. **GitHub Releases** (fallback) — stesso manifest, se SF risponde non-2XX

```text
GitHub Release ──sync auto──► SourceForge files
App updater ──1. check──► SourceForge
App updater ──2. fallback──► GitHub
```

Si pubblicano **sempre** release su GitHub e SourceForge. Nessun cutover “solo SF” per ora.

## Versioning

La versione è definita in:

- `src-tauri/tauri.conf.json` → `version`
- `package.json`
- `Cargo.toml` (workspace + `src-tauri/Cargo.toml`)

Per allineare tutto:

```bash
node scripts/bump-version.mjs 0.2.0
```

Poi commit, tag e push:

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

## Flusso dual publish (ordine operativo)

### 1. Crea la GitHub Release con tutti gli asset in un colpo

Importante: la **GitHub → SourceForge Release Integration** sincronizza gli asset presenti **al momento della publish**. File aggiunti dopo non vengono visti dal sync (one-shot).

Crea la release `v0.2.0` e carica **subito**:

- `AndroidAdwareCleaner_*.app.tar.gz` + `.sig` (arm64 e x64)
- `AndroidAdwareCleaner_*_x64-setup.exe` + `.sig`
- `.dmg` / installatori manuali (opzionale)

Non includere ancora `latest.json` se non è pronto: puoi aggiungerlo subito dopo (punto 4) e, se serve ri-triggerare il sync SF, **modifica il testo della release** (edit note).

### 2. Attendi sync SourceForge e verifica i path

Controlla su [SF Files → releases](https://sourceforge.net/projects/androidadwarecleaner/files/releases/) dove finiscono gli asset.

Pattern previsto (da confermare al primo sync):

```text
https://sourceforge.net/projects/androidadwarecleaner/files/releases/<tag>/<artifact>/download
```

Se il sync usa una cartella diversa, aggiorna solo `--base-url` alla generazione del manifest — **non** serve ricompilare l'app (l'endpoint `latest.json` resta fisso).

### 3. Genera `latest.json` con base URL SourceForge

```bash
npm run release:manifest -- \
  --version 0.2.0 \
  --notes "Descrizione release" \
  --base-url https://sourceforge.net/projects/androidadwarecleaner/files/releases/v0.2.0 \
  --darwin-aarch64 target/release/bundle/macos/AndroidAdwareCleaner.app.tar.gz.sig \
  --darwin-x86_64 target/x86_64-apple-darwin/release/bundle/macos/AndroidAdwareCleaner.app.tar.gz.sig \
  --windows-x86_64 target/x86_64-pc-windows-msvc/release/bundle/nsis/AndroidAdwareCleaner_0.2.0_x64-setup.exe.sig
```

Lo script aggiunge `/download` agli URL se `--base-url` punta a SourceForge.

### 4. Carica `latest.json` anche sulla GitHub Release

Serve al **fallback** updater: se SF risponde non-2XX, l'app legge:

```text
https://github.com/sebastianoboem/AndroidAdwareCleaner/releases/latest/download/latest.json
```

Dopo l'upload, se il sync SF non ha ancora preso `latest.json`, modifica una riga delle note della release per ri-triggerare.

### 5. Pubblica `latest.json` sul path stabile SourceForge

Il sync GitHub mette gli asset in cartelle per-release; non c'è un equivalente di `releases/latest/download/`. Sovrascrivi il file stabile:

```bash
npm run release:sourceforge
# oppure: node scripts/publish-sourceforge-latest.mjs ./latest.json
```

Remoto: `sebastianoboem@frs.sourceforge.net:/home/frs/project/androidadwarecleaner/releases/latest.json`

Endpoint pubblico (priorità updater):

```text
https://sourceforge.net/projects/androidadwarecleaner/files/releases/latest.json/download
```

### 6. Smoke-test URL (obbligatorio)

Entrambi devono restituire **JSON grezzo**, non HTML. Il fallback Tauri scatta solo su HTTP non-2XX; una landing HTML 200 su SF **bloccherebbe** il fallback.

```bash
# SourceForge (priorità)
curl -fsSL \
  "https://sourceforge.net/projects/androidadwarecleaner/files/releases/latest.json/download" \
  | head -c 200

# GitHub (fallback)
curl -fsSL \
  "https://github.com/sebastianoboem/AndroidAdwareCleaner/releases/latest/download/latest.json" \
  | head -c 200

# Opzionale: un artefatto dalla release syncata (dopo aver fissato il path)
# curl -fsI "https://sourceforge.net/projects/androidadwarecleaner/files/releases/v0.2.0/<artifact>/download"
```

`curl -fsSL` deve uscire 0 e il body deve iniziare con `{`.

### 7. Test updater end-to-end

Da un build con i nuovi endpoint in `tauri.conf.json`: **Impostazioni → Cerca aggiornamenti** (o check automatico all'avvio) → download → install.

## Endpoint in `tauri.conf.json`

```json
"endpoints": [
  "https://sourceforge.net/projects/androidadwarecleaner/files/releases/latest.json/download",
  "https://github.com/sebastianoboem/AndroidAdwareCleaner/releases/latest/download/latest.json"
]
```

Ordine = priorità. Firma (`pubkey`) invariata: cambiano solo gli host di distribuzione.

## Utenti già installati (≤ 0.1.2)

Al primo update leggeranno ancora **solo GitHub** (un solo endpoint nella build installata). Dopo aver installato una build con i due endpoint, i check useranno SF-first + GH fallback.

## Controllo manuale

In **Impostazioni** → **Cerca aggiornamenti**, oppure automaticamente dopo l'avvio della schermata principale.
