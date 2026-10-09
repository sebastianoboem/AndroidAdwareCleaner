mod providers;
mod supabase;

pub use providers::{
    ensure_sync_folder, list_providers, resolve_sync_folder, SyncProviderId,
    SyncProviderInfo, SyncSettingsView, SYNC_SUBFOLDER,
};
pub use supabase::{phone_hash, SupabaseConfig};

use chrono::{DateTime, Utc};
use reputation_db::{DbError, ReputationDb, ReputationSnapshot};
use serde::{Deserialize, Serialize};
use std::fs;
use std::path::{Path, PathBuf};
use thiserror::Error;

pub const SYNC_FILENAME: &str = "reputation-sync.json";
const RECONCILE_TRIES: u32 = 5;

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
    #[error("{0}")]
    Http(String),
}

#[derive(Debug, Clone)]
pub enum SyncBackend {
    Local,
    Folder(PathBuf),
    Supabase(SupabaseConfig),
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
    backend: SyncBackend,
    last_pull: Option<DateTime<Utc>>,
    last_push: Option<DateTime<Utc>>,
    last_error: Option<String>,
}

impl Default for CloudSync {
    fn default() -> Self {
        Self::new(SyncBackend::Local)
    }
}

impl CloudSync {
    pub fn new(backend: SyncBackend) -> Self {
        Self {
            backend,
            last_pull: None,
            last_push: None,
            last_error: None,
        }
    }

    pub fn set_backend(&mut self, backend: SyncBackend) {
        self.backend = backend;
    }

    pub fn is_supabase(&self) -> bool {
        matches!(self.backend, SyncBackend::Supabase(_))
    }

    pub fn sync_path(&self) -> Option<PathBuf> {
        match &self.backend {
            SyncBackend::Folder(dir) => Some(dir.join(SYNC_FILENAME)),
            _ => None,
        }
    }

    pub fn status(&self) -> SyncStatus {
        let (configured, sync_path) = match &self.backend {
            SyncBackend::Local => (false, None),
            SyncBackend::Folder(dir) => (
                true,
                Some(dir.join(SYNC_FILENAME).to_string_lossy().into_owned()),
            ),
            SyncBackend::Supabase(cfg) => (true, Some(cfg.url.clone())),
        };
        SyncStatus {
            configured,
            mode: if configured {
                SyncMode::Synced
            } else {
                SyncMode::Local
            },
            sync_path,
            last_pull: self.last_pull,
            last_push: self.last_push,
            last_error: self.last_error.clone(),
        }
    }

    /// Legge il remoto, unisce nel DB locale e riscrive solo se il risultato è diverso.
    pub fn reconcile(&mut self, db: &ReputationDb) -> Result<(), SyncError> {
        match &self.backend {
            SyncBackend::Local => Ok(()),
            SyncBackend::Folder(_) => self.reconcile_folder(db),
            SyncBackend::Supabase(cfg) => {
                let cfg = cfg.clone();
                match cfg.push_and_pull(db) {
                    Ok(()) => {
                        let now = Utc::now();
                        self.last_pull = Some(now);
                        self.last_push = Some(now);
                        self.last_error = None;
                        Ok(())
                    }
                    Err(e) => {
                        self.last_error = Some(e.to_string());
                        Err(e)
                    }
                }
            }
        }
    }

    /// Solo i totali remoti, per aggiornare i badge a inizio scansione.
    pub fn pull_stats(&mut self, db: &ReputationDb) -> Result<(), SyncError> {
        let SyncBackend::Supabase(cfg) = &self.backend else {
            return Ok(());
        };
        let cfg = cfg.clone();
        match cfg.pull_stats(db) {
            Ok(()) => {
                self.last_pull = Some(Utc::now());
                self.last_error = None;
                Ok(())
            }
            Err(e) => {
                self.last_error = Some(e.to_string());
                Err(e)
            }
        }
    }

