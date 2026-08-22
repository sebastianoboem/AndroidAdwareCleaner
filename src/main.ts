import { getVersion } from "@tauri-apps/api/app";
import { isGoogleApp } from "./googleApps.mjs";
import { Channel, invoke } from "@tauri-apps/api/core";
import { open, save } from "@tauri-apps/plugin-dialog";
import { relaunch } from "@tauri-apps/plugin-process";
import { check, type Update } from "@tauri-apps/plugin-updater";

// --- Types ---

interface SetupStatus {
  os: string;
  os_version: string;
  adb_present: boolean;
  adb_path: string | null;
  drivers_ok: boolean;
  drivers_message: string;
  ready: boolean;
}

interface AdbStatus {
  adb_path: string | null;
  devices: { serial: string; state: string; model?: string }[];
  has_authorized_device: boolean;
}

interface GuideStep {
  title: string;
  body: string;
  phase: string;
}

interface ConnectionGuide {
  steps: GuideStep[];
  tips: GuideStep[];
  resolved_brand: string | null;
}

interface DeviceGuides {
  brands: { brand: string }[];
}

interface PackageRow {
  package_name: string;
  label: string | null;
  author: string | null;
  icon_url: string | null;
  is_system: boolean;
  is_device_admin: boolean;
  uninstall_count: number;
  report_count: number;
  marked_system: boolean;
  marked_trusted: boolean;
  marked_suspicious: boolean;
  is_suspicious: boolean;
  is_reported: boolean;
}

interface PackageReputation {
  package_name: string;
  uninstall_count: number;
  report_count: number;
  marked_system: boolean;
  marked_trusted: boolean;
  marked_suspicious: boolean;
}

const SUSPICIOUS_UNINSTALL_THRESHOLD = 5;

type ScanProgressEvent =
  | {
      kind: "started";
      total: number;
      device_serial: string;
      device_model: string | null;
      device_brand: string | null;
    }
  | { kind: "package"; row: PackageRow }
  | { kind: "finished" };

type UninstallProgressEvent =
  | { kind: "started"; total: number }
  | {
      kind: "item";
      current: number;
      total: number;
      package_name: string;
      status: string;
      message: string;
    }
  | { kind: "finished" };

interface SyncStatus {
  configured: boolean;
  mode: "local" | "synced";
  sync_path: string | null;
  last_pull: string | null;
  last_push: string | null;
  last_error: string | null;
}

interface SyncProviderInfo {
  id: string;
  name: string;
  description: string;
  detected_root: string | null;
  sync_folder: string | null;
  available: boolean;
}

interface SyncSettingsView {
  provider_id: string;
  sync_folder: string | null;
  subfolder: string;
  providers: SyncProviderInfo[];
}

// --- State ---

let packages: PackageRow[] = [];
let packagesScanScope: "user" | "full" = "user";
let scanInFlight = false;
let operationInFlight = false;
let deviceSerial = "";
let deviceBrand = "";
let deviceModel = "";
let lastRemoved: string[] = [];
const selectedPackages = new Set<string>();
let sortKey: keyof PackageRow | "name" = "uninstall_count";
let sortAsc = false;
let guidesCache: DeviceGuides | null = null;
let connectionAttempts = 0;
const CONNECTION_ATTEMPTS_BEFORE_POPUP = 3;
let guideDebounceTimer: number | null = null;

const GENERIC_GUIDE_STEPS: GuideStep[] = [
  {
    title: "Opzioni sviluppatore",
    body: "Impostazioni → Info telefono → 7 tap su Numero build.",
    phase: "developer",
  },
  {
    title: "Debug USB",
    body: "Opzioni sviluppatore → abilita Debug USB.",
    phase: "debug",
  },
  {
    title: "Collega il cavo USB",
    body: "Cavo dati al PC; scegli Trasferimento file / MTP se richiesto.",
    phase: "cable",
  },
  {
    title: "Autorizza il PC",
    body: "Sblocca lo schermo e accetta «Consenti debug USB?» (spunta «Consenti sempre»).",
    phase: "authorize",
  },
];

const GENERIC_GUIDE_TIPS: GuideStep[] = [
  {
    title: "Modalità aereo (consigliato)",
    body: "Riduce popup pubblicitari mentre lavori sulle impostazioni.",
    phase: "airplane",
  },
];

function $(sel: string) {
  return document.querySelector(sel) as HTMLElement | null;
}

function setLoading(text: string) {
  ($("#loading-text") as HTMLElement).textContent = text;
}

function showLoading() {
  $("#screen-loading")?.classList.remove("hidden");
  $("#screen-main")?.classList.add("hidden");
}

function showMain() {
  $("#screen-loading")?.classList.add("hidden");
  $("#screen-main")?.classList.remove("hidden");
}

function delay(ms: number) {
  return new Promise((r) => setTimeout(r, ms));
}

type ToastKind = "success" | "error" | "warn" | "info";

function toast(message: string, kind: ToastKind = "info", durationMs = 4200) {
  const container = $("#toast-container");
  if (!container) return;

  const el = document.createElement("div");
  el.className = `toast ${kind}`;
  el.textContent = message;
  container.appendChild(el);

  window.setTimeout(() => {
    el.classList.add("out");
    el.addEventListener("animationend", () => el.remove(), { once: true });
  }, durationMs);
}

