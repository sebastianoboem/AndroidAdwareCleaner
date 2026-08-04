use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct UninstallEvent {
    pub id: String,
    pub package_name: String,
    pub device_serial: String,
    pub uninstalled_at: DateTime<Utc>,
    pub success: bool,
    pub notes: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ReportEvent {
    pub id: String,
    pub package_name: String,
    pub device_serial: String,
    pub reported_at: DateTime<Utc>,
    pub reason: Option<String>,
}

/// Disinstallazioni oltre questa soglia → flag automatico sospetta.
pub const SUSPICIOUS_UNINSTALL_THRESHOLD: u64 = 5;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PackageReputation {
    pub package_name: String,
    pub uninstall_count: u64,
    /// Legacy event count; suspicious flag uses `marked_suspicious` (bool).
    pub report_count: u64,
    #[serde(default)]
    pub marked_system: bool,
    #[serde(default)]
    pub marked_trusted: bool,
    #[serde(default)]
    pub marked_suspicious: bool,
}

impl PackageReputation {
    pub fn is_suspicious(&self) -> bool {
        !self.marked_trusted
            && !self.marked_system
            && (self.marked_suspicious || self.uninstall_count > SUSPICIOUS_UNINSTALL_THRESHOLD)
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PackageMark {
    pub package_name: String,
    pub marked_system: bool,
    pub marked_trusted: bool,
    #[serde(default)]
    pub marked_suspicious: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ReputationSnapshot {
    pub version: u32,
    pub exported_at: DateTime<Utc>,
    pub uninstall_events: Vec<UninstallEvent>,
    pub report_events: Vec<ReportEvent>,
    #[serde(default)]
    pub package_marks: Vec<PackageMark>,
}

impl ReputationSnapshot {
    pub const VERSION: u32 = 2;
}
