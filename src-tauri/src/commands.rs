use crate::state::AppState;
use adb_bridge::{AdbBridge, AdbDevice};
use package_scanner::ScannedPackage;
use reputation_db::PackageReputation;
use serde::{Deserialize, Serialize};
use system_setup::{load_guides, resolve_guide, DeviceGuides, GuideStep};
use tauri::ipc::Channel;
use tauri::State;
use uninstall_engine::{UninstallEngine, UninstallResult};

pub const SUSPICIOUS_THRESHOLD: u64 = reputation_db::SUSPICIOUS_UNINSTALL_THRESHOLD;

#[derive(Debug, Clone, Serialize)]
pub struct PackageRow {
    pub package_name: String,
    pub label: Option<String>,
    pub author: Option<String>,
    pub icon_url: Option<String>,
    pub is_system: bool,
    pub installer: Option<String>,
    pub is_device_admin: bool,
    pub uninstall_count: u64,
    pub report_count: u64,
    pub marked_system: bool,
    pub marked_trusted: bool,
    pub is_suspicious: bool,
    pub is_reported: bool,
}

#[derive(Debug, Serialize)]
pub struct AdbStatus {
    pub adb_path: Option<String>,
    pub devices: Vec<AdbDevice>,
    pub has_authorized_device: bool,
}

#[derive(Debug, Serialize)]
pub struct ConnectionGuide {
    pub brand: Option<String>,
    pub model: Option<String>,
    pub resolved_brand: Option<String>,
    pub device_unusable: bool,
    pub steps: Vec<GuideStep>,
    pub tips: Vec<GuideStep>,
    pub safe_mode_steps: Option<String>,
}

#[tauri::command(rename_all = "snake_case")]
pub fn check_setup() -> system_setup::SetupStatus {
    system_setup::check_setup()
}

#[tauri::command(rename_all = "snake_case")]
pub fn run_setup(state: State<'_, AppState>) -> Result<system_setup::SetupStatus, String> {
    let install_dir = state.platform_tools_dir.clone();
    let status = system_setup::run_setup(&install_dir)?;
    std::env::set_var(
        "ANDROID_ADWARE_PLATFORM_TOOLS",
        install_dir.to_string_lossy().as_ref(),
    );
    Ok(status)
}

#[tauri::command(rename_all = "snake_case")]
pub fn get_device_guides() -> DeviceGuides {
    load_guides()
}

#[tauri::command(rename_all = "snake_case")]
pub fn get_connection_guide(
    brand: Option<String>,
    model: Option<String>,
    device_unusable: bool,
) -> ConnectionGuide {
    let guides = load_guides();
    let brand_ref = brand.as_deref().unwrap_or("").trim();
    let model_ref = model.as_deref().unwrap_or("").trim();

    let matched = resolve_guide(&guides, brand_ref, model_ref);
    let resolved_brand = matched.map(|g| g.brand.clone());

    let steps: Vec<GuideStep> = if let Some(g) = matched {
        g.steps.clone()
    } else {
        guides.generic.clone()
    };

    let mut tips: Vec<GuideStep> = if let Some(g) = matched {
        g.tips.clone()
    } else {
        guides.generic_tips.clone()
    };

    if device_unusable {
        let has_safe_tip = tips.iter().any(|s| s.phase == "safe_mode");
        if !has_safe_tip {
            let safe_body = matched
                .map(|g| g.safe_mode_steps.clone())
                .unwrap_or_else(|| {
                    "Tieni premuto «Spegni» nel menu power finché appare Safe mode, poi selezionalo."
                        .into()
                });
            tips.push(GuideStep {
                title: "Safe Mode (se il telefono è inutilizzabile)".into(),
                body: safe_body,
                phase: "safe_mode".into(),
            });
        }
    } else {
        tips.retain(|s| s.phase != "safe_mode");
    }

    let safe_mode_steps = matched.map(|g| g.safe_mode_steps.clone());

    ConnectionGuide {
        brand,
        model,
        resolved_brand,
        device_unusable,
        steps,
        tips,
        safe_mode_steps,
    }
}

#[tauri::command(rename_all = "snake_case")]
pub fn set_custom_adb_path(
    state: State<'_, AppState>,
    path: Option<String>,
) -> Result<system_setup::SetupStatus, String> {
    {
        let mut config = state.config.lock().map_err(|e| e.to_string())?;
        config.custom_adb_path = path.clone();
    }
    state.save_config()?;
    if let Some(ref p) = path {
        std::env::set_var("ANDROID_ADWARE_CUSTOM_ADB", p);
    } else {
        std::env::remove_var("ANDROID_ADWARE_CUSTOM_ADB");
    }
    Ok(system_setup::check_setup())
}