function notifySetupStatus(status: SetupStatus) {
  toast(`Sistema: ${status.os} (${status.os_version})`, "info");

  if (status.adb_present) {
    toast(`ADB: presente${status.adb_path ? ` — ${status.adb_path}` : ""}`, "success");
  } else {
    toast("ADB: non trovato", "error");
  }

  if (status.drivers_ok) {
    toast(`Driver: OK — ${status.drivers_message}`, "success");
  } else {
    toast(`Driver: non disponibili — ${status.drivers_message}`, "warn");
  }
}

function setDbActivity(text: string, kind: "active" | "done" | "err" | "hidden" = "active") {
  const el = $("#db-activity")!;
  if (kind === "hidden") {
    el.classList.add("hidden");
    el.textContent = "";
    el.classList.remove("done", "err");
    return;
  }
  el.classList.remove("hidden", "done", "err");
  if (kind === "done") el.classList.add("done");
  if (kind === "err") el.classList.add("err");
  el.textContent = text;
}

// --- FASE 1 ---

async function setLoadingStep(text: string, ms = 500) {
  setLoading(text);
  await delay(ms);
}

async function runPhase1(): Promise<boolean> {
  showLoading();
  await setLoadingStep("Rilevamento sistema operativo…", 700);

  let status = await invoke<SetupStatus>("check_setup");
  await setLoadingStep("Rilevamento ADB…", 600);
  notifySetupStatus(status);

  if (!status.adb_present || !status.ready) {
    const dlg = $("#dialog-adb") as HTMLDialogElement;
    ($("#dialog-adb-message") as HTMLElement).textContent =
      `${status.drivers_message}\n\nADB non rilevato${status.adb_path ? "" : " nel sistema"}. Puoi installarlo automaticamente (scarica platform-tools) oppure indicare un percorso personalizzato.`;

    return new Promise((resolve) => {
      const onInstall = async () => {
        dlg.close();
        setLoading("Installazione ADB in corso…");
        try {
          status = await invoke<SetupStatus>("run_setup");
          notifySetupStatus(status);
          if (status.ready) {
            toast("ADB installato correttamente", "success");
            resolve(true);
          } else showAdbDialogAgain(resolve);
        } catch (e) {
          setLoading(`Errore installazione: ${e}`);
          await delay(1500);
          showAdbDialogAgain(resolve);
        }
      };

      const onBrowse = async () => {
        const picked = await open({
          directory: false,
          multiple: false,
          title: "Seleziona eseguibile adb (o cartella platform-tools)",
        });
        if (!picked) return;
        const path = Array.isArray(picked) ? picked[0] : picked;
        dlg.close();
        setLoading("Verifica percorso ADB…");
        status = await invoke<SetupStatus>("set_custom_adb_path", { path });
        notifySetupStatus(status);
        if (status.ready) {
          toast("Percorso ADB configurato", "success");
          resolve(true);
        } else showAdbDialogAgain(resolve);
      };

      const installBtn = $("#btn-adb-install")!;
      const browseBtn = $("#btn-adb-browse")!;
      installBtn.replaceWith(installBtn.cloneNode(true));
      browseBtn.replaceWith(browseBtn.cloneNode(true));
      $("#btn-adb-install")!.addEventListener("click", onInstall, { once: true });
      $("#btn-adb-browse")!.addEventListener("click", onBrowse, { once: true });
      dlg.showModal();
    });
  }

  await setLoadingStep("Sistema pronto", 400);
  toast("Sistema pronto per la connessione", "success");
  return true;
}

function showAdbDialogAgain(resolve: (v: boolean) => void) {
  const dlg = $("#dialog-adb") as HTMLDialogElement;
  ($("#dialog-adb-message") as HTMLElement).textContent =
    "ADB ancora non disponibile. Riprova l'installazione o specifica un percorso valido.";
  dlg.showModal();
  $("#btn-adb-install")!.addEventListener(
    "click",
    async () => {
      dlg.close();
      resolve(await runPhase1());
    },
    { once: true }
  );
}

// --- FASE 2 ---

async function loadBrandOptions() {
  guidesCache = await invoke<DeviceGuides>("get_device_guides");
  const select = $("#conn-brand") as HTMLSelectElement;
  select.innerHTML = '<option value="">— Auto da modello —</option>';
  for (const b of guidesCache.brands) {
    const opt = document.createElement("option");
    opt.value = b.brand;
    opt.textContent = b.brand;
    select.appendChild(opt);
  }
}

function scheduleGuideRefresh() {
  if (guideDebounceTimer !== null) clearTimeout(guideDebounceTimer);
  guideDebounceTimer = window.setTimeout(() => {
    guideDebounceTimer = null;
    void renderConnectionGuideInDialog();
  }, 280);
}

function renderGuideSteps(container: HTMLElement, steps: GuideStep[]) {
  container.replaceChildren();
  steps.forEach((s, i) => {
    const step = document.createElement("div");
    step.className = "guide-step";
    const title = document.createElement("h3");
    title.textContent = `${i + 1}. ${s.title}`;
    const body = document.createElement("p");
    body.textContent = s.body;
    step.append(title, body);
    container.appendChild(step);
  });
}

function renderGuideTips(container: HTMLElement, tips: GuideStep[]) {
  if (!tips.length) {
    container.classList.add("hidden");
    container.replaceChildren();
    return;
  }
  container.classList.remove("hidden");
  container.replaceChildren();
  const heading = document.createElement("h4");
  heading.textContent = "Consigli opzionali (anti-pubblicità)";
  container.appendChild(heading);
  tips.forEach((s) => {
    const tip = document.createElement("p");
    tip.className = `guide-tip${s.phase === "safe_mode" ? " guide-tip-safe" : ""}`;
    const label = document.createElement("strong");
    label.textContent = `${s.title}: `;
    tip.append(label, document.createTextNode(s.body));
    container.appendChild(tip);
  });
}

