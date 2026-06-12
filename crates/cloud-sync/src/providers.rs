use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};

pub const SYNC_SUBFOLDER: &str = "AndroidAdwareCleaner";

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum SyncProviderId {
    Local,
    GoogleDrive,
    Icloud,
    Onedrive,
    Dropbox,
    Custom,
}

impl SyncProviderId {
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::Local => "local",
            Self::GoogleDrive => "google_drive",
            Self::Icloud => "icloud",
            Self::Onedrive => "onedrive",
            Self::Dropbox => "dropbox",
            Self::Custom => "custom",
        }
    }

    pub fn from_str_id(s: &str) -> Self {
        match s {
            "google_drive" => Self::GoogleDrive,
            "icloud" => Self::Icloud,
            "onedrive" => Self::Onedrive,
            "dropbox" => Self::Dropbox,
            "custom" => Self::Custom,
            _ => Self::Local,
        }
    }
}

#[derive(Debug, Clone, Serialize)]
pub struct SyncProviderInfo {
    pub id: String,
    pub name: String,
    pub description: String,
    pub detected_root: Option<String>,
    pub sync_folder: Option<String>,
    pub available: bool,
}

#[derive(Debug, Clone, Serialize)]
pub struct SyncSettingsView {
    pub provider_id: String,
    pub sync_folder: Option<String>,
    pub subfolder: String,
    pub providers: Vec<SyncProviderInfo>,
}

pub fn list_providers() -> Vec<SyncProviderInfo> {
    vec![
        provider_local(),
        provider_google_drive(),
        provider_icloud(),
        provider_onedrive(),
        provider_dropbox(),
        provider_custom(),
    ]
}

fn provider_local() -> SyncProviderInfo {
    SyncProviderInfo {
        id: SyncProviderId::Local.as_str().into(),
        name: "Solo locale".into(),
        description: "Nessuna sincronizzazione cloud — database solo su questo PC.".into(),
        detected_root: None,
        sync_folder: None,
        available: true,
    }
}

fn provider_custom() -> SyncProviderInfo {
    SyncProviderInfo {
        id: SyncProviderId::Custom.as_str().into(),
        name: "Cartella personalizzata".into(),
        description: "Scegli manualmente una cartella (anche su NAS o altro cloud).".into(),
        detected_root: None,
        sync_folder: None,
        available: true,
    }
}

fn provider_from_cloud(
    id: SyncProviderId,
    name: &str,
    description: &str,
    root: Option<PathBuf>,
) -> SyncProviderInfo {
    let sync_folder = root.as_ref().map(|r| r.join(SYNC_SUBFOLDER));
    SyncProviderInfo {
        id: id.as_str().into(),
        name: name.into(),
        description: description.into(),
        detected_root: root.as_ref().map(|p| path_to_string(p.clone())),
        sync_folder: sync_folder.as_ref().map(|p| path_to_string(p.clone())),
        available: root_exists(&root),
    }
}

fn provider_google_drive() -> SyncProviderInfo {
    provider_from_cloud(
        SyncProviderId::GoogleDrive,
        "Google Drive",
        "Usa la cartella Google Drive installata sul PC (app desktop).",
        detect_google_drive_root(),
    )
}

fn provider_icloud() -> SyncProviderInfo {
    provider_from_cloud(
        SyncProviderId::Icloud,
        "iCloud Drive",
        "Usa iCloud Drive su macOS / Windows (iCloud per Windows).",
        detect_icloud_root(),
    )
}

fn provider_onedrive() -> SyncProviderInfo {
    provider_from_cloud(
        SyncProviderId::Onedrive,
        "OneDrive",
        "Usa la cartella OneDrive sincronizzata sul PC.",
        detect_onedrive_root(),
    )
}

fn provider_dropbox() -> SyncProviderInfo {
    provider_from_cloud(
        SyncProviderId::Dropbox,
        "Dropbox",
        "Usa la cartella Dropbox sul PC.",
        detect_dropbox_root(),
    )
}

fn root_exists(root: &Option<PathBuf>) -> bool {
    root.as_ref().is_some_and(|p| p.is_dir())
}