#[tauri::command(rename_all = "snake_case")]
pub fn get_adb_status() -> Result<AdbStatus, String> {
    let adb_path = adb_bridge::resolve_adb_path().map(|p| p.to_string_lossy().into_owned());
    let devices = match AdbBridge::new() {
        Ok(b) => b.list_devices().unwrap_or_default(),
        Err(_) => vec![],
    };
    let has_authorized_device = devices.iter().any(|d| d.state == adb_bridge::DeviceState::Device);
    Ok(AdbStatus {
        adb_path,
        devices,
        has_authorized_device,
    })
}

#[tauri::command(rename_all = "snake_case")]
pub fn list_devices() -> Result<Vec<AdbDevice>, String> {
    AdbBridge::new()
        .map_err(|e| e.to_string())?
        .list_devices()
        .map_err(|e| e.to_string())
}

#[tauri::command(rename_all = "snake_case")]
pub fn sync_pull(state: State<'_, AppState>) -> Result<cloud_sync::SyncStatus, String> {
    let mut sync = state.cloud_sync.lock().map_err(|e| e.to_string())?;
    let db = state.db.lock().map_err(|e| e.to_string())?;
    sync.pull(&db).map_err(|e| e.to_string())?;
    Ok(sync.status())
}

#[tauri::command(rename_all = "snake_case")]
pub fn sync_push(state: State<'_, AppState>) -> Result<cloud_sync::SyncStatus, String> {
    let mut sync = state.cloud_sync.lock().map_err(|e| e.to_string())?;
    let db = state.db.lock().map_err(|e| e.to_string())?;
    sync.push(&db).map_err(|e| e.to_string())?;
    Ok(sync.status())
}

#[derive(Clone, Serialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum ScanProgressEvent {
    Started {
        total: usize,
        device_serial: String,
        device_model: Option<String>,
        device_brand: Option<String>,
    },
    Package {
        row: PackageRow,
    },
    Finished,
}

#[derive(Clone, Serialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum UninstallProgressEvent {
    Started { total: usize },
    Item {
        current: usize,
        total: usize,
        package_name: String,
        status: String,
        message: String,
    },
    Finished,
}

fn scan_packages_streaming(
    user_only: bool,
    serial: Option<String>,
    reputations: std::collections::HashMap<String, PackageReputation>,
    on_progress: Channel<ScanProgressEvent>,
) -> Result<(), String> {
    let bridge = match serial {
        Some(s) => AdbBridge::with_serial(s).map_err(|e| e.to_string())?,
        None => AdbBridge::new().map_err(|e| e.to_string())?,
    };
    let device_serial = bridge.get_serial().map_err(|e| e.to_string())?;
    let device_model = bridge.get_device_property("ro.product.model").ok();
    let device_brand = bridge.get_device_property("ro.product.brand").ok();

    let scanner = package_scanner::PackageScanner::new(bridge);
    scanner
        .scan_with_progress(user_only, |event| match event {
            package_scanner::ScanProgress::Started { total } => {
                let _ = on_progress.send(ScanProgressEvent::Started {
                    total,
                    device_serial: device_serial.clone(),
                    device_model: device_model.clone(),
                    device_brand: device_brand.clone(),
                });
            }
            package_scanner::ScanProgress::Package(pkg) => {
                let row = package_to_row(pkg, &reputations);
                let _ = on_progress.send(ScanProgressEvent::Package { row });
            }
        })
        .map_err(|e| e.to_string())?;

    let _ = on_progress.send(ScanProgressEvent::Finished);
    Ok(())
}

#[tauri::command(rename_all = "snake_case")]
pub async fn scan_packages(
    state: State<'_, AppState>,
    user_only: bool,
    serial: Option<String>,
    on_progress: Channel<ScanProgressEvent>,
) -> Result<(), String> {
    let reputations: std::collections::HashMap<String, PackageReputation> = {
        let db = state.db.lock().map_err(|e| e.to_string())?;
        db.all_reputations()
            .unwrap_or_default()
            .into_iter()
            .map(|r| (r.package_name.clone(), r))
            .collect()
    };

    tauri::async_runtime::spawn_blocking(move || {
        scan_packages_streaming(user_only, serial, reputations, on_progress)
    })
    .await
    .map_err(|e| format!("scansione interrotta: {e}"))??;

    Ok(())
}