    fn reconcile_folder(&mut self, db: &ReputationDb) -> Result<(), SyncError> {
        let path = self.sync_path().ok_or(SyncError::NotConfigured)?;
        for _ in 0..RECONCILE_TRIES {
            let before = read_optional(&path)?;
            if let Some(text) = &before {
                let snapshot: ReputationSnapshot = serde_json::from_str(text)?;
                db.merge_snapshot(&snapshot)?;
            }
            let exported = db.export_snapshot()?;
            if before.as_deref().map(canonical_text).transpose()? == Some(canonical(&exported)) {
                self.last_pull = Some(Utc::now());
                self.last_error = None;
                return Ok(());
            }
            let json = serde_json::to_string_pretty(&exported)?;
            match try_replace(&path, before, &json)? {
                WriteOutcome::Wrote => {
                    let now = Utc::now();
                    self.last_pull = Some(now);
                    self.last_push = Some(now);
                    self.last_error = None;
                    return Ok(());
                }
                WriteOutcome::Conflict => continue,
            }
        }
        let err = SyncError::Http("il file remoto è cambiato durante la scrittura".into());
        self.last_error = Some(err.to_string());
        Err(err)
    }
}

enum WriteOutcome {
    Wrote,
    Conflict,
}

fn read_optional(path: &Path) -> Result<Option<String>, SyncError> {
    if !path.exists() {
        return Ok(None);
    }
    Ok(Some(fs::read_to_string(path)?))
}

fn canonical_text(text: &str) -> Result<String, SyncError> {
    let snapshot: ReputationSnapshot = serde_json::from_str(text)?;
    Ok(canonical(&snapshot))
}

fn canonical(snapshot: &ReputationSnapshot) -> String {
    let mut events: Vec<String> = snapshot
        .uninstall_events
        .iter()
        .map(|e| {
            format!(
                "{}|{}|{}|{}|{}|{}",
                e.id,
                e.package_name,
                e.device_serial,
                e.uninstalled_at.to_rfc3339(),
                e.success,
                e.notes.as_deref().unwrap_or("")
            )
        })
        .collect();
    events.sort();
    let mut votes: Vec<String> = snapshot
        .flag_votes
        .iter()
        .map(|v| {
            format!(
                "{}|{}|{}|{}",
                v.package_name,
                v.device_serial,
                v.flag,
                v.voted_at.to_rfc3339()
            )
        })
        .collect();
    votes.sort();
    let mut marks: Vec<String> = snapshot
        .package_marks
        .iter()
        .map(|m| {
            format!(
                "{}|{}|{}|{}",
                m.package_name, m.marked_system, m.marked_trusted, m.marked_suspicious
            )
        })
        .collect();
    marks.sort();
    format!("{}\n{}\n{}", events.join("\n"), votes.join("\n"), marks.join("\n"))
}

fn try_replace(path: &Path, expected: Option<String>, content: &str) -> Result<WriteOutcome, SyncError> {
    let current = read_optional(path)?;
    if current != expected {
        return Ok(WriteOutcome::Conflict);
    }
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)?;
    }
    write_atomic(path, content)?;
    Ok(WriteOutcome::Wrote)
}

fn write_atomic(path: &Path, content: &str) -> Result<(), SyncError> {
    let tmp = path.with_extension("json.tmp");
    fs::write(&tmp, content)?;
    fs::rename(&tmp, path)?;
    Ok(())
}