pub fn resolve_sync_folder(provider_id: &str, custom_folder: Option<&str>) -> Option<PathBuf> {
    match SyncProviderId::from_str_id(provider_id) {
        SyncProviderId::Local => None,
        SyncProviderId::Custom => custom_folder.map(PathBuf::from),
        SyncProviderId::GoogleDrive => detect_google_drive_root().map(|p| p.join(SYNC_SUBFOLDER)),
        SyncProviderId::Icloud => detect_icloud_root().map(|p| p.join(SYNC_SUBFOLDER)),
        SyncProviderId::Onedrive => detect_onedrive_root().map(|p| p.join(SYNC_SUBFOLDER)),
        SyncProviderId::Dropbox => detect_dropbox_root().map(|p| p.join(SYNC_SUBFOLDER)),
    }
}

pub fn ensure_sync_folder(path: &Path) -> std::io::Result<()> {
    std::fs::create_dir_all(path)
}

fn home() -> Option<PathBuf> {
    dirs::home_dir()
}

fn path_to_string(p: PathBuf) -> String {
    p.to_string_lossy().into_owned()
}

fn find_in_cloud_storage(name_prefix: &str) -> Option<PathBuf> {
    let base = home()?.join("Library/CloudStorage");
    read_dir_match_prefix(&base, name_prefix)
}

fn read_dir_match_prefix(base: &Path, name_prefix: &str) -> Option<PathBuf> {
    if !base.is_dir() {
        return None;
    }
    let mut matches: Vec<PathBuf> = std::fs::read_dir(base)
        .ok()?
        .filter_map(|e| e.ok())
        .map(|e| e.path())
        .filter(|p| {
            p.file_name()
                .and_then(|n| n.to_str())
                .map(|n| n.starts_with(name_prefix))
                .unwrap_or(false)
        })
        .collect();
    matches.sort();
    matches.into_iter().next()
}

fn first_existing(paths: &[PathBuf]) -> Option<PathBuf> {
    paths.iter().find(|p| p.is_dir()).cloned()
}

fn detect_google_drive_root() -> Option<PathBuf> {
    let home = home()?;
    let mut candidates = vec![
        home.join("Google Drive"),
        home.join("GoogleDrive"),
        home.join("My Drive"),
    ];
    if let Some(p) = find_in_cloud_storage("Google Drive") {
        candidates.insert(0, p);
    }
    if let Some(p) = find_in_cloud_storage("GoogleDrive") {
        candidates.insert(0, p);
    }
    #[cfg(target_os = "windows")]
    {
        if let Ok(profile) = std::env::var("USERPROFILE") {
            candidates.push(PathBuf::from(&profile).join("Google Drive"));
            candidates.push(PathBuf::from(&profile).join("My Drive"));
        }
    }
    first_existing(&candidates)
}

fn detect_icloud_root() -> Option<PathBuf> {
    let home = home()?;
    let mut candidates = vec![home.join(
        "Library/Mobile Documents/com~apple~CloudDocs",
    )];
    if let Some(p) = find_in_cloud_storage("iCloud") {
        candidates.insert(0, p);
    }
    #[cfg(target_os = "windows")]
    {
        if let Ok(profile) = std::env::var("USERPROFILE") {
            candidates.push(PathBuf::from(profile).join("iCloudDrive"));
        }
    }
    first_existing(&candidates)
}

fn detect_onedrive_root() -> Option<PathBuf> {
    let home = home()?;
    let mut candidates = vec![home.join("OneDrive")];
    if let Some(p) = find_in_cloud_storage("OneDrive") {
        candidates.insert(0, p);
    }
    #[cfg(target_os = "windows")]
    {
        if let Ok(profile) = std::env::var("USERPROFILE") {
            let p = PathBuf::from(&profile);
            candidates.push(p.join("OneDrive"));
            if let Ok(entries) = std::fs::read_dir(&p) {
                for entry in entries.flatten() {
                    let name = entry.file_name().to_string_lossy().into_owned();
                    if name.starts_with("OneDrive") {
                        candidates.push(entry.path());
                    }
                }
            }
        }
    }
    first_existing(&candidates)
}

fn detect_dropbox_root() -> Option<PathBuf> {
    let home = home()?;
    let mut candidates = vec![home.join("Dropbox")];
    if let Some(p) = find_in_cloud_storage("Dropbox") {
        candidates.insert(0, p);
    }
    #[cfg(target_os = "windows")]
    {
        if let Ok(profile) = std::env::var("USERPROFILE") {
            candidates.push(PathBuf::from(profile).join("Dropbox"));
        }
    }
    first_existing(&candidates)
}