fn package_to_row(
    p: ScannedPackage,
    reputations: &std::collections::HashMap<String, PackageReputation>,
) -> PackageRow {
    let rep = reputations.get(&p.package_name);
    let uninstall_count = rep.map(|r| r.uninstall_count).unwrap_or(0);
    let report_count = rep.map(|r| r.report_count).unwrap_or(0);
    let marked_system = rep.map(|r| r.marked_system).unwrap_or(false);
    let marked_trusted = rep.map(|r| r.marked_trusted).unwrap_or(false);
    let is_reported = report_count > 0;
    let is_whitelisted = marked_trusted || marked_system || p.is_system;
    let is_suspicious =
        !is_whitelisted && (is_reported || uninstall_count > SUSPICIOUS_THRESHOLD);

    PackageRow {
        package_name: p.package_name,
        label: p.label,
        author: p.author,
        icon_url: p.icon_url,
        is_system: p.is_system,
        installer: p.installer,
        is_device_admin: p.is_device_admin,
        uninstall_count,
        report_count,
        marked_system,
        marked_trusted,
        is_suspicious,
        is_reported,
    }
}

#[derive(Debug, Deserialize)]
pub struct UninstallRequest {
    pub packages: Vec<String>,
    pub dry_run: bool,
    pub serial: Option<String>,
}

#[tauri::command(rename_all = "snake_case")]
pub async fn bulk_uninstall(
    state: State<'_, AppState>,
    req: UninstallRequest,
    on_progress: Channel<UninstallProgressEvent>,
) -> Result<(), String> {
    let packages = req.packages.clone();
    let dry_run = req.dry_run;
    let serial = req.serial.clone();
    let total = packages.len();
    let on_progress_worker = on_progress.clone();

    let results = tauri::async_runtime::spawn_blocking(move || {
        let _ = on_progress_worker.send(UninstallProgressEvent::Started { total });

        let bridge = match serial {
            Some(s) => AdbBridge::with_serial(s).map_err(|e| e.to_string())?,
            None => AdbBridge::new().map_err(|e| e.to_string())?,
        };
        let device_serial = bridge.get_serial().map_err(|e| e.to_string())?;
        let engine = UninstallEngine::new(bridge);
        let results = engine
            .bulk_uninstall_with_progress(&packages, dry_run, |current, total, result| {
                let status = match result.status {
                    uninstall_engine::UninstallStatus::Success => "success",
                    uninstall_engine::UninstallStatus::Failed => "failed",
                    uninstall_engine::UninstallStatus::Skipped => "skipped",
                };
                let _ = on_progress_worker.send(UninstallProgressEvent::Item {
                    current,
                    total,
                    package_name: result.package_name.clone(),
                    status: status.into(),
                    message: result.message.clone(),
                });
            })
            .map_err(|e| e.to_string())?;

        Ok::<(Vec<UninstallResult>, String), String>((results, device_serial))
    })
    .await
    .map_err(|e| format!("disinstallazione interrotta: {e}"))??;

    let (results, device_serial) = results;

    if !dry_run {
        let db = state.db.lock().map_err(|e| e.to_string())?;
        for r in &results {
            if r.status == uninstall_engine::UninstallStatus::Success {
                let _ = db.record_uninstall(
                    &r.package_name,
                    &device_serial,
                    true,
                    Some(&r.message),
                );
            }
        }
        drop(db);
        push_db(&state)?;
    }

    let _ = on_progress.send(UninstallProgressEvent::Finished);
    Ok(())
}

#[tauri::command(rename_all = "snake_case")]
pub fn report_package(
    state: State<'_, AppState>,
    package_name: String,
    reason: Option<String>,
    serial: Option<String>,
) -> Result<PackageReputation, String> {
    let bridge = match serial {
        Some(s) => AdbBridge::with_serial(s).map_err(|e| e.to_string())?,
        None => AdbBridge::new().map_err(|e| e.to_string())?,
    };
    let device_serial = bridge.get_serial().map_err(|e| e.to_string())?;
    let db = state.db.lock().map_err(|e| e.to_string())?;
    db.record_report(&package_name, &device_serial, reason.as_deref())
        .map_err(|e| e.to_string())?;
    let rep = db
        .get_reputation(&package_name)
        .map_err(|e| e.to_string())?;
    drop(db);
    push_db(&state)?;
    Ok(rep)
}

#[derive(Debug, Deserialize)]
pub struct SetPackageMarksRequest {
    pub package_name: String,
    pub marked_system: Option<bool>,
    pub marked_trusted: Option<bool>,
}

