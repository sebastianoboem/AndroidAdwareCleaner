use super::OsFamily;
use serde::Serialize;
use std::path::{Path, PathBuf};
use std::process::Command;

#[cfg(windows)]
fn hide_console_window(cmd: &mut Command) {
    use std::os::windows::process::CommandExt;
    const CREATE_NO_WINDOW: u32 = 0x0800_0000;
    cmd.creation_flags(CREATE_NO_WINDOW);
}

#[cfg(not(windows))]
fn hide_console_window(_cmd: &mut Command) {}

fn command_no_window(program: &str) -> Command {
    let mut cmd = Command::new(program);
    hide_console_window(&mut cmd);
    cmd
}

#[derive(Debug, Clone, Serialize)]
pub struct SystemInfo {
    pub os: OsFamily,
    pub version: String,
    pub arch: String,
}

impl SystemInfo {
    pub fn detect() -> Self {
        let os = if cfg!(target_os = "macos") {
            OsFamily::MacOs
        } else if cfg!(target_os = "windows") {
            OsFamily::Windows
        } else if cfg!(target_os = "linux") {
            OsFamily::Linux
        } else {
            OsFamily::Unknown
        };

        let version = std::env::consts::OS.to_string();
        let arch = std::env::consts::ARCH.to_string();

        Self {
            os,
            version,
            arch,
        }
    }
}

#[derive(Debug, Clone, Serialize)]
pub struct PlatformToolsResult {
    pub installed_path: PathBuf,
    pub message: String,
}

pub fn ensure_platform_tools(dest: &Path) -> Result<PlatformToolsResult, String> {
    std::fs::create_dir_all(dest).map_err(|e| e.to_string())?;

    let url = platform_tools_url();
    let zip_path = dest.join("platform-tools-download.zip");

    download_file(&url, &zip_path)?;

    extract_zip(&zip_path, dest)?;

    let _ = std::fs::remove_file(&zip_path);

    let adb_name = if cfg!(windows) { "adb.exe" } else { "adb" };
    let adb_path = dest.join("platform-tools").join(adb_name);
    if !adb_path.exists() {
        return Err("download completato ma adb non trovato nell'archivio".into());
    }

    Ok(PlatformToolsResult {
        installed_path: adb_path,
        message: format!("platform-tools installati in {}", dest.display()),
    })
}

fn platform_tools_url() -> String {
    if cfg!(target_os = "macos") {
        "https://dl.google.com/android/repository/platform-tools-latest-darwin.zip".into()
    } else if cfg!(target_os = "windows") {
        "https://dl.google.com/android/repository/platform-tools-latest-windows.zip".into()
    } else {
        "https://dl.google.com/android/repository/platform-tools-latest-linux.zip".into()
    }
}

fn download_file(url: &str, dest: &Path) -> Result<(), String> {
    let status = command_no_window("curl")
        .args(["-fsSL", url, "-o"])
        .arg(dest)
        .status()
        .map_err(|e| format!("curl non disponibile: {e}"))?;

    if status.success() {
        Ok(())
    } else {
        Err(format!("download fallito da {url}"))
    }
}

fn extract_zip(zip: &Path, dest: &Path) -> Result<(), String> {
    if cfg!(target_os = "windows") {
        let zip_str = zip.to_string_lossy();
        let dest_str = dest.to_string_lossy();
        let ps = format!(
            "Expand-Archive -Path '{}' -DestinationPath '{}' -Force",
            zip_str.replace('\'', "''"),
            dest_str.replace('\'', "''")
        );
        let status = command_no_window("powershell")
            .args(["-NoProfile", "-Command", &ps])
            .status()
            .map_err(|e| e.to_string())?;
        if status.success() {
            Ok(())
        } else {
            Err("estrazione ZIP fallita (PowerShell)".into())
        }
    } else {
        let status = command_no_window("unzip")
            .args(["-q", "-o"])
            .arg(zip)
            .arg("-d")
            .arg(dest)
            .status()
            .map_err(|e| format!("unzip non disponibile: {e}"))?;
        if status.success() {
            Ok(())
        } else {
            Err("estrazione ZIP fallita".into())
        }
    }
}
