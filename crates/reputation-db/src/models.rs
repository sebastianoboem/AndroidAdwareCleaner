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

/// Disinstallazioni oltre questa soglia → sospetta, salvo flag effettivo trusted o sistema.
pub const SUSPICIOUS_UNINSTALL_THRESHOLD: u64 = 5;

/// Telefoni distinti che votano sospetta: da qui il flag effettivo è sospetta, sopra trusted e sistema.
pub const SUSPICIOUS_REPORT_THRESHOLD: u64 = 5;

/// Voto unico dei vecchi flag booleani, prima del modello per telefono.
pub const LEGACY_DEVICE: &str = "legacy";

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum VoteFlag {
    System,
    Trusted,
    Suspicious,
    None,
}

impl VoteFlag {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::System => "system",
            Self::Trusted => "trusted",
            Self::Suspicious => "suspicious",
            Self::None => "none",
        }
    }

    pub fn parse(s: &str) -> Option<Self> {
        match s {
            "system" => Some(Self::System),
            "trusted" => Some(Self::Trusted),
            "suspicious" => Some(Self::Suspicious),
            "none" => Some(Self::None),
            _ => None,
        }
    }
}

/// Un voto per telefono. `flag` è `system`, `trusted`, `suspicious` o `none`.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct FlagVote {
    pub package_name: String,
    pub device_serial: String,
    pub flag: String,
    pub voted_at: DateTime<Utc>,
}

/// Contatori già aggregati dal remoto (Supabase). Il locale prende il massimo con i propri.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RemoteStat {
    pub package_name: String,
    pub uninstall_count: u64,
    pub system_votes: u64,
    pub trusted_votes: u64,
    pub suspicious_votes: u64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PackageReputation {
    pub package_name: String,
    pub uninstall_count: u64,
    /// Telefoni distinti che votano sospetta (massimo tra locale e remoto).
    pub report_count: u64,
    #[serde(default)]
    pub marked_system: bool,
    #[serde(default)]
    pub marked_trusted: bool,
    #[serde(default)]
    pub marked_suspicious: bool,
    /// Voto del telefono collegato: `system`, `trusted`, `suspicious`, oppure assente.
    #[serde(default)]
    pub my_vote: Option<String>,
}

impl PackageReputation {
    pub fn is_suspicious(&self) -> bool {
        row_is_suspicious(self, false)
    }
}

/// Flag effettivo esclusivo. La soglia di segnalazioni scavalca sistema e trusted.
pub fn effective_marks(system_votes: u64, trusted_votes: u64, suspicious_votes: u64) -> (bool, bool, bool) {
    if suspicious_votes >= SUSPICIOUS_REPORT_THRESHOLD {
        return (false, false, true);
    }
    if system_votes >= 1 {
        return (true, false, false);
    }
    if trusted_votes >= 1 {
        return (false, true, false);
    }
    if suspicious_votes >= 1 {
        return (false, false, true);
    }
    (false, false, false)
}

/// `is_android_system` è il flag del pacchetto sul telefono, non un voto.
/// La soglia di segnalazioni resta sospetta anche per le app di sistema Android.
pub fn row_is_suspicious(rep: &PackageReputation, is_android_system: bool) -> bool {
    let forced = rep.report_count >= SUSPICIOUS_REPORT_THRESHOLD && rep.marked_suspicious;
    if forced {
        return true;
    }
    let whitelisted = rep.marked_trusted || rep.marked_system || is_android_system;
    !whitelisted
        && (rep.marked_suspicious || rep.uninstall_count > SUSPICIOUS_UNINSTALL_THRESHOLD)
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
    #[serde(default)]
    pub report_events: Vec<ReportEvent>,
    #[serde(default)]
    pub package_marks: Vec<PackageMark>,
    #[serde(default)]
    pub flag_votes: Vec<FlagVote>,
}

impl ReputationSnapshot {
    pub const VERSION: u32 = 3;
}