function closeConnectionDialog() {
  $("#dialog-connection")?.classList.add("hidden");
}

async function renderConnectionGuideInDialog() {
  const brand = ($("#conn-brand") as HTMLSelectElement).value || null;
  const model = ($("#conn-model") as HTMLInputElement).value.trim() || null;
  const stepsEl = $("#conn-guide-steps")!;

  let guide: ConnectionGuide;
  try {
    guide = await invoke<ConnectionGuide>("get_connection_guide", {
      brand,
      model,
      device_unusable: true,
    });
  } catch (e) {
    console.error("get_connection_guide failed:", e);
    guide = {
      steps: GENERIC_GUIDE_STEPS,
      tips: GENERIC_GUIDE_TIPS,
      resolved_brand: null,
    };
    toast("Impossibile caricare la guida — mostro istruzioni generiche", "error");
  }
  const resolvedEl = $("#conn-resolved")!;
  const selectedBrand = ($("#conn-brand") as HTMLSelectElement).value;
  if (guide.resolved_brand) {
    resolvedEl.textContent = `Istruzioni per ${guide.resolved_brand}${model ? ` · ${model}` : ""}`;
    resolvedEl.classList.remove("hidden");
    if (!selectedBrand) {
      ($("#conn-brand") as HTMLSelectElement).value = guide.resolved_brand;
    }
  } else if (selectedBrand) {
    resolvedEl.textContent = `Istruzioni per ${selectedBrand}${model ? ` · ${model}` : ""}`;
    resolvedEl.classList.remove("hidden");
  } else if (model && model.length >= 2) {
    resolvedEl.textContent = "Modello non riconosciuto — istruzioni generiche";
    resolvedEl.classList.remove("hidden");
  } else {
    resolvedEl.classList.add("hidden");
  }

  const steps = guide.steps.length > 0 ? guide.steps : GENERIC_GUIDE_STEPS;
  const tips = guide.tips?.length ? guide.tips : GENERIC_GUIDE_TIPS;
  renderGuideSteps(stepsEl, steps);
  renderGuideTips($("#conn-guide-tips")!, tips);
}

async function showConnectionDialog() {
  const overlay = $("#dialog-connection")!;
  const stepsEl = $("#conn-guide-steps")!;
  stepsEl.replaceChildren();
  const loading = document.createElement("p");
  loading.className = "guide-loading muted";
  loading.textContent = "Caricamento istruzioni…";
  stepsEl.appendChild(loading);
  $("#conn-guide-tips")?.classList.add("hidden");
  overlay.classList.remove("hidden");
  await renderConnectionGuideInDialog();
}

async function tryConnect(): Promise<boolean> {
  const status = await invoke<AdbStatus>("get_adb_status");
  if (status.has_authorized_device) {
    const dev = status.devices.find((d) => d.state === "device");
    if (dev?.model) deviceModel = dev.model;
    return true;
  }
  return false;
}

async function runPhase2(): Promise<void> {
  showLoading();
  connectionAttempts = 0;
  await loadBrandOptions();

  while (true) {
    setLoading("Tentativo di connessione al device…");
    connectionAttempts++;

    if (await tryConnect()) {
      await setLoadingStep("Dispositivo connesso", 500);
      await onDeviceConnected();
      return;
    }

    if (connectionAttempts >= CONNECTION_ATTEMPTS_BEFORE_POPUP) {
      await showConnectionDialog();
      await new Promise<void>((resolve) => {
        const retry = $("#btn-retry-connection")!;

        const handler = async () => {
          closeConnectionDialog();
          connectionAttempts = 0;
          resolve();
        };

        retry.replaceWith(retry.cloneNode(true));
        $("#btn-retry-connection")!.addEventListener("click", handler, { once: true });
      });
    } else {
      await delay(2000);
    }
  }
}

// --- FASE 3: connessione OK → pull DB ---

async function onDeviceConnected() {
  showMain();

  try {
    await invoke("sync_pull");
  } catch {
    // solo locale o remoto non disponibile — continua senza bloccare
  }

  await refreshDbPanel();
  await scanPackages();
}

let pendingUpdate: Update | null = null;
let continueAfterUpdateCheck: (() => void) | null = null;

function closeUpdateDialog() {
  $("#dialog-update")!.classList.add("hidden");
  $("#update-progress")!.classList.add("hidden");
  ($("#btn-update-now") as HTMLButtonElement).disabled = false;
  ($("#btn-update-later") as HTMLButtonElement).disabled = false;
  continueAfterUpdateCheck?.();
  continueAfterUpdateCheck = null;
}

function showUpdateDialog(update: Update) {
  pendingUpdate = update;
  ($("#update-version") as HTMLElement).textContent = `Versione ${update.version} disponibile`;
  ($("#update-notes") as HTMLElement).textContent =
    update.body?.trim() || "Sono disponibili miglioramenti e correzioni.";
  $("#dialog-update")!.classList.remove("hidden");
}

