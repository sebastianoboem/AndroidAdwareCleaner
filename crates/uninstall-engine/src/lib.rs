use adb_bridge::AdbBridge;
use serde::{Deserialize, Serialize};
use thiserror::Error;

/// Packages that must never be removed.
pub const BLOCKLIST: &[&str] = &[
    "com.android.systemui",
    "com.android.settings",
    "com.android.phone",
    "com.android.dialer",
    "com.google.android.dialer",
    "com.android.launcher",
    "com.android.launcher3",
    "com.google.android.apps.nexuslauncher",
    "com.google.android.gms",
    "com.android.vending",
    "com.android.keychain",
    "com.android.providers.settings",
    "com.android.server.telecom",
];

#[derive(Debug, Error)]
pub enum UninstallError {
    #[error("package {0} is blocklisted and cannot be removed")]
    Blocklisted(String),
    #[error(transparent)]
    Adb(#[from] adb_bridge::AdbError),
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum UninstallStatus {
    Success,
    Failed,
    Skipped,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct UninstallResult {
    pub package_name: String,
    pub status: UninstallStatus,
    pub message: String,
}

pub struct UninstallEngine {
    bridge: AdbBridge,
}

impl UninstallEngine {
    pub fn new(bridge: AdbBridge) -> Self {
        Self { bridge }
    }

    pub fn is_blocklisted(package: &str) -> bool {
        BLOCKLIST.contains(&package)
    }

    pub fn bulk_uninstall(
        &self,
        packages: &[String],
        dry_run: bool,
    ) -> Result<Vec<UninstallResult>, UninstallError> {
        self.bulk_uninstall_with_progress(packages, dry_run, |_, _, _| {})
    }

    pub fn bulk_uninstall_with_progress<F>(
        &self,
        packages: &[String],
        dry_run: bool,
        mut on_progress: F,
    ) -> Result<Vec<UninstallResult>, UninstallError>
    where
        F: FnMut(usize, usize, &UninstallResult),
    {
        let total = packages.len();
        let mut results = Vec::with_capacity(total);

        for (index, package) in packages.iter().enumerate() {
            let result = if Self::is_blocklisted(package) {
                UninstallResult {
                    package_name: package.clone(),
                    status: UninstallStatus::Skipped,
                    message: "blocklisted system package".into(),
                }
            } else if dry_run {
                UninstallResult {
                    package_name: package.clone(),
                    status: UninstallStatus::Skipped,
                    message: "dry run — no changes made".into(),
                }
            } else {
                self.uninstall_one(package)
            };
            on_progress(index + 1, total, &result);
            results.push(result);
        }

        Ok(results)
    }

    fn uninstall_one(&self, package: &str) -> UninstallResult {
        let _ = self.bridge.force_stop(package);

        if let Ok(admins) = self.bridge.list_device_admins() {
            for admin in admins {
                if admin.contains(package) {
                    let component = admin
                        .split_whitespace()
                        .find(|s| s.contains('/'))
                        .unwrap_or(&admin);
                    let _ = self.bridge.remove_active_admin(component);
                }
            }
        }

        match self.bridge.uninstall(package) {
            Ok(out) if out.to_lowercase().contains("success") => UninstallResult {
                package_name: package.to_string(),
                status: UninstallStatus::Success,
                message: out.trim().to_string(),
            },
            Ok(_) => match self.bridge.uninstall_user(package) {
                Ok(out) => UninstallResult {
                    package_name: package.to_string(),
                    status: if out.contains("Success") {
                        UninstallStatus::Success
                    } else {
                        UninstallStatus::Failed
                    },
                    message: out.trim().to_string(),
                },
                Err(e) => UninstallResult {
                    package_name: package.to_string(),
                    status: UninstallStatus::Failed,
                    message: e.to_string(),
                },
            },
            Err(e) => UninstallResult {
                package_name: package.to_string(),
                status: UninstallStatus::Failed,
                message: e.to_string(),
            },
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn blocklist_protects_settings() {
        assert!(UninstallEngine::is_blocklisted("com.android.settings"));
        assert!(!UninstallEngine::is_blocklisted("com.evil.adware"));
    }
}