pub fn backend_for(
    provider: Option<&str>,
    folder: Option<PathBuf>,
    session_path: PathBuf,
    supabase_url: Option<&str>,
    supabase_key: Option<&str>,
) -> SyncBackend {
    match provider.unwrap_or("local") {
        "local" => SyncBackend::Local,
        "supabase" => match (supabase_url, supabase_key) {
            (Some(url), Some(key)) => supabase::config_from(url, key, session_path)
                .map(SyncBackend::Supabase)
                .unwrap_or(SyncBackend::Local),
            _ => SyncBackend::Local,
        },
        _ => match folder {
            Some(path) => SyncBackend::Folder(path),
            None => SyncBackend::Local,
        },
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use reputation_db::ReputationDb;
    use std::io::{Read, Write};
    use std::net::TcpListener;
    use std::sync::{Arc, Mutex};
    use std::thread;
    use uuid::Uuid;

    #[test]
    fn folder_reconcile_unions_two_clients_and_skips_identical_rewrite() {
        let dir = std::env::temp_dir().join(format!("aac-sync-{}", Uuid::new_v4()));
        fs::create_dir_all(&dir).unwrap();
        let db_a = ReputationDb::open(dir.join("a.db")).unwrap();
        db_a.set_vote("com.a", "phone-a", "suspicious").unwrap();
        let mut sync = CloudSync::new(SyncBackend::Folder(dir.clone()));
        sync.reconcile(&db_a).unwrap();
        let path = dir.join(SYNC_FILENAME);
        let db_b = ReputationDb::open(dir.join("b.db")).unwrap();
        db_b.set_vote("com.b", "phone-b", "trusted").unwrap();
        sync.reconcile(&db_b).unwrap();
        assert!(db_b.get_reputation("com.a", None).unwrap().marked_suspicious);
        assert!(db_b.get_reputation("com.b", None).unwrap().marked_trusted);

        let db_c = ReputationDb::open(dir.join("c.db")).unwrap();
        sync.reconcile(&db_c).unwrap();
        assert!(db_c.get_reputation("com.a", None).unwrap().marked_suspicious);
        assert!(db_c.get_reputation("com.b", None).unwrap().marked_trusted);
        let pushed = sync.status().last_push;
        sync.reconcile(&db_c).unwrap();
        assert_eq!(sync.status().last_push, pushed);
        let _ = path;
    }

    #[test]
    fn try_replace_conflicts_when_file_changed() {
        let dir = std::env::temp_dir().join(format!("aac-replace-{}", Uuid::new_v4()));
        fs::create_dir_all(&dir).unwrap();
        let path = dir.join(SYNC_FILENAME);
        fs::write(&path, "prima").unwrap();
        let outcome = try_replace(&path, Some("diverso".into()), "dopo").unwrap();
        assert!(matches!(outcome, WriteOutcome::Conflict));
        assert_eq!(fs::read_to_string(&path).unwrap(), "prima");
        let outcome = try_replace(&path, Some("prima".into()), "dopo").unwrap();
        assert!(matches!(outcome, WriteOutcome::Wrote));
        assert_eq!(fs::read_to_string(&path).unwrap(), "dopo");
    }

    #[test]
    fn v2_snapshot_imports() {
        let dir = std::env::temp_dir().join(format!("aac-v2-{}", Uuid::new_v4()));
        fs::create_dir_all(&dir).unwrap();
        let legacy = r#"{
            "version": 2,
            "exported_at": "2020-01-01T00:00:00Z",
            "uninstall_events": [],
            "report_events": [],
            "package_marks": [{
                "package_name": "com.bank.app",
                "marked_system": false,
                "marked_trusted": true,
                "marked_suspicious": false
            }]
        }"#;
        fs::write(dir.join(SYNC_FILENAME), legacy).unwrap();
        let db = ReputationDb::open(dir.join("local.db")).unwrap();
        let mut sync = CloudSync::new(SyncBackend::Folder(dir));
        sync.reconcile(&db).unwrap();
        assert!(db.get_reputation("com.bank.app", None).unwrap().marked_trusted);
    }

    #[test]
    fn supabase_reconcile_against_fake_server() {
        let state = Arc::new(Mutex::new(FakeState::default()));
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let port = listener.local_addr().unwrap().port();
        let shared = state.clone();
        thread::spawn(move || serve(listener, shared));

        let dir = std::env::temp_dir().join(format!("aac-sb-{}", Uuid::new_v4()));
        fs::create_dir_all(&dir).unwrap();
        let cfg = SupabaseConfig {
            url: format!("http://127.0.0.1:{port}"),
            key: "anon-test".into(),
            session_path: dir.join("session.json"),
        };
        let db = ReputationDb::open(dir.join("local.db")).unwrap();
        db.set_vote("com.app", "phone-1", "suspicious").unwrap();
        db.record_uninstall("com.app", "phone-1", true, None).unwrap();
        let mut sync = CloudSync::new(SyncBackend::Supabase(cfg));
        sync.reconcile(&db).unwrap();
        sync.reconcile(&db).unwrap();
        let guard = state.lock().unwrap();
        assert_eq!(guard.signups, 1);
        assert!(guard.refreshes >= 1);
        assert_eq!(guard.votes.len(), 1);
        assert_eq!(guard.uninstalls.len(), 1);
        drop(guard);
        let rep = db.get_reputation("com.app", None).unwrap();
        assert!(rep.report_count >= 1);

        let mut offline = CloudSync::new(SyncBackend::Supabase(SupabaseConfig {
            url: "http://127.0.0.1:1".into(),
            key: "anon-test".into(),
            session_path: dir.join("offline.json"),
        }));
        assert!(offline.reconcile(&db).is_err());
        assert!(offline.status().last_error.is_some());
    }

    #[derive(Default)]
    struct FakeState {
        signups: u32,
        refreshes: u32,
        votes: Vec<serde_json::Value>,
        uninstalls: Vec<serde_json::Value>,
    }

    fn serve(listener: TcpListener, state: Arc<Mutex<FakeState>>) {
        for stream in listener.incoming().flatten() {
            let _ = handle(stream, &state);
        }
    }

    fn handle(mut stream: std::net::TcpStream, state: &Mutex<FakeState>) -> std::io::Result<()> {
        let mut buf = Vec::new();
        let mut tmp = [0u8; 2048];
        let mut header_end = None;
        loop {
            let n = stream.read(&mut tmp)?;
            if n == 0 {
                break;
            }
            buf.extend_from_slice(&tmp[..n]);
            if let Some(pos) = buf.windows(4).position(|w| w == b"\r\n\r\n") {
                header_end = Some(pos + 4);
                break;
            }
        }
        let Some(header_end) = header_end else {
            return Ok(());
        };
        let header = String::from_utf8_lossy(&buf[..header_end]).to_string();
        let length = header
            .lines()
            .find_map(|line| {
                line.to_ascii_lowercase()
                    .strip_prefix("content-length:")
                    .map(|v| v.trim().parse::<usize>().unwrap_or(0))
            })
            .unwrap_or(0);
        while buf.len() < header_end + length {
            let n = stream.read(&mut tmp)?;
            if n == 0 {
                break;
            }
            buf.extend_from_slice(&tmp[..n]);
        }
        let body = String::from_utf8_lossy(&buf[header_end..header_end + length.min(buf.len() - header_end)]).to_string();
        let request_line = header.lines().next().unwrap_or("");
        let mut guard = state.lock().unwrap();
        let (status, response) = if request_line.starts_with("POST /auth/v1/signup") {
            guard.signups += 1;
            let n = guard.signups;
            (
                200,
                serde_json::json!({
                    "access_token": format!("access-{n}"),
                    "refresh_token": format!("refresh-{n}"),
                    "expires_in": 0
                })
                .to_string(),
            )
        } else if request_line.starts_with("POST /auth/v1/token") {
            guard.refreshes += 1;
            (
                200,
                serde_json::json!({
                    "access_token": "access-refreshed",
                    "refresh_token": "refresh-1",
                    "expires_in": 3600
                })
                .to_string(),
            )
        } else if request_line.starts_with("POST /rest/v1/rpc/push_votes") {
            if let Ok(value) = serde_json::from_str::<serde_json::Value>(&body) {
                if let Some(rows) = value.get("rows").and_then(|r| r.as_array()) {
                    for row in rows {
                        let key = row.to_string();
                        if !guard.votes.iter().any(|v| v.to_string() == key) {
                            guard.votes.push(row.clone());
                        }
                    }
                }
            }
            (200, "{}".into())
        } else if request_line.starts_with("POST /rest/v1/rpc/push_uninstalls") {
            if let Ok(value) = serde_json::from_str::<serde_json::Value>(&body) {
                if let Some(rows) = value.get("rows").and_then(|r| r.as_array()) {
                    for row in rows {
                        let id = row.get("id").cloned().unwrap_or(serde_json::Value::Null);
                        if !guard.uninstalls.iter().any(|v| v.get("id") == Some(&id)) {
                            guard.uninstalls.push(row.clone());
                        }
                    }
                }
            }
            (200, "{}".into())
        } else if request_line.starts_with("GET /rest/v1/package_stats") {
            let suspicious = guard.votes.len() as u64;
            (
                200,
                serde_json::json!([{
                    "package_name": "com.app",
                    "uninstall_count": guard.uninstalls.len(),
                    "system_votes": 0,
                    "trusted_votes": 0,
                    "suspicious_votes": suspicious
                }])
                .to_string(),
            )
        } else {
            (404, "{}".into())
        };
        drop(guard);
        let raw = format!(
            "HTTP/1.1 {status} OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{response}",
            response.len()
        );
        stream.write_all(raw.as_bytes())?;
        Ok(())
    }
}