#[tauri::command(rename_all = "snake_case")]
pub fn set_package_marks(
    state: State<'_, AppState>,
    req: SetPackageMarksRequest,
) -> Result<PackageReputation, String> {
    if req.marked_system.is_none() && req.marked_trusted.is_none() {
        return Err("specificare marked_system o marked_trusted".into());
    }
    let db = state.db.lock().map_err(|e| e.to_string())?;
    if let Some(value) = req.marked_system {
        db.set_marked_system(&req.package_name, value)
            .map_err(|e| e.to_string())?;
    }
    if let Some(value) = req.marked_trusted {
        db.set_marked_trusted(&req.package_name, value)
            .map_err(|e| e.to_string())?;
    }
    let rep = db
        .get_reputation(&req.package_name)
        .map_err(|e| e.to_string())?;
    drop(db);
    push_db(&state)?;
    Ok(rep)
}

fn push_db(state: &State<'_, AppState>) -> Result<(), String> {
    let mut sync = state.cloud_sync.lock().map_err(|e| e.to_string())?;
    if !sync.status().configured {
        return Ok(());
    }
    let db = state.db.lock().map_err(|e| e.to_string())?;
    sync.push(&db).map_err(|e| e.to_string())?;
    Ok(())
}

#[tauri::command(rename_all = "snake_case")]
pub fn set_airplane_mode(enabled: bool, serial: Option<String>) -> Result<String, String> {
    let bridge = match serial {
        Some(s) => AdbBridge::with_serial(s).map_err(|e| e.to_string())?,
        None => AdbBridge::new().map_err(|e| e.to_string())?,
    };
    bridge
        .set_airplane_mode(enabled)
        .map_err(|e| e.to_string())
}

#[tauri::command(rename_all = "snake_case")]
pub fn get_sync_status(state: State<'_, AppState>) -> Result<cloud_sync::SyncStatus, String> {
    let sync = state.cloud_sync.lock().map_err(|e| e.to_string())?;
    Ok(sync.status())
}

#[tauri::command(rename_all = "snake_case")]
pub fn get_sync_settings(state: State<'_, AppState>) -> Result<cloud_sync::SyncSettingsView, String> {
    let config = state.config.lock().map_err(|e| e.to_string())?;
    let sync = state.cloud_sync.lock().map_err(|e| e.to_string())?;
    let provider_id = config
        .sync_provider
        .clone()
        .unwrap_or_else(|| "local".into());
    Ok(cloud_sync::SyncSettingsView {
        provider_id,
        sync_folder: config.sync_folder.clone().or_else(|| {
            sync.sync_path().and_then(|p| {
                p.parent()
                    .map(|d| d.to_string_lossy().into_owned())
            })
        }),
        subfolder: cloud_sync::SYNC_SUBFOLDER.into(),
        providers: cloud_sync::list_providers(),
    })
}

fn apply_sync_folder(state: &AppState, provider_id: &str, folder: Option<String>) -> Result<(), String> {
    {
        let mut config = state.config.lock().map_err(|e| e.to_string())?;
        config.sync_provider = Some(provider_id.into());
        config.sync_folder = folder.clone();
    }
    state.save_config()?;
    let mut sync = state.cloud_sync.lock().map_err(|e| e.to_string())?;
    sync.set_sync_dir(folder.map(std::path::PathBuf::from));
    let db = state.db.lock().map_err(|e| e.to_string())?;
    let _ = sync.pull(&db);
    Ok(())
}

#[tauri::command(rename_all = "snake_case")]
pub fn set_sync_provider(state: State<'_, AppState>, provider_id: String) -> Result<cloud_sync::SyncStatus, String> {
    if provider_id == "local" {
        apply_sync_folder(&state, "local", None)?;
        let sync = state.cloud_sync.lock().map_err(|e| e.to_string())?;
        return Ok(sync.status());
    }
    if provider_id == "custom" {
        return Err("usa set_sync_folder per una cartella personalizzata".into());
    }
    let folder = cloud_sync::resolve_sync_folder(&provider_id, None).ok_or_else(|| {
        format!(
            "cartella {} non trovata — installa il client sul PC o usa «Cartella personalizzata»",
            provider_id
        )
    })?;
    cloud_sync::ensure_sync_folder(&folder).map_err(|e| e.to_string())?;
    let folder_str = folder.to_string_lossy().into_owned();
    apply_sync_folder(&state, &provider_id, Some(folder_str))?;
    let sync = state.cloud_sync.lock().map_err(|e| e.to_string())?;
    Ok(sync.status())
}

