use crate::error::AdbError;
use crate::resolver::resolve_adb_path;
use serde::{Deserialize, Serialize};
use std::path::PathBuf;
use std::process::Command;
use std::sync::OnceLock;
use std::thread;
use std::time::{Duration, Instant};

#[cfg(windows)]
fn hide_console_window(cmd: &mut Command) {
    use std::os::windows::process::CommandExt;
    const CREATE_NO_WINDOW: u32 = 0x0800_0000;
    cmd.creation_flags(CREATE_NO_WINDOW);
}

#[cfg(not(windows))]
fn hide_console_window(_cmd: &mut Command) {}

fn command_no_window(program: &std::path::Path) -> Command {
    let mut cmd = Command::new(program);
    hide_console_window(&mut cmd);
    cmd
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum DeviceState {
    Device,
    Unauthorized,
    Offline,
    Unknown(String),
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AdbDevice {
    pub serial: String,
    pub state: DeviceState,
    pub model: Option<String>,
    pub product: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PackageInfo {
    pub package_name: String,
    pub is_system: bool,
}

pub struct AdbBridge {
    adb: PathBuf,
    serial: Option<String>,
    resolved_serial: OnceLock<String>,
}

impl AdbBridge {
    pub fn new() -> Result<Self, AdbError> {
        let adb = resolve_adb_path().ok_or(AdbError::BinaryNotFound)?;
        Ok(Self {
            adb,
            serial: None,
            resolved_serial: OnceLock::new(),
        })
    }

    pub fn with_serial(serial: impl Into<String>) -> Result<Self, AdbError> {
        let mut bridge = Self::new()?;
        bridge.serial = Some(serial.into());
        Ok(bridge)
    }

    pub fn adb_path(&self) -> &PathBuf {
        &self.adb
    }

    pub fn list_devices(&self) -> Result<Vec<AdbDevice>, AdbError> {
        let output = self.run_adb(&["devices", "-l"])?;
        parse_devices(&output)
    }

    pub fn start_server(&self) -> Result<(), AdbError> {
        let _ = self.run_adb(&["start-server"])?;
        Ok(())
    }

    pub fn reconnect_usb(&self) -> Result<(), AdbError> {
        let _ = self.run_adb(&["reconnect", "usb"])?;
        Ok(())
    }

    /// Attende fino a `wait_ms` un dispositivo visibile in `adb devices` (anche non autorizzato).
    pub fn discover_device_serial(&self, wait_ms: u64) -> Result<String, AdbError> {
        self.start_server()?;
        let deadline = Instant::now() + Duration::from_millis(wait_ms);
        let mut reconnected = false;

        loop {
            if let Some(device) = self.pick_best_device()? {
                return Ok(device.serial);
            }
            if Instant::now() >= deadline {
                return Err(AdbError::NoDevice);
            }
            if !reconnected {
                let _ = self.reconnect_usb();
                reconnected = true;
            }
            thread::sleep(Duration::from_millis(500));
        }
    }

    pub fn device_state_for_serial(&self, serial: &str) -> Result<Option<DeviceState>, AdbError> {
        let devices = self.list_devices()?;
        Ok(devices
            .iter()
            .find(|d| d.serial == serial)
            .map(|d| d.state.clone()))
    }

    fn pick_best_device(&self) -> Result<Option<AdbDevice>, AdbError> {
        let devices = self.list_devices()?;
        if let Some(want) = &self.serial {
            return Ok(devices.into_iter().find(|d| d.serial == *want));
        }
        Ok(pick_best_from_list(&devices))
    }

    pub fn get_serial(&self) -> Result<String, AdbError> {
        if let Some(s) = self.resolved_serial.get() {
            return Ok(s.clone());
        }
        let devices = self.list_devices()?;
        let serial = select_serial(&devices, self.serial.as_deref())?;
        let _ = self.resolved_serial.set(serial.clone());
        Ok(serial)
    }

    pub fn list_packages(&self, user_only: bool) -> Result<Vec<PackageInfo>, AdbError> {
        let serial = self.get_serial()?;
        let args: Vec<&str> = if user_only {
            vec!["-s", &serial, "shell", "pm", "list", "packages", "-3"]
        } else {
            vec!["-s", &serial, "shell", "pm", "list", "packages"]
        };
        let output = self.run_adb(&args)?;
        let mut packages: Vec<PackageInfo> = output
            .lines()
            .filter_map(|line| line.strip_prefix("package:"))
            .map(|name| PackageInfo {
                package_name: name.trim().to_string(),
                is_system: false,
            })
            .collect();

        if !user_only {
            let sys_out = self.run_adb(&[
                "-s",
                &serial,
                "shell",
                "pm",
                "list",
                "packages",
                "-s",
            ])?;
            let system: std::collections::HashSet<String> = sys_out
                .lines()
                .filter_map(|l| l.strip_prefix("package:"))
                .map(str::trim)
                .map(str::to_string)
                .collect();
            for pkg in &mut packages {
                pkg.is_system = system.contains(&pkg.package_name);
            }
        }

        packages.sort_by(|a, b| a.package_name.cmp(&b.package_name));
        Ok(packages)
    }

    pub fn shell(&self, command: &str) -> Result<String, AdbError> {
        let serial = self.get_serial()?;
        self.run_adb(&["-s", &serial, "shell", command])
    }

    pub fn exec_out(&self, command: &str) -> Result<Vec<u8>, AdbError> {
        let serial = self.get_serial()?;
        let output = command_no_window(&self.adb)
            .args(["-s", &serial, "exec-out", command])
            .output()
            .map_err(AdbError::Io)?;

        if !output.status.success() && output.stdout.is_empty() {
            let stderr = String::from_utf8_lossy(&output.stderr).to_string();
            return Err(AdbError::CommandFailed(if stderr.is_empty() {
                format!("adb exec-out {:?} failed", command)
            } else {
                stderr
            }));
        }

        Ok(output.stdout)
    }

    pub fn uninstall(&self, package: &str) -> Result<String, AdbError> {
        let serial = self.get_serial()?;
        self.run_adb(&["-s", &serial, "uninstall", package])
    }

    pub fn uninstall_user(&self, package: &str) -> Result<String, AdbError> {
        let serial = self.get_serial()?;
        self.run_adb(&[
            "-s",
            &serial,
            "shell",
            "pm",
            "uninstall",
            "-k",
            "--user",
            "0",
            package,
        ])
    }

    pub fn force_stop(&self, package: &str) -> Result<String, AdbError> {
        self.shell(&format!("am force-stop {package}"))
    }

    pub fn list_device_admins(&self) -> Result<Vec<String>, AdbError> {
        let output = self.shell("dpm list-owners")?;
        Ok(output.lines().map(str::trim).filter(|l| !l.is_empty()).map(str::to_string).collect())
    }

    pub fn remove_active_admin(&self, component: &str) -> Result<String, AdbError> {
        self.shell(&format!("dpm remove-active-admin {component}"))
    }

    pub fn get_device_property(&self, prop: &str) -> Result<String, AdbError> {
        let val = self.shell(&format!("getprop {prop}"))?;
        Ok(val.trim().to_string())
    }

    pub fn input_text(&self, text: &str) -> Result<String, AdbError> {
        let escaped = text
            .replace('\\', "\\\\")
            .replace(' ', "%s")
            .replace('\'', "\\'");
        self.shell(&format!("input text {escaped}"))
    }

    pub fn keyevent(&self, code: i32) -> Result<String, AdbError> {
        self.shell(&format!("input keyevent {code}"))
    }

    pub fn open_settings(&self) -> Result<String, AdbError> {
        self.shell("am start -a android.settings.SETTINGS")
    }

    pub fn device_connection_state(&self) -> Result<DeviceState, AdbError> {
        self.start_server()?;
        if let Some(device) = self.pick_best_device()? {
            return Ok(device.state);
        }
        Err(AdbError::NoDevice)
    }

    pub fn set_airplane_mode(&self, enabled: bool) -> Result<String, AdbError> {
        let cmd = if enabled {
            "cmd connectivity airplane-mode enable"
        } else {
            "cmd connectivity airplane-mode disable"
        };
        self.shell(cmd)
    }

    fn run_adb(&self, args: &[&str]) -> Result<String, AdbError> {
        let output = command_no_window(&self.adb)
            .args(args)
            .output()
            .map_err(AdbError::Io)?;

        let stdout = String::from_utf8_lossy(&output.stdout).to_string();
        let stderr = String::from_utf8_lossy(&output.stderr).to_string();

        if !output.status.success() && stdout.is_empty() {
            return Err(AdbError::CommandFailed(if stderr.is_empty() {
                format!("adb {:?} failed", args)
            } else {
                stderr
            }));
        }

        Ok(if stdout.is_empty() { stderr } else { stdout })
    }
}

fn select_serial(devices: &[AdbDevice], want: Option<&str>) -> Result<String, AdbError> {
    if let Some(s) = want {
        if devices.iter().any(|d| d.serial == s) {
            return Ok(s.to_string());
        }
        return Err(AdbError::NoDevice);
    }
    let authorized: Vec<_> = devices
        .iter()
        .filter(|d| d.state == DeviceState::Device)
        .collect();
    match authorized.len() {
        0 => {
            if devices.iter().any(|d| d.state == DeviceState::Unauthorized) {
                return Err(AdbError::Unauthorized);
            }
            Err(AdbError::NoDevice)
        }
        1 => Ok(authorized[0].serial.clone()),
        _ => Err(AdbError::CommandFailed(
            "multiple devices connected; specify serial".into(),
        )),
    }
}

fn pick_best_from_list(devices: &[AdbDevice]) -> Option<AdbDevice> {
    for state in [
        DeviceState::Device,
        DeviceState::Unauthorized,
        DeviceState::Offline,
    ] {
        if let Some(d) = devices.iter().find(|d| d.state == state) {
            return Some(d.clone());
        }
    }
    devices.first().cloned()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn dev(serial: &str, state: DeviceState) -> AdbDevice {
        AdbDevice {
            serial: serial.into(),
            state,
            model: None,
            product: None,
        }
    }

    #[test]
    fn select_serial_returns_explicit_serial_when_listed() {
        let devices = vec![dev("abc", DeviceState::Unauthorized)];
        assert_eq!(select_serial(&devices, Some("abc")).unwrap(), "abc");
    }

    #[test]
    fn select_serial_errors_when_explicit_serial_missing() {
        let devices = vec![dev("other", DeviceState::Device)];
        assert!(matches!(
            select_serial(&devices, Some("abc")),
            Err(AdbError::NoDevice)
        ));
    }

    #[test]
    fn select_serial_picks_single_authorized_device() {
        let devices = vec![
            dev("un1", DeviceState::Unauthorized),
            dev("ok1", DeviceState::Device),
        ];
        assert_eq!(select_serial(&devices, None).unwrap(), "ok1");
    }

    #[test]
    fn select_serial_reports_unauthorized_when_no_authorized() {
        let devices = vec![dev("un1", DeviceState::Unauthorized)];
        assert!(matches!(
            select_serial(&devices, None),
            Err(AdbError::Unauthorized)
        ));
    }

    #[test]
    fn select_serial_errors_on_multiple_authorized() {
        let devices = vec![dev("a", DeviceState::Device), dev("b", DeviceState::Device)];
        assert!(matches!(
            select_serial(&devices, None),
            Err(AdbError::CommandFailed(_))
        ));
    }

    #[test]
    fn pick_prefers_authorized_device() {
        let devices = vec![
            AdbDevice {
                serial: "offline1".into(),
                state: DeviceState::Offline,
                model: None,
                product: None,
            },
            AdbDevice {
                serial: "auth1".into(),
                state: DeviceState::Device,
                model: None,
                product: None,
            },
        ];
        assert_eq!(
            pick_best_from_list(&devices).map(|d| d.serial),
            Some("auth1".into())
        );
    }

    #[test]
    fn pick_unauthorized_when_no_authorized() {
        let devices = vec![AdbDevice {
            serial: "unauth1".into(),
            state: DeviceState::Unauthorized,
            model: None,
            product: None,
        }];
        assert_eq!(
            pick_best_from_list(&devices).map(|d| d.serial),
            Some("unauth1".into())
        );
    }
}

fn parse_devices(output: &str) -> Result<Vec<AdbDevice>, AdbError> {
    let mut devices = Vec::new();
    for line in output.lines().skip(1) {
        let line = line.trim();
        if line.is_empty() {
            continue;
        }
        let mut parts = line.split_whitespace();
        let serial = parts.next().unwrap_or_default().to_string();
        let state_str = parts.next().unwrap_or("unknown");
        let state = match state_str {
            "device" => DeviceState::Device,
            "unauthorized" => DeviceState::Unauthorized,
            "offline" => DeviceState::Offline,
            other => DeviceState::Unknown(other.to_string()),
        };
        let mut model = None;
        let mut product = None;
        for token in parts {
            if let Some(m) = token.strip_prefix("model:") {
                model = Some(m.to_string());
            } else if let Some(p) = token.strip_prefix("product:") {
                product = Some(p.to_string());
            }
        }
        devices.push(AdbDevice {
            serial,
            state,
            model,
            product,
        });
    }
    Ok(devices)
}
