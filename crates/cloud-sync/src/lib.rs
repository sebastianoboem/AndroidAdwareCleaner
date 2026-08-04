mod providers;

pub use providers::{
    ensure_sync_folder, list_providers, resolve_sync_folder, SyncProviderId, SyncProviderInfo,
    SyncSettingsView, SYNC_SUBFOLDER,
};

use chrono::{DateTime, Utc};
use reputation_db::{DbError, ReputationDb, ReputationSnapshot};
use serde::{Deserialize, Serialize};
use std::fs;
use std::path::{Path, PathBuf};
use thiserror::Error;

pub const SYNC_FILENAME: &str = "reputation-sync.json";

#[derive(Debug, Error)]
pub enum SyncError {
    #[error(transparent)]
    Db(#[from] DbError),
    #[error(transparent)]
    Io(#[from] std::io::Error),
    #[error(transparent)]
    Json(#[from] serde_json::Error),
    #[error("sync folder not configured")]
    NotConfigured,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum SyncMode {
    Local,
    Synced,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SyncStatus {
    pub configured: bool,
    pub mode: SyncMode,
    pub sync_path: Option<String>,
    pub last_pull: Option<DateTime<Utc>>,
    pub last_push: Option<DateTime<Utc>>,
    pub last_error: Option<String>,
}

pub struct CloudSync {
    sync_dir: Option<PathBuf>,
    last_pull: Option<DateTime<Utc>>,
    last_push: Option<DateTime<Utc>>,
    last_error: Option<String>,
}

impl Default for CloudSync {
    fn default() -> Self {
        Self::new(None)
    }
}

impl CloudSync {
    pub fn new(sync_dir: Option<PathBuf>) -> Self {
        Self {
            sync_dir,
            last_pull: None,
            last_push: None,
            last_error: None,
        }
    }

    pub fn set_sync_dir(&mut self, path: Option<PathBuf>) {
        self.sync_dir = path;
    }

    pub fn sync_path(&self) -> Option<PathBuf> {
        self.sync_dir
            .as_ref()
            .map(|d| d.join(SYNC_FILENAME))
    }

    pub fn status(&self) -> SyncStatus {
        let configured = self.sync_dir.is_some();
        let mode = if configured && self.last_pull.is_some() {
            SyncMode::Synced
        } else if configured {
            SyncMode::Synced
        } else {
            SyncMode::Local
        };
        SyncStatus {
            configured,
            mode,
            sync_path: self.sync_path().map(|p| p.to_string_lossy().into_owned()),
            last_pull: self.last_pull,
            last_push: self.last_push,
            last_error: self.last_error.clone(),
        }
    }

    /// Pull remote snapshot and merge into local DB (on app startup).
    pub fn pull(&mut self, db: &ReputationDb) -> Result<bool, SyncError> {
        let path = self.sync_path().ok_or(SyncError::NotConfigured)?;
        if !path.exists() {
            return Ok(false);
        }
        let data = fs::read_to_string(&path)?;
        let snapshot: ReputationSnapshot = serde_json::from_str(&data)?;
        db.merge_snapshot(&snapshot)?;
        self.last_pull = Some(Utc::now());
        self.last_error = None;
        Ok(true)
    }

    /// Export local DB and push to remote path (after scan/uninstall).
    pub fn push(&mut self, db: &ReputationDb) -> Result<(), SyncError> {
        let path = self.sync_path().ok_or(SyncError::NotConfigured)?;
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent)?;
        }
        let snapshot = db.export_snapshot()?;
        let json = serde_json::to_string_pretty(&snapshot)?;
        write_atomic(&path, &json)?;
        self.last_push = Some(Utc::now());
        self.last_error = None;
        Ok(())
    }

    pub fn pull_then_push(&mut self, db: &ReputationDb) -> Result<(), SyncError> {
        let _ = self.pull(db);
        self.push(db)
    }

    pub fn record_error(&mut self, err: impl Into<String>) {
        self.last_error = Some(err.into());
    }
}

fn write_atomic(path: &Path, content: &str) -> Result<(), SyncError> {
    let tmp = path.with_extension("json.tmp");
    fs::write(&tmp, content)?;
    fs::rename(tmp, path)?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::env::temp_dir;
    use uuid::Uuid;

    #[test]
    fn push_pull_roundtrip() {
        let dir = temp_dir().join(format!("aac-sync-test-{}", Uuid::new_v4()));
        fs::create_dir_all(&dir).unwrap();
        let db = ReputationDb::open(dir.join("local.db")).unwrap();
        db.record_report("com.bad.app", "s1", Some("adware")).unwrap();

        let mut sync = CloudSync::new(Some(dir.clone()));
        sync.push(&db).unwrap();

        let db2 = ReputationDb::open(dir.join("local2.db")).unwrap();
        sync.pull(&db2).unwrap();
        let rep = db2.get_reputation("com.bad.app").unwrap();
        assert_eq!(rep.report_count, 1);
        assert!(rep.marked_suspicious);
        assert!(rep.is_suspicious());
    }
}