#[tauri::command(rename_all = "snake_case")]
pub fn set_sync_folder(state: State<'_, AppState>, folder: Option<String>) -> Result<(), String> {
    if let Some(ref path) = folder {
        cloud_sync::ensure_sync_folder(std::path::Path::new(path)).map_err(|e| e.to_string())?;
    }
    apply_sync_folder(&state, "custom", folder)
}

#[tauri::command(rename_all = "snake_case")]
pub fn sync_now(state: State<'_, AppState>) -> Result<cloud_sync::SyncStatus, String> {
    let mut sync = state.cloud_sync.lock().map_err(|e| e.to_string())?;
    let db = state.db.lock().map_err(|e| e.to_string())?;
    sync.pull(&db).map_err(|e| e.to_string())?;
    sync.push(&db).map_err(|e| e.to_string())?;
    Ok(sync.status())
}

#[derive(Debug, Deserialize)]
pub struct ExportReportRequest {
    pub device_serial: String,
    pub imei: Option<String>,
    pub removed_packages: Vec<String>,
    pub output_path: String,
    pub format: String,
}

#[tauri::command(rename_all = "snake_case")]
pub fn export_report(req: ExportReportRequest) -> Result<String, String> {
    let timestamp = chrono::Utc::now().format("%Y-%m-%d %H:%M:%S UTC");
    match req.format.as_str() {
        "csv" => export_csv(&req, &timestamp.to_string()),
        "pdf" => export_pdf(&req, &timestamp.to_string()),
        _ => Err("format must be csv or pdf".into()),
    }
}

fn export_csv(req: &ExportReportRequest, timestamp: &str) -> Result<String, String> {
    let mut lines = vec![
        "field,value".to_string(),
        format!("timestamp,{timestamp}"),
        format!("device_serial,{}", req.device_serial),
        format!("imei,{}", req.imei.as_deref().unwrap_or("")),
        format!("removed_count,{}", req.removed_packages.len()),
    ];
    for (i, pkg) in req.removed_packages.iter().enumerate() {
        lines.push(format!("removed_{i},{pkg}"));
    }
    std::fs::write(&req.output_path, lines.join("\n")).map_err(|e| e.to_string())?;
    Ok(req.output_path.clone())
}

fn export_pdf(req: &ExportReportRequest, timestamp: &str) -> Result<String, String> {
    let content = format!(
        "AndroidAdwareCleaner Work Report\n\nTimestamp: {timestamp}\nDevice serial: {}\nIMEI: {}\nPackages removed: {}\n\nRemoved:\n{}",
        req.device_serial,
        req.imei.as_deref().unwrap_or("N/A"),
        req.removed_packages.len(),
        req.removed_packages.join("\n")
    );
    let pdf = build_simple_pdf(&content);
    std::fs::write(&req.output_path, pdf).map_err(|e| e.to_string())?;
    Ok(req.output_path.clone())
}

fn build_simple_pdf(text: &str) -> Vec<u8> {
    let escaped: String = text
        .chars()
        .map(|c| match c {
            '(' | ')' | '\\' => format!("\\{c}"),
            '\n' => "\\n".to_string(),
            _ => c.to_string(),
        })
        .collect();
    let stream = format!("BT /F1 10 Tf 50 750 Td ({escaped}) Tj ET");
    let stream_len = stream.len();
    let pdf = format!(
        "%PDF-1.4\n\
        1 0 obj<</Type/Catalog/Pages 2 0 R>>endobj\n\
        2 0 obj<</Type/Pages/Kids[3 0 R]/Count 1>>endobj\n\
        3 0 obj<</Type/Page/Parent 2 0 R/MediaBox[0 0 612 792]/Contents 4 0 R/Resources<</Font<</F1 5 0 R>>>>>>endobj\n\
        4 0 obj<</Length {stream_len}>>stream\n{stream}\nendstream endobj\n\
        5 0 obj<</Type/Font/Subtype/Type1/BaseFont/Helvetica>>endobj\n\
        xref\n0 6\n0000000000 65535 f \n0000000009 00000 n \n0000000058 00000 n \n0000000115 00000 n \n0000000266 00000 n \n0000000{} 00000 n \n\
        trailer<</Size 6/Root 1 0 R>>\nstartxref\n{}\n%%EOF",
        350 + stream_len,
        350 + stream_len
    );
    pdf.into_bytes()
}
