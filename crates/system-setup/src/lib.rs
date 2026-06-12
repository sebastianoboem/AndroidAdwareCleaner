mod guides;
mod platform_tools;

pub use guides::{find_guide, find_guide_by_model, resolve_guide, DeviceGuide, DeviceGuides, GuideStep};
pub use platform_tools::{ensure_platform_tools, PlatformToolsResult, SystemInfo};

use serde::Serialize;

#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum OsFamily {
    MacOs,
    Windows,
    Linux,
    Unknown,
}

#[derive(Debug, Clone, Serialize)]
pub struct SetupStatus {
    pub os: OsFamily,
    pub os_version: String,
    pub adb_present: bool,
    pub adb_path: Option<String>,
    pub drivers_ok: bool,
    pub drivers_message: String,
    pub ready: bool,
}

pub fn detect_system() -> SystemInfo {
    SystemInfo::detect()
}

pub fn check_setup() -> SetupStatus {
    let sys = SystemInfo::detect();
    let adb_path = adb_bridge::resolve_adb_path();
    let adb_present = adb_path.is_some();

    let (drivers_ok, drivers_message) = match sys.os {
        OsFamily::MacOs => (
            true,
            "macOS: driver USB Android non richiesti per la maggior parte dei dispositivi".into(),
        ),
        OsFamily::Linux => (
            true,
            "Linux: verifica regole udev se il dispositivo non viene rilevato".into(),
        ),
        OsFamily::Windows => {
            if adb_present {
                (
                    true,
                    "Windows: ADB presente; se il telefono non compare installa il driver OEM o Google USB".into(),
                )
            } else {
                (
                    false,
                    "Windows: ADB assente — verrà installato platform-tools con driver USB Google".into(),
                )
            }
        }
        OsFamily::Unknown => (false, "Sistema operativo non riconosciuto".into()),
    };

    let ready = adb_present && drivers_ok;

    SetupStatus {
        os: sys.os,
        os_version: sys.version,
        adb_present,
        adb_path: adb_path.map(|p| p.to_string_lossy().into_owned()),
        drivers_ok,
        drivers_message,
        ready,
    }
}

pub fn run_setup(install_dir: &std::path::Path) -> Result<SetupStatus, String> {
    if adb_bridge::resolve_adb_path().is_none() {
        platform_tools::ensure_platform_tools(install_dir)?;
    }
    Ok(check_setup())
}

pub fn load_guides() -> DeviceGuides {
    guides::load_embedded_guides()
}