async function checkForAppUpdatesOnStartup(): Promise<void> {
  setLoading("Controllo aggiornamenti…");
  try {
    const update = await check();
    if (!update) return;
    showUpdateDialog(update);
    await new Promise<void>((resolve) => {
      continueAfterUpdateCheck = resolve;
    });
  } catch {
    // offline o release non pubblicata — continua verso la connessione device
  }
}

async function checkForAppUpdates(silent = true) {
  try {
    const update = await check();
    if (!update) {
      if (!silent) toast("Sei già aggiornato.", "success");
      return;
    }
    showUpdateDialog(update);
  } catch {
    if (!silent) {
      toast("Controllo aggiornamenti non disponibile (offline o release non pubblicata).", "warn");
    }
  }
}

async function installPendingUpdate() {
  if (!pendingUpdate) return;
  const update = pendingUpdate;
  ($("#btn-update-now") as HTMLButtonElement).disabled = true;
  ($("#btn-update-later") as HTMLButtonElement).disabled = true;
  $("#update-progress")!.classList.remove("hidden");

  let downloaded = 0;
  let contentLength: number | undefined;

  try {
    await update.downloadAndInstall((event) => {
      if (event.event === "Started") {
        contentLength = event.data.contentLength ?? undefined;
        downloaded = 0;
        ($("#update-progress-text") as HTMLElement).textContent = "Download in corso…";
      } else if (event.event === "Progress") {
        downloaded += event.data.chunkLength;
        const mb = (downloaded / (1024 * 1024)).toFixed(1);
        if (contentLength && contentLength > 0) {
          const pct = Math.min(100, Math.round((downloaded / contentLength) * 100));
          const totalMb = (contentLength / (1024 * 1024)).toFixed(1);
          ($("#update-progress-text") as HTMLElement).textContent =
            `Download in corso… ${mb} / ${totalMb} MB (${pct}%)`;
        } else {
          ($("#update-progress-text") as HTMLElement).textContent =
            `Download in corso… ${mb} MB`;
        }
      } else if (event.event === "Finished") {
        ($("#update-progress-text") as HTMLElement).textContent = "Installazione…";
      }
    });
    await relaunch();
  } catch (e) {
    toast(`Aggiornamento fallito: ${e}`, "error");
    closeUpdateDialog();
  }
}

let syncConfigured = false;

function updateSyncButton(configured: boolean, loading = false) {
  syncConfigured = configured;
  const btn = $("#btn-sync-full") as HTMLButtonElement | null;
  const wrap = $("#btn-sync-wrap") as HTMLElement | null;
  if (!btn || !wrap) return;
  btn.classList.toggle("loading", loading);
  wrap.classList.toggle("sync-unavailable", !configured);
  btn.disabled = !configured || loading;
}

function setSyncButtonLoading(loading: boolean) {
  updateSyncButton(syncConfigured, loading);
}

async function runSync(notify = true) {
  const btn = $("#btn-sync-full") as HTMLButtonElement | null;
  if (!syncConfigured || btn?.classList.contains("loading")) return;

  setSyncButtonLoading(true);
  try {
    await invoke("sync_now");
    await scanPackages();
    await refreshDbPanel();
    if (notify) toast("Sync completato.", "success");
  } catch (e) {
    if (notify) toast(`Sync non riuscito: ${e}`, "error");
    throw e;
  } finally {
    setSyncButtonLoading(false);
  }
}

async function refreshDbPanel() {
  const status = await invoke<SyncStatus>("get_sync_status");
  const btn = $("#btn-sync-full") as HTMLButtonElement | null;
  updateSyncButton(status.configured, btn?.classList.contains("loading") ?? false);
  const el = $("#db-status")!;

  const modeLabel = status.configured ? "sincronizzato" : "solo locale";
  const badgeClass = status.configured ? "sync-synced" : "sync-local";

  el.innerHTML = `
    <span class="sync-badge ${badgeClass}">DB: ${modeLabel}</span>
    <span class="muted">${deviceBrand} ${deviceModel} · ${deviceSerial}</span>
    ${status.last_pull ? `<span class="muted">↓ ${formatTime(status.last_pull)}</span>` : ""}
    ${status.last_push ? `<span class="muted">↑ ${formatTime(status.last_push)}</span>` : ""}
    ${status.last_error ? `<span style="color:#f87171">${escapeHtml(status.last_error)}</span>` : ""}
  `;
}

function formatTime(iso: string) {
  try {
    return new Date(iso).toLocaleString("it-IT", { hour: "2-digit", minute: "2-digit" });
  } catch {
    return iso;
  }
}

// --- FASE 4: lista app ---

function escapeHtml(s: string) {
  return s.replace(/&/g, "&amp;").replace(/</g, "&lt;").replace(/>/g, "&gt;");
}

