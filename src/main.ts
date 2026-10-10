import { getVersion } from "@tauri-apps/api/app";
import { Channel, invoke } from "@tauri-apps/api/core";
import { listen } from "@tauri-apps/api/event";
import { open, save } from "@tauri-apps/plugin-dialog";
import { relaunch } from "@tauri-apps/plugin-process";
import { check, type Update } from "@tauri-apps/plugin-updater";

import { isGoogleApp } from "./googleApps.mjs";

declare const __FULLSCAN__: boolean;

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
  my_vote: string | null;
}

interface PackageReputation {
  package_name: string;
  uninstall_count: number;
  report_count: number;
  marked_system: boolean;
  marked_trusted: boolean;
  marked_suspicious: boolean;
  my_vote: string | null;
}

const SUSPICIOUS_UNINSTALL_THRESHOLD = 5;
const SUSPICIOUS_REPORT_THRESHOLD = 5;

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
  supabase_url: string | null;
  supabase_key: string | null;
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

function placeConnectionDialog() {
  const bar = $("#db-panel");
  const height = bar?.offsetHeight ?? 0;
  document.documentElement.style.setProperty("--db-panel-height", `${height}px`);
}

async function showConnectionDialog() {
  placeConnectionDialog();
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
  connectionAttempts = 0;
  await loadBrandOptions();
  showMain();
  await refreshDbPanel();
  ($("#scan-info") as HTMLElement).textContent = "Nessun dispositivo collegato";

  while (true) {
    connectionAttempts++;

    if (await tryConnect()) {
      closeConnectionDialog();
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

const ICON_FALLBACK_HTML = `<span class="pkg-icon pkg-icon-fallback" aria-hidden="true">📦</span>`;

function packageIconHtml(iconUrl: string | null): string {
  if (
    !iconUrl ||
    (!iconUrl.startsWith("https://") && !iconUrl.startsWith("data:image/"))
  ) {
    return ICON_FALLBACK_HTML;
  }
  const safe = iconUrl.replace(/"/g, "&quot;");
  return `<img class="pkg-icon" src="${safe}" alt="" loading="lazy" decoding="async" />`;
}

// Le icone arrivano da URL Play Store o data-URL: se il caricamento fallisce
// (rete assente, throttling, app rimossa dallo store) mostra il placeholder
// invece del glifo di immagine rotta. Gli eventi error non fanno bubbling,
// quindi serve un listener in fase di capture.
function installIconErrorFallback() {
  document.addEventListener(
    "error",
    (e) => {
      const target = e.target;
      if (target instanceof HTMLImageElement && target.classList.contains("pkg-icon")) {
        target.outerHTML = ICON_FALLBACK_HTML;
      }
    },
    true,
  );
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

type PackageFlag = "suspicious" | "system" | "google" | "trusted";

function packageFlag(p: PackageRow): PackageFlag | null {
  if (p.is_suspicious) return "suspicious";
  if (p.marked_trusted) return "trusted";
  if (isGoogleApp(p.package_name)) return "google";
  if (p.is_system || p.marked_system) return "system";
  return null;
}

function flagShown(flag: PackageFlag): boolean {
  const el = document.querySelector(`[data-flag-filter="${flag}"]`);
  return el?.getAttribute("aria-pressed") === "true";
}

function packagePassesFilters(p: PackageRow): boolean {
  const filter = ($("#filter-text") as HTMLInputElement).value.toLowerCase();
  const onlySuspicious = ($("#filter-suspicious") as HTMLInputElement).checked;
  const flag = packageFlag(p);

  if (flag && !flagShown(flag)) return false;
  if (onlySuspicious && flag !== "suspicious") return false;
  const hay = `${p.package_name} ${p.label ?? ""} ${p.author ?? ""}`.toLowerCase();
  return hay.includes(filter);
}

function getFilteredPackages(): PackageRow[] {
  return packages.filter(packagePassesFilters);
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
  const suspiciousOn = p.marked_suspicious;
  const systemOn = p.marked_system;
  const trustedOn = p.marked_trusted;
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
        <button type="button" class="btn-flag btn-flag-suspicious ${suspiciousOn ? "active" : ""}"
          data-package="${p.package_name}" title="${suspiciousOn ? "Ritira il voto sospetta" : "Vota sospetta"}">⚑</button>
        <button type="button" class="btn-flag btn-flag-system ${systemOn ? "active" : ""}"
          data-package="${p.package_name}" title="${systemOn ? "Ritira il voto sistema" : "Vota sistema"}">⚙</button>
        <button type="button" class="btn-flag btn-flag-trusted ${trustedOn ? "active" : ""}"
          data-package="${p.package_name}" title="${trustedOn ? "Ritira il voto trusted" : "Vota trusted"}">✓</button>
      </td>
    </tr>`;
}

function appendPackageRow(p: PackageRow) {
  if (!packagePassesFilters(p)) return;
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
  for (const el of document.querySelectorAll<HTMLButtonElement>("[data-flag-filter]")) {
    el.disabled = disabled;
  }
  ($("#filter-suspicious") as HTMLInputElement).disabled = disabled;
  ($("#filter-text") as HTMLInputElement).disabled = disabled;
  ($("#btn-rescan") as HTMLButtonElement).disabled = disabled;
  ($("#btn-optimize") as HTMLButtonElement).disabled = disabled;
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

  const hideSystem = !flagShown("system");
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
      serial: deviceSerial || null,
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
  const hideSystem = !flagShown("system");
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
  row.my_vote = rep.my_vote;
  row.is_reported = rep.marked_suspicious;
  const forced = row.report_count >= SUSPICIOUS_REPORT_THRESHOLD && row.marked_suspicious;
  const whitelisted = !forced && (row.marked_trusted || row.marked_system || row.is_system);
  row.is_suspicious =
    forced ||
    (!whitelisted &&
      (row.marked_suspicious || row.uninstall_count > SUSPICIOUS_UNINSTALL_THRESHOLD));
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

async function toggleMark(packageName: string, flag: "system" | "trusted" | "suspicious") {
  if (!deviceSerial) {
    toast("Collega un dispositivo prima di votare", "error");
    return;
  }
  const row = packages.find((p) => p.package_name === packageName);
  const on =
    flag === "system"
      ? row?.marked_system
      : flag === "trusted"
        ? row?.marked_trusted
        : row?.marked_suspicious;
  const next = on ? "none" : flag;
  try {
    const rep = await invoke<PackageReputation>("set_package_marks", {
      req: {
        package_name: packageName,
        device_serial: deviceSerial,
        flag: next,
      },
    });
    applyReputationUpdate(rep);
    const label =
      next === "none"
        ? "Voto ritirato"
        : flag === "system"
          ? "Voto: sistema"
          : flag === "trusted"
            ? "Voto: trusted"
            : "Voto: sospetta";
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
          : p.id === "supabase"
            ? "Condiviso"
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

function updateSettingsPanels(settings: SyncSettingsView) {
  const custom = $("#settings-custom-path")!;
  const supabase = $("#settings-supabase")!;
  const input = $("#settings-folder-input") as HTMLInputElement;
  custom.classList.toggle("hidden", settings.provider_id !== "custom");
  supabase.classList.toggle("hidden", settings.provider_id !== "supabase");
  input.value = settings.sync_folder ?? "";
  ($("#settings-supabase-url") as HTMLInputElement).value = settings.supabase_url ?? "";
  ($("#settings-supabase-key") as HTMLInputElement).value = settings.supabase_key ?? "";
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
  updateSettingsPanels(settings);
  await updateSettingsStatusText();
  ($("#settings-app-version") as HTMLElement).textContent = `Versione: ${await getVersion()}`;
  $("#dialog-settings")!.classList.remove("hidden");
}

async function applySyncProvider(providerId: string) {
  try {
    if (providerId === "custom" || providerId === "supabase") {
      updateSettingsPanels({
        provider_id: providerId,
        sync_folder: ($("#settings-folder-input") as HTMLInputElement).value || null,
        subfolder: "AndroidAdwareCleaner",
        providers: [],
        supabase_url: ($("#settings-supabase-url") as HTMLInputElement).value || null,
        supabase_key: ($("#settings-supabase-key") as HTMLInputElement).value || null,
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
    updateSettingsPanels(settings);
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

interface StorageTarget {
  path: string;
  bytes: number;
}

interface OptimizeCategory {
  id: string;
  bytes: number;
  count: number;
  estimated: boolean;
  targets: StorageTarget[];
}

interface OptimizeFile {
  path: string;
  bytes: number;
}

interface OptimizeScan {
  scan_id: number;
  categories: OptimizeCategory[];
  large_files: OptimizeFile[];
}

interface OptimizeProgressEvent {
  kind: "started" | "item" | "finished";
  total?: number;
  current?: number;
  freed_bytes?: number;
}

interface CleanResult {
  freed_bytes: number;
  trimmed_system_cache: boolean;
  errors: string[];
}

const OPTIMIZE_CATEGORIES = [
  {
    id: "system_cache",
    title: "System and user cache",
    description: "Temporary data automatically created by the system and apps.",
  },
  {
    id: "residual",
    title: "Residual files of deleted apps",
    description:
      "Empty folders, configuration files, and settings from previously deleted apps.",
  },
  {
    id: "ad_junk",
    title: "Ad junk",
    description: "Unwanted files accumulated from ads displayed within apps.",
  },
  {
    id: "apk",
    title: "APK files of installed apps",
    description: "Program archives left after installation that are no longer needed.",
  },
  {
    id: "large_files",
    title: "Large files",
    description: "Identifies and removes files occupying significant space.",
  },
  {
    id: "app_cache",
    title: "App cache",
    description:
      "Accumulated temporary data from games and apps, such as image thumbnails and activity logs.",
  },
];

let optimizeToken = 0;
let optimizeScan: OptimizeScan | null = null;
let optimizeChecked = new Set<string>();
let optimizeLargeSelected = new Set<string>();
let optimizeLargeOpen = true;
let optimizeRunning = false;

function formatBytes(bytes: number): string {
  if (!Number.isFinite(bytes) || bytes < 1024) return `${Math.max(0, Math.round(bytes))} B`;
  const units = ["KB", "MB", "GB", "TB"];
  let value = bytes / 1024;
  let unit = 0;
  while (value >= 1024 && unit < units.length - 1) {
    value /= 1024;
    unit += 1;
  }
  return `${value.toLocaleString("it-IT", { maximumFractionDigits: 1 })} ${units[unit]}`;
}

function storageRelative(path: string): string {
  for (const root of ["/sdcard/", "/storage/emulated/0/", "/storage/self/primary/"]) {
    if (path.startsWith(root)) return path.slice(root.length);
  }
  return path;
}

function pathCovered(path: string, targets: StorageTarget[]): boolean {
  return targets.some((target) => path === target.path || path.startsWith(`${target.path}/`));
}

function optimizeSelection() {
  const scan = optimizeScan;
  const categories: string[] = [];
  const targets: StorageTarget[] = [];
  let bytes = 0;
  let count = 0;
  if (!scan) return { categories, files: [] as string[], bytes, count, canRun: false };
  for (const category of scan.categories) {
    if (!optimizeChecked.has(category.id)) continue;
    categories.push(category.id);
    if (category.estimated) {
      count += 1;
      continue;
    }
    targets.push(...category.targets);
    bytes += category.bytes;
    count += category.count;
  }
  const files: string[] = [];
  for (const file of scan.large_files) {
    if (!optimizeLargeSelected.has(file.path)) continue;
    files.push(file.path);
    if (pathCovered(file.path, targets)) continue;
    bytes += file.bytes;
    count += 1;
  }
  return { categories, files, bytes, count, canRun: categories.length + files.length > 0 };
}

function recoverableBytes(scan: OptimizeScan): number {
  const targets = scan.categories.filter((category) => !category.estimated).flatMap((category) => category.targets);
  let bytes = scan.categories
    .filter((category) => !category.estimated)
    .reduce((sum, category) => sum + category.bytes, 0);
  for (const file of scan.large_files) {
    if (!pathCovered(file.path, targets)) bytes += file.bytes;
  }
  return bytes;
}

function showOptimizePhase(phase: "loading" | "results" | "progress" | "result" | "error") {
  $("#optimize-loading")?.classList.toggle("hidden", phase !== "loading");
  $("#optimize-body")?.classList.toggle("hidden", phase !== "results");
  $("#optimize-progress")?.classList.toggle("hidden", phase !== "progress");
  $("#optimize-result")?.classList.toggle("hidden", phase !== "result");
  $("#optimize-error")?.classList.toggle("hidden", phase !== "error");
  const cancel = $("#btn-optimize-cancel") as HTMLButtonElement;
  const run = $("#btn-optimize-run") as HTMLButtonElement;
  cancel.disabled = phase === "progress";
  cancel.textContent = phase === "result" || phase === "error" ? "Chiudi" : "Annulla";
  run.classList.toggle("hidden", phase !== "loading" && phase !== "results" && phase !== "progress");
  if (phase !== "results") run.disabled = true;
}

function renderOptimize() {
  const scan = optimizeScan;
  const list = $("#optimize-categories");
  if (!scan || !list) return;
  const scroll = list.querySelector(".optimize-files")?.scrollTop ?? 0;
  ($("#optimize-total") as HTMLElement).textContent = `Recuperabili: ${formatBytes(recoverableBytes(scan))}`;
  const byId = new Map(scan.categories.map((category) => [category.id, category]));
  const selectedLarge = scan.large_files.filter((file) => optimizeLargeSelected.has(file.path));
  list.innerHTML = OPTIMIZE_CATEGORIES.map((info) => {
    const category = byId.get(info.id);
    const isLarge = info.id === "large_files";
    const empty = isLarge ? scan.large_files.length === 0 : !category?.estimated && (category?.count ?? 0) === 0;
    const checked = isLarge
      ? scan.large_files.length > 0 && selectedLarge.length === scan.large_files.length
      : optimizeChecked.has(info.id);
    let meta = "Niente da pulire";
    if (isLarge && scan.large_files.length > 0) {
      const bytes = selectedLarge.reduce((sum, file) => sum + file.bytes, 0);
      meta = `${selectedLarge.length} di ${scan.large_files.length} selezionati, ${formatBytes(bytes)}`;
    } else if (info.id === "system_cache" && category && category.bytes > 0) {
      meta = formatBytes(category.bytes);
    } else if (!isLarge && category && category.count > 0) {
      const unit = info.id === "apk" ? "file" : info.id === "app_cache" ? "app" : "elem.";
      meta = `${category.count} ${unit} ${formatBytes(category.bytes)}`;
    }
    const files = isLarge && optimizeLargeOpen && scan.large_files.length > 0
      ? `<div class="optimize-files">${scan.large_files
          .map((file, index) => {
            const selected = optimizeLargeSelected.has(file.path) ? " checked" : "";
            return `<label class="optimize-file"><input type="checkbox" class="optimize-file-check" data-index="${index}"${selected} /><span class="path" title="${escapeHtml(file.path).replace(/"/g, "&quot;")}">${escapeHtml(storageRelative(file.path))}</span><span>${formatBytes(file.bytes)}</span></label>`;
          })
          .join("")}</div>`
      : "";
    const toggle = isLarge && scan.large_files.length > 0
      ? `<button type="button" class="optimize-toggle" data-optimize-toggle aria-expanded="${optimizeLargeOpen}">${optimizeLargeOpen ? "▴" : "▾"}</button>`
      : "";
    return `<li class="optimize-category${empty ? " is-empty" : ""}">
      <input id="optimize-check-${info.id}" type="checkbox" class="optimize-cat" data-category="${info.id}"${checked ? " checked" : ""}${empty ? " disabled" : ""} />
      <div class="optimize-copy"><strong>${escapeHtml(info.title)}</strong><p>${escapeHtml(info.description)}</p></div>
      <div class="optimize-meta"><span>${escapeHtml(meta)}</span>${toggle}</div>
      ${files}
    </li>`;
  }).join("");
  const largeCheck = $("#optimize-check-large_files") as HTMLInputElement | null;
  if (largeCheck) {
    largeCheck.indeterminate = selectedLarge.length > 0 && selectedLarge.length < scan.large_files.length;
  }
  const files = list.querySelector(".optimize-files");
  if (files) files.scrollTop = scroll;
  const selection = optimizeSelection();
  const run = $("#btn-optimize-run") as HTMLButtonElement;
  run.disabled = !selection.canRun;
  run.textContent = selection.bytes > 0 ? `Ottimizza ${formatBytes(selection.bytes)}` : "Ottimizza";
}

function tailPath(path: string): string {
  const parts = path.split("/").filter((part) => part.length > 0);
  if (parts.length <= 3) return path;
  return `…/${parts.slice(-3).join("/")}`;
}

async function openOptimize() {
  if (operationInFlight) return;
  const token = ++optimizeToken;
  optimizeRunning = false;
  optimizeScan = null;
  optimizeChecked = new Set();
  optimizeLargeSelected = new Set();
  optimizeLargeOpen = true;
  ($("#optimize-total") as HTMLElement).textContent = "";
  ($("#optimize-error") as HTMLElement).textContent = "";
  showOptimizePhase("loading");
  $("#dialog-optimize")?.classList.remove("hidden");
  const paths = $("#optimize-loading-paths");
  const recent: { full: string; short: string }[] = [];
  if (paths) paths.replaceChildren();
  const onProgress = new Channel<{ label: string }>();
  onProgress.onmessage = (event) => {
    const short = tailPath(event.label);
    if (recent[recent.length - 1]?.short === short) return;
    recent.push({ full: event.label, short });
    if (recent.length > 3) recent.shift();
    if (!paths) return;
    paths.replaceChildren(
      ...recent.map((item) => {
        const li = document.createElement("li");
        li.textContent = item.short;
        li.title = item.full;
        return li;
      }),
    );
  };
  try {
    const scan = await invoke<OptimizeScan>("scan_storage", {
      serial: deviceSerial || null,
      on_progress: onProgress,
    });
    if (token !== optimizeToken) return;
    optimizeScan = scan;
    for (const category of scan.categories) {
      if (category.estimated || category.count > 0) optimizeChecked.add(category.id);
    }
    showOptimizePhase("results");
    renderOptimize();
  } catch (e) {
    if (token !== optimizeToken) return;
    ($("#optimize-error") as HTMLElement).textContent = `Errore: ${e}`;
    showOptimizePhase("error");
  }
}

function closeOptimize() {
  if (optimizeRunning) return;
  optimizeToken += 1;
  $("#dialog-optimize")?.classList.add("hidden");
}

async function runOptimize() {
  const scan = optimizeScan;
  const selection = optimizeSelection();
  if (!scan || !selection.canRun || optimizeRunning || operationInFlight) return;
  const size = selection.bytes > 0 ? formatBytes(selection.bytes) : "dimensione non misurabile";
  if (!confirm(`Eliminare ${selection.count} elementi, ${size}? L'operazione non è reversibile.`)) return;

  optimizeRunning = true;
  operationInFlight = true;
  setScanControlsDisabled(true);
  showOptimizePhase("progress");
  ($("#optimize-progress-fill") as HTMLElement).style.width = "0%";
  ($("#optimize-progress-text") as HTMLElement).textContent = "Pulizia…";

  const onProgress = new Channel<OptimizeProgressEvent>();
  onProgress.onmessage = (event) => {
    if (event.kind !== "started" && event.kind !== "item") return;
    const current = event.kind === "item" ? event.current ?? 0 : 0;
    const total = event.total ?? 0;
    const pct = total > 0 ? Math.min(100, Math.round((current / total) * 100)) : 0;
    ($("#optimize-progress-fill") as HTMLElement).style.width = `${pct}%`;
    ($("#optimize-progress-text") as HTMLElement).textContent = `Pulizia… ${current} / ${total}`;
  };

  try {
    const result = await invoke<CleanResult>("clean_storage", {
      req: {
        scan_id: scan.scan_id,
        categories: selection.categories,
        large_files: selection.files,
        serial: deviceSerial || null,
      },
      on_progress: onProgress,
    });
    const summary = result.trimmed_system_cache && result.freed_bytes === 0
      ? "Cache di sistema ridotta. Lo spazio liberato non è misurabile."
      : result.trimmed_system_cache
        ? `Liberati ${formatBytes(result.freed_bytes)}. Cache di sistema ridotta.`
        : `Liberati ${formatBytes(result.freed_bytes)}.`;
    ($("#optimize-result-text") as HTMLElement).textContent = summary;
    const errors = $("#optimize-errors")!;
    const shown = result.errors.slice(0, 8);
    errors.innerHTML = shown.map((error) => `<li>${escapeHtml(error)}</li>`).join("");
    if (result.errors.length > shown.length) {
      errors.insertAdjacentHTML("beforeend", `<li>e altri ${result.errors.length - shown.length}</li>`);
    }
    showOptimizePhase("result");
    toast(summary, result.errors.length ? "warn" : "success");
  } catch (e) {
    ($("#optimize-error") as HTMLElement).textContent = `Errore: ${e}`;
    showOptimizePhase("error");
    toast(`Errore pulizia: ${e}`, "error");
  } finally {
    optimizeRunning = false;
    operationInFlight = false;
    setScanControlsDisabled(false);
  }
}

window.addEventListener("DOMContentLoaded", () => {
  installIconErrorFallback();
  if (__FULLSCAN__) {
    for (const el of document.querySelectorAll("[data-flag-filter]")) {
      el.setAttribute("aria-pressed", "true");
    }
    ($("#filter-suspicious") as HTMLInputElement).checked = false;
  }
  void listen("sync-finished", () => {
    void refreshDbPanel();
  });
  boot();

  $("#conn-brand")?.addEventListener("change", () => void renderConnectionGuideInDialog());
  $("#conn-model")?.addEventListener("input", scheduleGuideRefresh);

  $("#btn-sync-full")?.addEventListener("click", () => void runSync(true));

  $("#btn-settings")?.addEventListener("click", () => void openSettings());

  $("#btn-settings-close")?.addEventListener("click", closeSettings);

  $("#btn-settings-check-update")?.addEventListener("click", () => void checkForAppUpdates(false));

  $("#btn-settings-clear-cache")?.addEventListener("click", async () => {
    if (
      !confirm(
        "Cancellare la cache di nomi e icone? La prossima scansione li rileggerà dal telefono."
      )
    ) {
      return;
    }
    try {
      await invoke("clear_metadata_cache");
      toast("Cache scansione cancellata", "success");
    } catch (e) {
      toast(String(e), "error");
    }
  });

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

  $("#btn-settings-supabase")?.addEventListener("click", async () => {
    const url = ($("#settings-supabase-url") as HTMLInputElement).value;
    const key = ($("#settings-supabase-key") as HTMLInputElement).value;
    try {
      await invoke("set_supabase_config", { url, key });
      toast("Supabase collegato", "success");
      await refreshDbPanel();
      const settings = await invoke<SyncSettingsView>("get_sync_settings");
      renderSettingsProviders(settings);
      updateSettingsPanels(settings);
      await updateSettingsStatusText();
    } catch (err) {
      toast(String(err), "error");
    }
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
      updateSettingsPanels(settings);
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
  $("#btn-optimize")?.addEventListener("click", () => void openOptimize());
  $("#btn-optimize-cancel")?.addEventListener("click", closeOptimize);
  $("#btn-optimize-run")?.addEventListener("click", () => void runOptimize());
  $("#dialog-optimize")?.addEventListener("click", (e) => {
    if (e.target === $("#dialog-optimize")) closeOptimize();
  });
  $("#optimize-categories")?.addEventListener("change", (e) => {
    const input = e.target as HTMLInputElement;
    if (!optimizeScan) return;
    if (input.classList.contains("optimize-cat")) {
      const id = input.dataset.category ?? "";
      if (id === "large_files") {
        optimizeLargeSelected = input.checked
          ? new Set(optimizeScan.large_files.map((file) => file.path))
          : new Set();
      } else if (input.checked) optimizeChecked.add(id);
      else optimizeChecked.delete(id);
      renderOptimize();
    }
    if (input.classList.contains("optimize-file-check")) {
      const file = optimizeScan.large_files[Number(input.dataset.index)];
      if (!file) return;
      if (input.checked) optimizeLargeSelected.add(file.path);
      else optimizeLargeSelected.delete(file.path);
      renderOptimize();
    }
  });
  $("#optimize-categories")?.addEventListener("click", (e) => {
    const toggle = (e.target as HTMLElement).closest("[data-optimize-toggle]");
    if (!toggle) return;
    optimizeLargeOpen = !optimizeLargeOpen;
    renderOptimize();
  });
  $("#btn-uninstall")?.addEventListener("click", bulkUninstall);
  $("#btn-export")?.addEventListener("click", () => {
    exportReport(confirm("OK = CSV, Annulla = PDF") ? "csv" : "pdf");
  });

  $("#filter-text")?.addEventListener("input", () => {
    updateScanInfo();
    renderTable();
  });
  $("#filter-suspicious")?.addEventListener("change", () => {
    updateScanInfo();
    renderTable();
  });
  document.querySelector(".flag-legend")?.addEventListener("click", (e) => {
    const btn = (e.target as HTMLElement).closest<HTMLButtonElement>("[data-flag-filter]");
    if (!btn || btn.disabled) return;
    btn.setAttribute("aria-pressed", btn.getAttribute("aria-pressed") === "true" ? "false" : "true");
    if (btn.dataset.flagFilter === "system") onHideSystemChange();
    else {
      updateScanInfo();
      renderTable();
    }
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
    if (t.classList.contains("btn-flag-suspicious")) toggleMark(pkg, "suspicious");
    if (t.classList.contains("btn-flag-system")) toggleMark(pkg, "system");
    if (t.classList.contains("btn-flag-trusted")) toggleMark(pkg, "trusted");
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
