use reputation_db::{FlagVote, RemoteStat, ReputationDb, UninstallEvent};
use serde::Deserialize;
use sha2::{Digest, Sha256};
use std::fs;
use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use crate::SyncError;

#[derive(Debug, Clone)]
pub struct SupabaseConfig {
    pub url: String,
    pub key: String,
    pub session_path: PathBuf,
}

pub fn config_from(url: &str, key: &str, session_path: PathBuf) -> Option<SupabaseConfig> {
    let url = url.trim().trim_end_matches('/').to_string();
    let key = key.trim().to_string();
    if url.is_empty() || key.is_empty() {
        return None;
    }
    Some(SupabaseConfig {
        url,
        key,
        session_path,
    })
}

pub fn phone_hash(serial: &str) -> String {
    let digest = Sha256::digest(serial.as_bytes());
    digest.iter().map(|b| format!("{b:02x}")).collect()
}

#[derive(Debug, Deserialize)]
struct AuthResponse {
    access_token: String,
    refresh_token: String,
    expires_in: i64,
}

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
struct Session {
    access_token: String,
    refresh_token: String,
    expires_at: i64,
}

#[derive(Debug, Deserialize)]
struct StatRow {
    package_name: String,
    uninstall_count: u64,
    system_votes: u64,
    trusted_votes: u64,
    suspicious_votes: u64,
}

fn agent() -> ureq::Agent {
    ureq::AgentBuilder::new()
        .timeout(Duration::from_secs(20))
        .build()
}

fn now_secs() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0)
}

fn load_session(path: &Path) -> Option<Session> {
    let data = fs::read_to_string(path).ok()?;
    serde_json::from_str(&data).ok()
}

fn save_session(path: &Path, session: &Session) -> Result<(), SyncError> {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)?;
    }
    fs::write(path, serde_json::to_string_pretty(session)?)?;
    Ok(())
}

fn session_from_auth(auth: AuthResponse) -> Session {
    Session {
        access_token: auth.access_token,
        refresh_token: auth.refresh_token,
        expires_at: now_secs() + auth.expires_in - 60,
    }
}

impl SupabaseConfig {
    fn auth_header_value(&self, token: &str) -> String {
        format!("Bearer {token}")
    }

    fn signup(&self) -> Result<Session, SyncError> {
        let auth: AuthResponse = agent()
            .post(&format!("{}/auth/v1/signup", self.url))
            .set("apikey", &self.key)
            .set("Authorization", &self.auth_header_value(&self.key))
            .set("Content-Type", "application/json")
            .send_json(serde_json::json!({}))
            .map_err(http_err)?
            .into_json()
            .map_err(|e| SyncError::Http(e.to_string()))?;
        let session = session_from_auth(auth);
        save_session(&self.session_path, &session)?;
        Ok(session)
    }

    fn refresh(&self, refresh_token: &str) -> Result<Session, SyncError> {
        let auth: AuthResponse = agent()
            .post(&format!(
                "{}/auth/v1/token?grant_type=refresh_token",
                self.url
            ))
            .set("apikey", &self.key)
            .set("Authorization", &self.auth_header_value(&self.key))
            .set("Content-Type", "application/json")
            .send_json(serde_json::json!({ "refresh_token": refresh_token }))
            .map_err(http_err)?
            .into_json()
            .map_err(|e| SyncError::Http(e.to_string()))?;
        let session = session_from_auth(auth);
        save_session(&self.session_path, &session)?;
        Ok(session)
    }

    pub fn access_token(&self) -> Result<String, SyncError> {
        if let Some(session) = load_session(&self.session_path) {
            if session.expires_at > now_secs() {
                return Ok(session.access_token);
            }
            match self.refresh(&session.refresh_token) {
                Ok(s) => return Ok(s.access_token),
                Err(_) => {}
            }
        }
        Ok(self.signup()?.access_token)
    }

    pub fn push_and_pull(&self, db: &ReputationDb) -> Result<(), SyncError> {
        let token = self.access_token()?;
        let uninstalls = db.load_uninstall_events()?;
        let votes = db.load_flag_votes()?;
        if !uninstalls.is_empty() {
            self.rpc(&token, "push_uninstalls", &uninstall_rows(&uninstalls))?;
        }
        if !votes.is_empty() {
            self.rpc(&token, "push_votes", &vote_rows(&votes))?;
        }
        let stats = self.fetch_stats(&token)?;
        db.replace_remote_stats(&stats)?;
        Ok(())
    }

    pub fn pull_stats(&self, db: &ReputationDb) -> Result<(), SyncError> {
        let token = self.access_token()?;
        let stats = self.fetch_stats(&token)?;
        db.replace_remote_stats(&stats)?;
        Ok(())
    }

    fn rpc(&self, token: &str, name: &str, rows: &serde_json::Value) -> Result<(), SyncError> {
        agent()
            .post(&format!("{}/rest/v1/rpc/{name}", self.url))
            .set("apikey", &self.key)
            .set("Authorization", &self.auth_header_value(token))
            .set("Content-Type", "application/json")
            .send_json(serde_json::json!({ "rows": rows }))
            .map_err(http_err)?;
        Ok(())
    }

    fn fetch_stats(&self, token: &str) -> Result<Vec<RemoteStat>, SyncError> {
        let rows: Vec<StatRow> = agent()
            .get(&format!("{}/rest/v1/package_stats?select=*", self.url))
            .set("apikey", &self.key)
            .set("Authorization", &self.auth_header_value(token))
            .call()
            .map_err(http_err)?
            .into_json()
            .map_err(|e| SyncError::Http(e.to_string()))?;
        Ok(rows
            .into_iter()
            .map(|row| RemoteStat {
                package_name: row.package_name,
                uninstall_count: row.uninstall_count,
                system_votes: row.system_votes,
                trusted_votes: row.trusted_votes,
                suspicious_votes: row.suspicious_votes,
            })
            .collect())
    }
}

fn uninstall_rows(events: &[UninstallEvent]) -> serde_json::Value {
    serde_json::json!(events
        .iter()
        .map(|e| {
            serde_json::json!({
                "id": e.id,
                "package_name": e.package_name,
                "phone_hash": phone_hash(&e.device_serial),
                "uninstalled_at": e.uninstalled_at.to_rfc3339(),
                "success": e.success,
                "notes": e.notes,
            })
        })
        .collect::<Vec<_>>())
}

fn vote_rows(votes: &[FlagVote]) -> serde_json::Value {
    serde_json::json!(votes
        .iter()
        .map(|v| {
            serde_json::json!({
                "package_name": v.package_name,
                "phone_hash": phone_hash(&v.device_serial),
                "flag": v.flag,
                "voted_at": v.voted_at.to_rfc3339(),
            })
        })
        .collect::<Vec<_>>())
}

fn http_err(err: ureq::Error) -> SyncError {
    match err {
        ureq::Error::Status(code, response) => {
            let body = response.into_string().unwrap_or_default();
            SyncError::Http(format!("HTTP {code}: {body}"))
        }
        other => SyncError::Http(other.to_string()),
    }
}
