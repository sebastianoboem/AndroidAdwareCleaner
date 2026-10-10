# AndroidAdwareCleaner

[![release](https://img.shields.io/github/v/release/sebastianoboem/AndroidAdwareCleaner?label=release)](https://github.com/sebastianoboem/AndroidAdwareCleaner/releases)
[![license](https://img.shields.io/badge/license-MIT-blue)](LICENSE)
[![platform](https://img.shields.io/badge/platform-macOS%20%7C%20Windows-lightgrey)](https://github.com/sebastianoboem/AndroidAdwareCleaner/releases)

**Pulizia adware e app sospette su Android.**

App desktop per **macOS** e **Windows** che collega il telefono via USB (ADB), elenca le app installate, evidenzia quelle sospette grazie a un database condiviso, e permette di disinstallarle in blocco con un report riepilogativo. Può anche liberare spazio: cache, residui di app cancellate, file pubblicitari e APK rimasti.

![Screenshot della schermata principale con lista app, icone, flag e contatori del database reputazione.](/docs/screenshots/03-app-list.png)

---

## Perché usarlo

- **Workflow rapido:** niente menu Android a mano — vedi subito cleaner, VPN truffaldine e adware.
- **Database reputazione:** disinstallazioni e flag (sospetta, sistema, trusted) alimentano un database locale e, se configurato, Supabase o una cartella condivisa.
- **Sicuro:** conferma prima della disinstallazione e della pulizia memoria; niente bypass di FRP o autorizzazione ADB.
- **Multi-PC:** la sync unisce i voti tra più computer. Un flag conta nel totale remoto anche senza disinstallare.

---

## Come funziona

### 1. Collega il telefono

All’avvio l’app verifica ADB e guida alla connessione USB: opzioni sviluppatore, debug USB, autorizzazione PC. Supporta le principali marche con istruzioni Safe Mode per modello.

![Screenshot della guida connessione dispositivo con i passaggi per abilitare il debug USB.](/docs/screenshots/01-connection-guide.png)

![Screenshot del selettore marca dispositivo con elenco Samsung, Xiaomi, Google e altre.](/docs/screenshots/02-brand-selector.png)

### 2. Scansiona e filtra

Lista app con **icona**, nome, autore e package. Si cerca per nome. All’avvio è acceso solo il filtro **Sospetta**: le app trusted restano nascoste, **Solo sospette** è spento (le app senza flag si vedono). Sistema e Google si riaccendono dai pulsanti in testata.

### 3. Segnala con le flag

Ogni riga ha tre flag:

| Flag | Significato |
|------|-------------|
| 🚩 **Sospetta** | Segna o togli adware o app indesiderata |
| ⚙️ **Sistema** | Tratta come app di sistema |
| ✅ **Trusted** | App sicura — sparisce dall’elenco finché il filtro Trusted è spento |

![Screenshot di app segnate come sospette con badge arancioni e flag rosse attive.](/docs/screenshots/04-flags-suspicious.png)

Il pulsante acceso riflette il flag nel database, anche se è arrivato da un altro computer. La colonna **Rim.** conta le disinstallazioni (locale + sync).

### 4. Disinstalla in blocco

Seleziona più app → **Disinstalla**. Il database locale si aggiorna subito. Il cloud si allinea alla sync successiva, non nel momento del click.

![Screenshot della selezione multipla di app sospette pronte per la disinstallazione.](/docs/screenshots/06-bulk-uninstall.png)

![Screenshot del filtro Solo sospette attivo con tre app cleaner evidenziate.](/docs/screenshots/05-filter-suspicious.png)

### 5. Ottimizza memoria

**Ottimizza** analizza la memoria condivisa del telefono e propone cosa eliminare: cache di sistema e delle app, cartelle rimaste dopo una disinstallazione, file pubblicitari (Unity Ads, Mintegral, Moloco e altri SDK), APK non più necessari e file grandi.

Durante l’analisi, sotto il titolo compaiono gli ultimi tre percorsi letti, accorciati alle ultime tre cartelle.

![Screenshot della finestra Ottimizza memoria durante l’analisi, con i percorsi in scansione.](/docs/screenshots/07-optimize.png)

### 6. Sync e aggiornamenti

La sync completa parte:

- all’avvio dell’app, in background;
- quando il telefono risulta collegato;
- dal pulsante **Sync** e da **Sync** nelle impostazioni;
- quando si cambia fornitore, cartella o credenziali Supabase;
- alla chiusura dell’app.

Un flag impostato senza disinstallare viene inviato lo stesso e conta nel totale remoto, un voto per telefono. All’inizio di una scansione app, con Supabase, vengono solo scaricati i totali già presenti sul server.

- **Aggiornamenti automatici** — all’avvio controlla GitHub Releases e propone l’installazione della nuova versione.

---

## Requisiti

| Componente | Dettaglio |
|------------|-----------|
| **PC** | macOS (Apple Silicon o Intel) o Windows 10/11 |
| **Telefono** | Android con **Debug USB** attivo e PC autorizzato |
| **Cavo** | USB dati (non solo ricarica) |
| **ADB** | Installato dall’app al primo avvio, oppure già presente sul sistema |

---

## Download

> [!NOTE]
> Le build ufficiali saranno pubblicate nelle [GitHub Releases](https://github.com/sebastianoboem/AndroidAdwareCleaner/releases).

| Piattaforma | File |
|-------------|------|
| macOS (Apple Silicon) | `AndroidAdwareCleaner_*_aarch64.dmg` |
| macOS (Intel) | `AndroidAdwareCleaner_*_x64.dmg` |
| Windows | `AndroidAdwareCleaner_*_x64-setup.exe` |

---

## Sviluppo

```bash
npm install
bash scripts/download-platform-tools.sh   # opzionale: adb locale
npm run tauri dev
```

Build release:

```bash
npm run tauri build
```

Versioning e pubblicazione release: [docs/RELEASE.md](docs/RELEASE.md).

---

## Struttura dati

- **Database locale:** `~/Library/Application Support/AndroidAdwareCleaner/` (macOS) o equivalente Windows.
- **Sync:** Supabase, oppure il file `reputation-sync.json` in una cartella Drive/OneDrive scelta nelle impostazioni. Senza fornitore il database resta solo su questo computer.

---

## Note legali

Utilizzare solo con **consenso del proprietario del dispositivo**. Lo strumento non aggira protezioni del dispositivo (FRP, blocco schermo, ecc.).

---

## Licenza

[MIT](LICENSE) — © Sebastiano Boem