function packageIconHtml(iconUrl: string | null): string {
  if (
    !iconUrl ||
    (!iconUrl.startsWith("https://") && !iconUrl.startsWith("data:image/"))
  ) {
    return `<span class="pkg-icon pkg-icon-fallback" aria-hidden="true">📦</span>`;
  }
  const safe = iconUrl.replace(/"/g, "&quot;");
  return `<img class="pkg-icon" src="${safe}" alt="" loading="lazy" decoding="async" />`;
}

function getSelectedPackages(): string[] {
  return Array.from(selectedPackages);
}

function pruneSelectedPackages() {
  const names = new Set(packages.map((p) => p.package_name));
  for (const pkg of selectedPackages) {
    if (!names.has(pkg)) selectedPackages.delete(pkg);
  }
}

function updateSelectAllState() {
  const el = $("#select-all") as HTMLInputElement | null;
  if (!el) return;
  const visible = document.querySelectorAll<HTMLInputElement>(".pkg-check");
  if (visible.length === 0) {
    el.checked = false;
    el.indeterminate = false;
    return;
  }
  const checkedCount = Array.from(visible).filter((c) => c.checked).length;
  el.checked = checkedCount === visible.length;
  el.indeterminate = checkedCount > 0 && checkedCount < visible.length;
}

function getFilteredPackages(): PackageRow[] {
  const filter = ($("#filter-text") as HTMLInputElement).value.toLowerCase();
  const hideSystem = ($("#hide-system") as HTMLInputElement).checked;
  const hideGoogle = ($("#hide-google") as HTMLInputElement).checked;
  const onlySuspicious = ($("#filter-suspicious") as HTMLInputElement).checked;

  return packages.filter((p) => {
    if (hideSystem && (p.is_system || p.marked_system)) return false;
    if (hideGoogle && isGoogleApp(p.package_name)) return false;
    if (onlySuspicious && !p.is_suspicious) return false;
    const hay = `${p.package_name} ${p.label ?? ""} ${p.author ?? ""}`.toLowerCase();
    return hay.includes(filter);
  });
}

function setOperationProgress(
  visible: boolean,
  current: number,
  total: number,
  label: string,
) {
  const bar = $("#operation-progress");
  const fill = $("#progress-fill");
  const text = $("#operation-progress-text");
  const track = bar?.querySelector(".progress-track") as HTMLElement | null;
  if (!bar || !fill || !text) return;

  if (visible) {
    bar.classList.remove("hidden");
    const pct = total > 0 ? Math.min(100, Math.round((current / total) * 100)) : 0;
    fill.style.width = `${pct}%`;
    text.textContent = label;
    if (track) {
      track.setAttribute("aria-valuenow", String(pct));
      track.setAttribute("aria-valuemax", "100");
    }
  } else {
    bar.classList.add("hidden");
    fill.style.width = "0%";
    text.textContent = "";
  }
}

function setPackagesLoading(loading: boolean, longHint = false) {
  const tbody = $("#packages-body");
  if (loading) {
    tbody!.innerHTML = "";
    setScanControlsDisabled(true);
    setOperationProgress(true, 0, 0, longHint
      ? "Scansione app… (con le app di sistema può richiedere alcuni minuti)"
      : "Scansione app…");
  } else {
    setOperationProgress(false, 0, 0, "");
    setScanControlsDisabled(false);
  }
}

function buildPackageRowHtml(p: PackageRow): string {
  const badges = [];
  if (p.is_suspicious) badges.push('<span class="badge suspicious">sospetta</span>');
  if (p.is_system) badges.push('<span class="badge system">sistema</span>');
  if (p.marked_system && !p.is_system) badges.push('<span class="badge system">sistema DB</span>');
  if (p.marked_trusted) badges.push('<span class="badge trusted">trusted</span>');
  if (p.is_device_admin) badges.push('<span class="badge admin">admin</span>');

  const title = p.label ?? p.package_name;
  const authorLine = p.author
    ? `<span class="pkg-by"> by </span><span class="pkg-author">${escapeHtml(p.author)}</span>`
    : "";

  const checked = selectedPackages.has(p.package_name) ? " checked" : "";
  return `
    <tr class="${p.is_suspicious ? "row-suspicious" : ""}" data-package="${p.package_name}">
      <td class="col-check"><input type="checkbox" class="pkg-check" data-package="${p.package_name}"${checked} /></td>
      <td>
        <div class="pkg-row-main">
          ${packageIconHtml(p.icon_url)}
          <div class="pkg-text">
            <div class="pkg-headline">
              <span class="pkg-title">${escapeHtml(title)}</span>${authorLine}${badges.join("")}
            </div>
            <span class="pkg-id">${escapeHtml(p.package_name)}</span>
          </div>
        </div>
      </td>
      <td>${p.uninstall_count > 0 ? `<strong>${p.uninstall_count}×</strong>` : "—"}</td>
      <td class="col-flags">
        <button type="button" class="btn-flag btn-flag-suspicious ${p.marked_suspicious ? "active" : ""}"
          data-package="${p.package_name}" title="${p.marked_suspicious ? "Rimuovi sospetta" : "Segna come sospetta/adware"}">⚑</button>
        <button type="button" class="btn-flag btn-flag-system ${p.marked_system ? "active" : ""}"
          data-package="${p.package_name}" title="Segna come app di sistema">⚙</button>
        <button type="button" class="btn-flag btn-flag-trusted ${p.marked_trusted ? "active" : ""}"
          data-package="${p.package_name}" title="Segna come app trusted">✓</button>
      </td>
    </tr>`;
}

function appendPackageRow(p: PackageRow) {
  const tbody = $("#packages-body");
  if (!tbody) return;
  tbody.insertAdjacentHTML("beforeend", buildPackageRowHtml(p));
}

function removePackageRow(packageName: string) {
  const row = $("#packages-body")?.querySelector(
    `tr[data-package="${CSS.escape(packageName)}"]`,
  );
  row?.remove();
  packages = packages.filter((p) => p.package_name !== packageName);
  selectedPackages.delete(packageName);
}

function setScanControlsDisabled(disabled: boolean) {
  ($("#hide-system") as HTMLInputElement).disabled = disabled;
  ($("#hide-google") as HTMLInputElement).disabled = disabled;
  ($("#filter-suspicious") as HTMLInputElement).disabled = disabled;
  ($("#filter-text") as HTMLInputElement).disabled = disabled;
  ($("#btn-rescan") as HTMLButtonElement).disabled = disabled;
}

function renderTable() {
  const tbody = $("#packages-body")!;

  let filtered = getFilteredPackages();

  filtered = [...filtered].sort((a, b) => {
    let av: string | number = "";
    let bv: string | number = "";
    switch (sortKey) {
      case "name":
        av = (a.label ?? a.package_name).toLowerCase();
        bv = (b.label ?? b.package_name).toLowerCase();
        break;
      case "package_name":
        av = a.package_name;
        bv = b.package_name;
        break;
      case "uninstall_count":
        av = a.uninstall_count;
        bv = b.uninstall_count;
        break;
      default:
        av = a.uninstall_count;
        bv = b.uninstall_count;
    }
    if (av < bv) return sortAsc ? -1 : 1;
    if (av > bv) return sortAsc ? 1 : -1;
    return 0;
  });

  tbody.innerHTML = filtered
    .map((p) => buildPackageRowHtml(p))
    .join("");

  updateActionButtons();
  updateSelectAllState();
}

function updateActionButtons() {
  const n = getSelectedPackages().length;
  ($("#btn-uninstall") as HTMLButtonElement).disabled = n === 0;
  ($("#btn-export") as HTMLButtonElement).disabled = lastRemoved.length === 0;
}

async function scanPackages(options?: { forceFull?: boolean }) {
  if (scanInFlight || operationInFlight) return;

  const hideSystem = ($("#hide-system") as HTMLInputElement).checked;
  const userOnly = options?.forceFull ? false : hideSystem;
  const longHint = !userOnly;

  scanInFlight = true;
  operationInFlight = true;
  packages = [];
  setPackagesLoading(true, longHint);
  ($("#scan-info") as HTMLElement).textContent = "Scansione app…";

  let received = 0;
  let total = 0;

  const onProgress = new Channel<ScanProgressEvent>();
  onProgress.onmessage = (event) => {
    if (event.kind === "started") {
      total = event.total;
      deviceSerial = event.device_serial;
      deviceBrand = event.device_brand ?? deviceBrand;
      deviceModel = event.device_model ?? deviceModel;
      setOperationProgress(
        true,
        0,
        total,
        longHint
          ? `Scansione app… 0 / ${total} (con le app di sistema può richiedere alcuni minuti)`
          : `Scansione app… 0 / ${total}`,
      );
      updateScanInfo();
      return;
    }

    if (event.kind === "package") {
      packages.push(event.row);
      received += 1;
      appendPackageRow(event.row);
      setOperationProgress(
        true,
        received,
        total,
        longHint
          ? `Scansione app… ${received} / ${total} (con le app di sistema può richiedere alcuni minuti)`
          : `Scansione app… ${received} / ${total}`,
      );
      ($("#scan-info") as HTMLElement).textContent = `Scansione app… ${received} / ${total}`;
      return;
    }

    if (event.kind === "finished") {
      pruneSelectedPackages();
      packagesScanScope = userOnly ? "user" : "full";
      renderTable();
      updateScanInfo();
    }
  };

  try {
    await invoke("scan_packages", {
      user_only: userOnly,
      serial: null,
      on_progress: onProgress,
    });
    await refreshDbPanel();
  } catch (e) {
    ($("#scan-info") as HTMLElement).textContent = `Errore: ${e}`;
    toast(`Errore scansione: ${e}`, "error");
  } finally {
    scanInFlight = false;
    operationInFlight = false;
    setPackagesLoading(false);
  }
}

function onHideSystemChange() {
  const hideSystem = ($("#hide-system") as HTMLInputElement).checked;
  if (!hideSystem && packagesScanScope === "user") {
    void scanPackages({ forceFull: true });
    return;
  }
  updateScanInfo();
  renderTable();
}

function applyReputationUpdate(rep: PackageReputation) {
  const row = packages.find((p) => p.package_name === rep.package_name);
  if (!row) return;
  row.uninstall_count = rep.uninstall_count;
  row.report_count = rep.report_count;
  row.marked_system = rep.marked_system;
  row.marked_trusted = rep.marked_trusted;
  row.marked_suspicious = rep.marked_suspicious;
  row.is_reported = rep.marked_suspicious;
  const whitelisted = row.marked_trusted || row.marked_system || row.is_system;
  row.is_suspicious =
    !whitelisted &&
    (row.marked_suspicious || row.uninstall_count > SUSPICIOUS_UNINSTALL_THRESHOLD);
  updateScanInfo();
  renderTable();
}

function updateScanInfo() {
  const visible = getFilteredPackages();
  const suspicious = visible.filter((p) => p.is_suspicious).length;
  ($("#scan-info") as HTMLElement).textContent =
    `${visible.length} app · ${suspicious} sospette`;
}

function flashDbActivity(label: string) {
  toast(label, "success", 1000);
  void refreshDbPanel();
}

async function toggleMark(
  packageName: string,
  field: "marked_system" | "marked_trusted" | "marked_suspicious",
  current: boolean
) {
  try {
    const rep = await invoke<PackageReputation>("set_package_marks", {
      req: {
        package_name: packageName,
        [field]: !current,
      },
    });
    applyReputationUpdate(rep);
    const label =
      field === "marked_system"
        ? !current
          ? "Segnata come sistema"
          : "Rimossa segnalazione sistema"
        : field === "marked_trusted"
          ? !current
            ? "Segnata come trusted"
            : "Rimossa segnalazione trusted"
          : !current
            ? "Segnata come sospetta"
            : "Rimossa segnalazione sospetta";
    flashDbActivity(label);
  } catch (e) {
    setDbActivity(`Errore: ${e}`, "err");
  }
}

// --- FASE 5/6: disinstallazione → DB locale + push ---

async function bulkUninstall() {
  const selected = getSelectedPackages();
  if (!selected.length || operationInFlight) return;
  if (!confirm(`Disinstallare ${selected.length} app selezionate?`)) return;

  operationInFlight = true;
  setScanControlsDisabled(true);
  ($("#btn-uninstall") as HTMLButtonElement).disabled = true;

  const removed: string[] = [];
  let total = selected.length;

  const onProgress = new Channel<UninstallProgressEvent>();
  onProgress.onmessage = (event) => {
    if (event.kind === "started") {
      total = event.total;
      setOperationProgress(true, 0, total, `Disinstallazione… 0 / ${total}`);
      return;
    }

    if (event.kind === "item") {
      setOperationProgress(
        true,
        event.current,
        event.total,
        `Disinstallazione… ${event.current} / ${event.total} — ${event.package_name}`,
      );
      if (event.status === "success") {
        removed.push(event.package_name);
        removePackageRow(event.package_name);
      }
      return;
    }
  };

  try {
    await invoke("bulk_uninstall", {
      req: { packages: selected, dry_run: false, serial: null },
      on_progress: onProgress,
    });
    lastRemoved = removed;

    operationInFlight = false;
    await scanPackages();
    await refreshDbPanel();
    toast(`${lastRemoved.length} app rimosse.`, "success");
  } catch (e) {
    toast(`Errore disinstallazione: ${e}`, "error");
  } finally {
    operationInFlight = false;
    setOperationProgress(false, 0, 0, "");
    setScanControlsDisabled(false);
    updateActionButtons();
  }
}

function closeSettings() {
  $("#dialog-settings")!.classList.add("hidden");
}

function renderSettingsProviders(settings: SyncSettingsView) {
  const el = $("#settings-providers")!;
  el.innerHTML = settings.providers
    .map((p) => {
      const checked = settings.provider_id === p.id;
      const canSelect = p.id === "local" || p.id === "custom" || p.available;
      const status =
        p.id === "local" || p.id === "custom"
          ? ""
          : p.available
            ? "Rilevato"
            : "Non installato";
      const path = p.sync_folder ?? p.detected_root ?? "";
      return `
      <label class="settings-provider ${canSelect ? "" : "unavailable"}">
        <input type="radio" name="sync-provider" value="${p.id}" ${checked ? "checked" : ""}
          ${canSelect ? "" : "disabled"} />
        <span class="settings-provider-body">
          <strong>${escapeHtml(p.name)}</strong>
          ${status ? `<span class="settings-provider-badge">${status}</span>` : ""}
          <span class="muted">${escapeHtml(p.description)}</span>
          ${path && p.id !== "local" ? `<code class="settings-provider-path">${escapeHtml(path)}</code>` : ""}
        </span>
      </label>`;
    })
    .join("");
}

function updateSettingsCustomPath(settings: SyncSettingsView) {
  const custom = $("#settings-custom-path")!;
  const input = $("#settings-folder-input") as HTMLInputElement;
  const isCustom = settings.provider_id === "custom";
  custom.classList.toggle("hidden", !isCustom);
  input.value = settings.sync_folder ?? "";
}

async function updateSettingsStatusText() {
  const status = await invoke<SyncStatus>("get_sync_status");
  const el = $("#settings-sync-status")!;
  if (!status.configured) {
    el.textContent = "Stato: solo locale — nessun push/pull cloud.";
    return;
  }
  const parts = [`Stato: sincronizzato`, status.sync_path ?? ""];
  if (status.last_pull) parts.push(`↓ ${formatTime(status.last_pull)}`);
  if (status.last_push) parts.push(`↑ ${formatTime(status.last_push)}`);
  el.textContent = parts.join(" · ");
}

async function openSettings() {
  const settings = await invoke<SyncSettingsView>("get_sync_settings");
  renderSettingsProviders(settings);
  updateSettingsCustomPath(settings);
  await updateSettingsStatusText();
  ($("#settings-app-version") as HTMLElement).textContent = `Versione: ${await getVersion()}`;
  $("#dialog-settings")!.classList.remove("hidden");
}

async function applySyncProvider(providerId: string) {
  try {
    if (providerId === "custom") {
      updateSettingsCustomPath({
        provider_id: "custom",
        sync_folder: ($("#settings-folder-input") as HTMLInputElement).value || null,
        subfolder: "AndroidAdwareCleaner",
        providers: [],
      });
      return;
    }
    await invoke("set_sync_provider", { provider_id: providerId });
    toast(
      providerId === "local" ? "Modalità solo locale" : "Cloud collegato",
      "success"
    );
    await refreshDbPanel();
    const settings = await invoke<SyncSettingsView>("get_sync_settings");
    renderSettingsProviders(settings);
    updateSettingsCustomPath(settings);
    await updateSettingsStatusText();
  } catch (e) {
    toast(String(e), "error");
  }
}

async function exportReport(format: "csv" | "pdf") {
  const path = await save({
    defaultPath: `report-${Date.now()}.${format}`,
    filters: [{ name: format.toUpperCase(), extensions: [format] }],
  });
  if (!path) return;
  const imei = prompt("IMEI (opzionale):") ?? undefined;
  await invoke("export_report", {
    req: {
      device_serial: deviceSerial,
      imei: imei || null,
      removed_packages: lastRemoved,
      output_path: path,
      format,
    },
  });
}

// --- Init ---

async function boot() {
  const ok = await runPhase1();
  if (!ok) return;
  await checkForAppUpdatesOnStartup();
  await runPhase2();
}

window.addEventListener("DOMContentLoaded", () => {
  boot();

  $("#conn-brand")?.addEventListener("change", () => void renderConnectionGuideInDialog());
  $("#conn-model")?.addEventListener("input", scheduleGuideRefresh);

  $("#btn-sync-full")?.addEventListener("click", () => void runSync(true));

  $("#btn-settings")?.addEventListener("click", () => void openSettings());

  $("#btn-settings-close")?.addEventListener("click", closeSettings);

  $("#btn-settings-check-update")?.addEventListener("click", () => void checkForAppUpdates(false));

  $("#btn-update-later")?.addEventListener("click", closeUpdateDialog);
  $("#btn-update-now")?.addEventListener("click", () => void installPendingUpdate());
  $("#dialog-update")?.addEventListener("click", (e) => {
    if (e.target === $("#dialog-update")) closeUpdateDialog();
  });

  $("#dialog-settings")?.addEventListener("click", (e) => {
    if (e.target === $("#dialog-settings")) closeSettings();
  });

  $("#settings-providers")?.addEventListener("change", (e) => {
    const t = e.target as HTMLInputElement;
    if (t.name === "sync-provider" && t.checked) void applySyncProvider(t.value);
  });

  $("#btn-settings-browse")?.addEventListener("click", async () => {
    const folder = await open({ directory: true, multiple: false });
    if (!folder) return;
    const path = Array.isArray(folder) ? folder[0] : folder;
    try {
      await invoke("set_sync_folder", { folder: path });
      ($("#settings-folder-input") as HTMLInputElement).value = path;
      toast("Cartella sincronizzazione impostata", "success");
      await refreshDbPanel();
      const settings = await invoke<SyncSettingsView>("get_sync_settings");
      renderSettingsProviders(settings);
      updateSettingsCustomPath(settings);
      await updateSettingsStatusText();
    } catch (err) {
      toast(String(err), "error");
    }
  });

  $("#btn-settings-sync-now")?.addEventListener("click", async () => {
    try {
      await invoke("sync_now");
      toast("Sync completato", "success");
      await refreshDbPanel();
      await updateSettingsStatusText();
    } catch (e) {
      toast(String(e), "error");
    }
  });

  $("#btn-rescan")?.addEventListener("click", () => void scanPackages());
  $("#btn-uninstall")?.addEventListener("click", bulkUninstall);
  $("#btn-export")?.addEventListener("click", () => {
    exportReport(confirm("OK = CSV, Annulla = PDF") ? "csv" : "pdf");
  });

  $("#filter-text")?.addEventListener("input", () => {
    updateScanInfo();
    renderTable();
  });
  $("#hide-system")?.addEventListener("change", onHideSystemChange);
  $("#filter-suspicious")?.addEventListener("change", () => {
    updateScanInfo();
    renderTable();
  });
  $("#hide-google")?.addEventListener("change", () => {
    updateScanInfo();
    renderTable();
  });

  $("#select-all")?.addEventListener("change", (e) => {
    const checked = (e.target as HTMLInputElement).checked;
    for (const p of getFilteredPackages()) {
      if (checked) selectedPackages.add(p.package_name);
      else selectedPackages.delete(p.package_name);
    }
    renderTable();
  });

  $("#packages-body")?.addEventListener("click", (e) => {
    const t = e.target as HTMLElement;
    if (t.classList.contains("pkg-check")) {
      const input = t as HTMLInputElement;
      const pkg = input.dataset.package;
      if (pkg) {
        if (input.checked) selectedPackages.add(pkg);
        else selectedPackages.delete(pkg);
      }
      updateActionButtons();
      updateSelectAllState();
      return;
    }
    const pkg = t.dataset.package;
    if (!pkg) return;
    if (t.classList.contains("btn-flag-suspicious")) {
      const row = packages.find((p) => p.package_name === pkg);
      toggleMark(pkg, "marked_suspicious", row?.marked_suspicious ?? false);
    }
    if (t.classList.contains("btn-flag-system")) {
      const row = packages.find((p) => p.package_name === pkg);
      toggleMark(pkg, "marked_system", row?.marked_system ?? false);
    }
    if (t.classList.contains("btn-flag-trusted")) {
      const row = packages.find((p) => p.package_name === pkg);
      toggleMark(pkg, "marked_trusted", row?.marked_trusted ?? false);
    }
  });

  document.querySelectorAll("#packages-table th[data-sort]").forEach((th) => {
    th.addEventListener("click", () => {
      const key = th.getAttribute("data-sort")!;
      const map: Record<string, keyof PackageRow | "name"> = {
        name: "name",
        uninstall: "uninstall_count",
      };
      const newKey = map[key] ?? "name";
      if (sortKey === newKey) sortAsc = !sortAsc;
      else {
        sortKey = newKey;
        sortAsc = key === "uninstall" ? false : true;
      }
      renderTable();
    });
  });
});
