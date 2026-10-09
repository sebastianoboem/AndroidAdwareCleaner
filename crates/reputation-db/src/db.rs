use crate::models::{
    effective_marks, FlagVote, PackageReputation, RemoteStat, ReportEvent,
    ReputationSnapshot, UninstallEvent, VoteFlag, LEGACY_DEVICE,
};
use chrono::Utc;
use rusqlite::{params, Connection};
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use thiserror::Error;
use uuid::Uuid;

#[derive(Debug, Error)]
pub enum DbError {
    #[error(transparent)]
    Sqlite(#[from] rusqlite::Error),
    #[error(transparent)]
    Json(#[from] serde_json::Error),
    #[error("io error: {0}")]
    Io(#[from] std::io::Error),
    #[error("flag non valido")]
    BadFlag,
}

pub struct ReputationDb {
    conn: Connection,
    path: PathBuf,
}

struct Counts {
    uninstalls: u64,
    system: u64,
    trusted: u64,
    suspicious: u64,
}

impl ReputationDb {
    pub fn open_default() -> Result<Self, DbError> {
        let dir = dirs::data_dir()
            .unwrap_or_else(|| PathBuf::from("."))
            .join("AndroidAdwareCleaner");
        std::fs::create_dir_all(&dir)?;
        Self::open(dir.join("reputation.db"))
    }

    pub fn open(path: impl AsRef<Path>) -> Result<Self, DbError> {
        let path = path.as_ref().to_path_buf();
        if let Some(parent) = path.parent() {
            if !parent.as_os_str().is_empty() {
                std::fs::create_dir_all(parent)?;
            }
        }
        let conn = Connection::open(&path)?;
        let db = Self { conn, path };
        db.migrate()?;
        Ok(db)
    }

    fn migrate(&self) -> Result<(), DbError> {
        self.conn.execute_batch(
            "
            CREATE TABLE IF NOT EXISTS packages (
                package_name TEXT PRIMARY KEY,
                first_seen_at TEXT NOT NULL,
                last_seen_at TEXT NOT NULL
            );
            CREATE TABLE IF NOT EXISTS uninstall_events (
                id TEXT PRIMARY KEY,
                package_name TEXT NOT NULL,
                device_serial TEXT NOT NULL,
                uninstalled_at TEXT NOT NULL,
                success INTEGER NOT NULL,
                notes TEXT
            );
            CREATE TABLE IF NOT EXISTS report_events (
                id TEXT PRIMARY KEY,
                package_name TEXT NOT NULL,
                device_serial TEXT NOT NULL,
                reported_at TEXT NOT NULL,
                reason TEXT
            );
            CREATE TABLE IF NOT EXISTS flag_votes (
                package_name TEXT NOT NULL,
                device_serial TEXT NOT NULL,
                flag TEXT NOT NULL,
                voted_at TEXT NOT NULL,
                PRIMARY KEY (package_name, device_serial)
            );
            CREATE TABLE IF NOT EXISTS remote_stats (
                package_name TEXT PRIMARY KEY,
                uninstall_count INTEGER NOT NULL,
                system_votes INTEGER NOT NULL,
                trusted_votes INTEGER NOT NULL,
                suspicious_votes INTEGER NOT NULL
            );
            ",
        )?;
        self.ensure_column("packages", "marked_system", "INTEGER NOT NULL DEFAULT 0")?;
        self.ensure_column("packages", "marked_trusted", "INTEGER NOT NULL DEFAULT 0")?;
        let added_suspicious =
            self.ensure_column("packages", "marked_suspicious", "INTEGER NOT NULL DEFAULT 0")?;
        if added_suspicious {
            self.conn.execute(
                "UPDATE packages SET marked_suspicious = 1
                 WHERE package_name IN (SELECT DISTINCT package_name FROM report_events)",
                [],
            )?;
        }
        // Vecchi bool → un voto del telefono fittizio `legacy`. INSERT OR IGNORE: non si ripete.
        self.conn.execute(
            "INSERT OR IGNORE INTO flag_votes (package_name, device_serial, flag, voted_at)
             SELECT package_name, ?1,
               CASE
                 WHEN marked_system = 1 THEN 'system'
                 WHEN marked_trusted = 1 THEN 'trusted'
                 ELSE 'suspicious'
               END,
               last_seen_at
             FROM packages
             WHERE marked_system = 1 OR marked_trusted = 1 OR marked_suspicious = 1",
            params![LEGACY_DEVICE],
        )?;
        Ok(())
    }

    /// Returns `true` if the column was newly added.
    fn ensure_column(&self, table: &str, column: &str, definition: &str) -> Result<bool, DbError> {
        let mut stmt = self.conn.prepare(&format!("PRAGMA table_info({table})"))?;
        let exists = stmt
            .query_map([], |row| row.get::<_, String>(1))?
            .filter_map(|r| r.ok())
            .any(|name| name == column);
        if !exists {
            self.conn
                .execute(&format!("ALTER TABLE {table} ADD COLUMN {column} {definition}"), [])?;
            return Ok(true);
        }
        Ok(false)
    }

    pub fn path(&self) -> &Path {
        &self.path
    }

    pub fn touch_package(&self, package_name: &str) -> Result<(), DbError> {
        let now = Utc::now().to_rfc3339();
        self.conn.execute(
            "INSERT INTO packages (package_name, first_seen_at, last_seen_at)
             VALUES (?1, ?2, ?2)
             ON CONFLICT(package_name) DO UPDATE SET last_seen_at = ?2",
            params![package_name, now],
        )?;
        Ok(())
    }

    pub fn record_uninstall(
        &self,
        package_name: &str,
        device_serial: &str,
        success: bool,
        notes: Option<&str>,
    ) -> Result<UninstallEvent, DbError> {
        self.touch_package(package_name)?;
        let event = UninstallEvent {
            id: Uuid::new_v4().to_string(),
            package_name: package_name.to_string(),
            device_serial: device_serial.to_string(),
            uninstalled_at: Utc::now(),
            success,
            notes: notes.map(str::to_string),
        };
        self.insert_uninstall(&event)?;
        Ok(event)
    }

    fn insert_uninstall(&self, event: &UninstallEvent) -> Result<(), DbError> {
        self.conn.execute(
            "INSERT OR IGNORE INTO uninstall_events (id, package_name, device_serial, uninstalled_at, success, notes)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
            params![
                event.id,
                event.package_name,
                event.device_serial,
                event.uninstalled_at.to_rfc3339(),
                event.success as i32,
                event.notes,
            ],
        )?;
        Ok(())
    }

    pub fn record_report(
        &self,
        package_name: &str,
        device_serial: &str,
        reason: Option<&str>,
    ) -> Result<ReportEvent, DbError> {
        self.touch_package(package_name)?;
        let event = ReportEvent {
            id: Uuid::new_v4().to_string(),
            package_name: package_name.to_string(),
            device_serial: device_serial.to_string(),
            reported_at: Utc::now(),
            reason: reason.map(str::to_string),
        };
        self.conn.execute(
            "INSERT OR IGNORE INTO report_events (id, package_name, device_serial, reported_at, reason)
             VALUES (?1, ?2, ?3, ?4, ?5)",
            params![
                event.id,
                event.package_name,
                event.device_serial,
                event.reported_at.to_rfc3339(),
                event.reason,
            ],
        )?;
        // Le segnalazioni vecchie contano come voto sospetta di quel telefono, senza coprire un voto già presente.
        self.conn.execute(
            "INSERT OR IGNORE INTO flag_votes (package_name, device_serial, flag, voted_at)
             VALUES (?1, ?2, 'suspicious', ?3)",
            params![
                event.package_name,
                event.device_serial,
                event.reported_at.to_rfc3339()
            ],
        )?;
        Ok(event)
    }

    pub fn set_vote(&self, package_name: &str, device_serial: &str, flag: &str) -> Result<(), DbError> {
        if device_serial.is_empty() || VoteFlag::parse(flag).is_none() {
            return Err(DbError::BadFlag);
        }
        self.touch_package(package_name)?;
        self.upsert_vote(&FlagVote {
            package_name: package_name.to_string(),
            device_serial: device_serial.to_string(),
            flag: flag.to_string(),
            voted_at: Utc::now(),
        })?;
        Ok(())
    }

    fn upsert_vote(&self, vote: &FlagVote) -> Result<(), DbError> {
        self.conn.execute(
            "INSERT INTO flag_votes (package_name, device_serial, flag, voted_at)
             VALUES (?1, ?2, ?3, ?4)
             ON CONFLICT(package_name, device_serial) DO UPDATE
               SET flag = excluded.flag, voted_at = excluded.voted_at
               WHERE flag_votes.voted_at < excluded.voted_at",
            params![
                vote.package_name,
                vote.device_serial,
                vote.flag,
                vote.voted_at.to_rfc3339(),
            ],
        )?;
        Ok(())
    }

    pub fn replace_remote_stats(&self, stats: &[RemoteStat]) -> Result<(), DbError> {
        self.conn.execute("DELETE FROM remote_stats", [])?;
        for stat in stats {
            self.conn.execute(
                "INSERT INTO remote_stats (package_name, uninstall_count, system_votes, trusted_votes, suspicious_votes)
                 VALUES (?1, ?2, ?3, ?4, ?5)",
                params![
                    stat.package_name,
                    stat.uninstall_count as i64,
                    stat.system_votes as i64,
                    stat.trusted_votes as i64,
                    stat.suspicious_votes as i64,
                ],
            )?;
        }
        Ok(())
    }

    pub fn get_reputation(
        &self,
        package_name: &str,
        viewer_serial: Option<&str>,
    ) -> Result<PackageReputation, DbError> {
        let mut reps = self.reputations_for(&[package_name.to_string()], viewer_serial)?;
        Ok(reps.pop().unwrap_or_else(|| empty_rep(package_name, None)))
    }

    pub fn all_reputations(&self, viewer_serial: Option<&str>) -> Result<Vec<PackageReputation>, DbError> {
        let mut names = Vec::new();
        for table in ["packages", "flag_votes", "remote_stats", "uninstall_events"] {
            let mut stmt = self
                .conn
                .prepare(&format!("SELECT DISTINCT package_name FROM {table}"))?;
            let rows = stmt.query_map([], |row| row.get::<_, String>(0))?;
            for name in rows {
                names.push(name?);
            }
        }
        names.sort();
        names.dedup();
        self.reputations_for(&names, viewer_serial)
    }

    fn reputations_for(
        &self,
        names: &[String],
        viewer_serial: Option<&str>,
    ) -> Result<Vec<PackageReputation>, DbError> {
        let local = self.local_counts()?;
        let remote = self.remote_counts()?;
        let mine = self.my_votes(viewer_serial)?;
        let mut out = Vec::with_capacity(names.len());
        for name in names {
            let local = local.get(name);
            let remote = remote.get(name);
            let uninstalls = local
                .map(|c| c.uninstalls)
                .unwrap_or(0)
                .max(remote.map(|c| c.uninstalls).unwrap_or(0));
            let system = local
                .map(|c| c.system)
                .unwrap_or(0)
                .max(remote.map(|c| c.system).unwrap_or(0));
            let trusted = local
                .map(|c| c.trusted)
                .unwrap_or(0)
                .max(remote.map(|c| c.trusted).unwrap_or(0));
            let suspicious = local
                .map(|c| c.suspicious)
                .unwrap_or(0)
                .max(remote.map(|c| c.suspicious).unwrap_or(0));
            let (marked_system, marked_trusted, marked_suspicious) =
                effective_marks(system, trusted, suspicious);
            let my_vote = mine.get(name).cloned().filter(|f| f != "none");
            out.push(PackageReputation {
                package_name: name.clone(),
                uninstall_count: uninstalls,
                report_count: suspicious,
                marked_system,
                marked_trusted,
                marked_suspicious,
                my_vote,
            });
        }
        Ok(out)
    }

    fn local_counts(&self) -> Result<HashMap<String, Counts>, DbError> {
        let mut map: HashMap<String, Counts> = HashMap::new();
        let mut stmt = self.conn.prepare(
            "SELECT package_name, flag, COUNT(DISTINCT device_serial)
             FROM flag_votes
             WHERE flag != 'none'
             GROUP BY package_name, flag",
        )?;
        let rows = stmt.query_map([], |row| {
            Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?, row.get::<_, u64>(2)?))
        })?;
        for row in rows {
            let (name, flag, n) = row?;
            let entry = map.entry(name).or_insert(Counts {
                uninstalls: 0,
                system: 0,
                trusted: 0,
                suspicious: 0,
            });
            match flag.as_str() {
                "system" => entry.system = n,
                "trusted" => entry.trusted = n,
                "suspicious" => entry.suspicious = n,
                _ => {}
            }
        }
        let mut stmt = self.conn.prepare(
            "SELECT package_name, COUNT(*) FROM uninstall_events WHERE success = 1 GROUP BY package_name",
        )?;
        let rows = stmt.query_map([], |row| Ok((row.get::<_, String>(0)?, row.get::<_, u64>(1)?)))?;
        for row in rows {
            let (name, n) = row?;
            map.entry(name).or_insert(Counts {
                uninstalls: 0,
                system: 0,
                trusted: 0,
                suspicious: 0,
            }).uninstalls = n;
        }
        Ok(map)
    }

    fn remote_counts(&self) -> Result<HashMap<String, Counts>, DbError> {
        let mut stmt = self.conn.prepare(
            "SELECT package_name, uninstall_count, system_votes, trusted_votes, suspicious_votes FROM remote_stats",
        )?;
        let rows = stmt.query_map([], |row| {
            Ok((
                row.get::<_, String>(0)?,
                Counts {
                    uninstalls: row.get(1)?,
                    system: row.get(2)?,
                    trusted: row.get(3)?,
                    suspicious: row.get(4)?,
                },
            ))
        })?;
        let mut map = HashMap::new();
        for row in rows {
            let (name, counts) = row?;
            map.insert(name, counts);
        }
        Ok(map)
    }

    fn my_votes(&self, serial: Option<&str>) -> Result<HashMap<String, String>, DbError> {
        let Some(serial) = serial.filter(|s| !s.is_empty()) else {
            return Ok(HashMap::new());
        };
        let mut stmt = self.conn.prepare(
            "SELECT package_name, flag FROM flag_votes WHERE device_serial = ?1",
        )?;
        let rows = stmt.query_map(params![serial], |row| {
            Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?))
        })?;
        let mut map = HashMap::new();
        for row in rows {
            let (name, flag) = row?;
            map.insert(name, flag);
        }
        Ok(map)
    }

    pub fn export_snapshot(&self) -> Result<ReputationSnapshot, DbError> {
        Ok(ReputationSnapshot {
            version: ReputationSnapshot::VERSION,
            exported_at: Utc::now(),
            uninstall_events: self.load_uninstall_events()?,
            report_events: Vec::new(),
            package_marks: Vec::new(),
            flag_votes: self.load_flag_votes()?,
        })
    }

    pub fn load_uninstall_events(&self) -> Result<Vec<UninstallEvent>, DbError> {
        let mut stmt = self.conn.prepare(
            "SELECT id, package_name, device_serial, uninstalled_at, success, notes FROM uninstall_events",
        )?;
        let rows = stmt.query_map([], |row| {
            let ts: String = row.get(3)?;
            Ok(UninstallEvent {
                id: row.get(0)?,
                package_name: row.get(1)?,
                device_serial: row.get(2)?,
                uninstalled_at: ts.parse().unwrap_or_else(|_| Utc::now()),
                success: row.get::<_, i32>(4)? != 0,
                notes: row.get(5)?,
            })
        })?;
        rows.collect::<Result<Vec<_>, _>>().map_err(DbError::from)
    }

    pub fn load_flag_votes(&self) -> Result<Vec<FlagVote>, DbError> {
        let mut stmt = self.conn.prepare(
            "SELECT package_name, device_serial, flag, voted_at FROM flag_votes",
        )?;
        let rows = stmt.query_map([], |row| {
            let ts: String = row.get(3)?;
            Ok(FlagVote {
                package_name: row.get(0)?,
                device_serial: row.get(1)?,
                flag: row.get(2)?,
                voted_at: ts.parse().unwrap_or_else(|_| Utc::now()),
            })
        })?;
        rows.collect::<Result<Vec<_>, _>>().map_err(DbError::from)
    }

    pub fn merge_snapshot(&self, snapshot: &ReputationSnapshot) -> Result<(), DbError> {
        for event in &snapshot.uninstall_events {
            self.touch_package(&event.package_name)?;
            self.insert_uninstall(event)?;
        }
        for event in &snapshot.report_events {
            self.touch_package(&event.package_name)?;
            self.conn.execute(
                "INSERT OR IGNORE INTO report_events (id, package_name, device_serial, reported_at, reason)
                 VALUES (?1, ?2, ?3, ?4, ?5)",
                params![
                    event.id,
                    event.package_name,
                    event.device_serial,
                    event.reported_at.to_rfc3339(),
                    event.reason,
                ],
            )?;
            self.conn.execute(
                "INSERT OR IGNORE INTO flag_votes (package_name, device_serial, flag, voted_at)
                 VALUES (?1, ?2, 'suspicious', ?3)",
                params![
                    event.package_name,
                    event.device_serial,
                    event.reported_at.to_rfc3339()
                ],
            )?;
        }
        for mark in &snapshot.package_marks {
            let flag = if mark.marked_system {
                "system"
            } else if mark.marked_trusted {
                "trusted"
            } else if mark.marked_suspicious {
                "suspicious"
            } else {
                continue;
            };
            self.touch_package(&mark.package_name)?;
            self.conn.execute(
                "INSERT OR IGNORE INTO flag_votes (package_name, device_serial, flag, voted_at)
                 VALUES (?1, ?2, ?3, ?4)",
                params![
                    mark.package_name,
                    LEGACY_DEVICE,
                    flag,
                    snapshot.exported_at.to_rfc3339()
                ],
            )?;
        }
        for vote in &snapshot.flag_votes {
            if VoteFlag::parse(&vote.flag).is_none() {
                continue;
            }
            self.touch_package(&vote.package_name)?;
            self.upsert_vote(vote)?;
        }
        Ok(())
    }
}

fn empty_rep(package_name: &str, my_vote: Option<String>) -> PackageReputation {
    PackageReputation {
        package_name: package_name.to_string(),
        uninstall_count: 0,
        report_count: 0,
        marked_system: false,
        marked_trusted: false,
        marked_suspicious: false,
        my_vote,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::models::PackageMark;

    #[test]
    fn votes_roundtrip_and_legacy_marks() {
        let db = ReputationDb::open(":memory:").unwrap();
        db.set_vote("com.android.settings", "phone-a", "system").unwrap();
        db.set_vote("com.bank.app", "phone-a", "trusted").unwrap();
        db.set_vote("com.bad.app", "phone-a", "suspicious").unwrap();
        let rep = db.get_reputation("com.android.settings", Some("phone-a")).unwrap();
        assert!(rep.marked_system);
        assert_eq!(rep.my_vote.as_deref(), Some("system"));
        let trusted = db.get_reputation("com.bank.app", Some("phone-a")).unwrap();
        assert!(trusted.marked_trusted);
        assert!(!trusted.is_suspicious());
        let bad = db.get_reputation("com.bad.app", Some("phone-a")).unwrap();
        assert!(bad.marked_suspicious);
        assert!(bad.is_suspicious());
        db.set_vote("com.bad.app", "phone-a", "none").unwrap();
        assert!(!db.get_reputation("com.bad.app", Some("phone-a")).unwrap().is_suspicious());
        for _ in 0..6 {
            db.record_uninstall("com.android.settings", "phone-a", true, None)
                .unwrap();
        }
        let system = db.get_reputation("com.android.settings", None).unwrap();
        assert_eq!(system.uninstall_count, 6);
        assert!(!system.is_suspicious());
        let snap = db.export_snapshot().unwrap();
        assert_eq!(snap.version, 3);
        assert!(snap.flag_votes.len() >= 3);
        let db2 = ReputationDb::open(":memory:").unwrap();
        db2.merge_snapshot(&snap).unwrap();
        assert!(db2.get_reputation("com.bank.app", Some("phone-a")).unwrap().marked_trusted);
        assert_eq!(
            db2.get_reputation("com.bad.app", Some("phone-a")).unwrap().my_vote.as_deref(),
            None
        );
    }

    #[test]
    fn merge_snapshot_deduplicates_events() {
        let db = ReputationDb::open(":memory:").unwrap();
        db.record_uninstall("com.evil.ads", "serial1", true, None).unwrap();
        let snap = db.export_snapshot().unwrap();
        let db2 = ReputationDb::open(":memory:").unwrap();
        db2.merge_snapshot(&snap).unwrap();
        db2.merge_snapshot(&snap).unwrap();
        let rep = db2.get_reputation("com.evil.ads", None).unwrap();
        assert_eq!(rep.uninstall_count, 1);
    }

    #[test]
    fn suspicious_threshold_overrides_higher_flags() {
        let db = ReputationDb::open(":memory:").unwrap();
        db.set_vote("com.app", "keeper", "trusted").unwrap();
        for i in 0..4 {
            db.set_vote("com.app", &format!("phone-{i}"), "suspicious").unwrap();
        }
        let below = db.get_reputation("com.app", None).unwrap();
        assert!(below.marked_trusted);
        assert!(!below.marked_suspicious);
        assert_eq!(below.report_count, 4);
        db.set_vote("com.app", "phone-4", "suspicious").unwrap();
        let at = db.get_reputation("com.app", None).unwrap();
        assert!(at.marked_suspicious);
        assert!(!at.marked_trusted);
        assert!(!at.marked_system);
        assert!(at.is_suspicious());

        db.set_vote("com.sys", "keeper", "system").unwrap();
        for i in 0..5 {
            db.set_vote("com.sys", &format!("phone-{i}"), "suspicious").unwrap();
        }
        let sys = db.get_reputation("com.sys", None).unwrap();
        assert!(sys.marked_suspicious);
        assert!(!sys.marked_system);
    }

    #[test]
    fn withdrawn_vote_does_not_count_and_newer_vote_wins() {
        let db = ReputationDb::open(":memory:").unwrap();
        db.set_vote("com.app", "phone-a", "suspicious").unwrap();
        db.set_vote("com.app", "phone-a", "none").unwrap();
        let rep = db.get_reputation("com.app", Some("phone-a")).unwrap();
        assert_eq!(rep.report_count, 0);
        assert!(rep.my_vote.is_none());

        let older = db.export_snapshot().unwrap();
        db.set_vote("com.app", "phone-a", "trusted").unwrap();
        let newer = db.export_snapshot().unwrap();
        let other = ReputationDb::open(":memory:").unwrap();
        other.set_vote("com.app", "phone-b", "system").unwrap();
        other.merge_snapshot(&older).unwrap();
        other.merge_snapshot(&newer).unwrap();
        let merged = other.get_reputation("com.app", Some("phone-a")).unwrap();
        assert_eq!(merged.my_vote.as_deref(), Some("trusted"));
        assert!(other.get_reputation("com.app", Some("phone-b")).unwrap().marked_system || merged.marked_trusted);
        assert!(other.get_reputation("com.app", Some("phone-b")).unwrap().my_vote.as_deref() == Some("system"));
    }

    #[test]
    fn same_phone_twice_counts_once_and_remote_max() {
        let db = ReputationDb::open(":memory:").unwrap();
        db.set_vote("com.app", "phone-a", "suspicious").unwrap();
        db.set_vote("com.app", "phone-a", "suspicious").unwrap();
        assert_eq!(db.get_reputation("com.app", None).unwrap().report_count, 1);
        db.replace_remote_stats(&[RemoteStat {
            package_name: "com.app".into(),
            uninstall_count: 0,
            system_votes: 0,
            trusted_votes: 1,
            suspicious_votes: 5,
        }])
        .unwrap();
        let rep = db.get_reputation("com.app", None).unwrap();
        assert_eq!(rep.report_count, 5);
        assert!(rep.marked_suspicious);
        assert!(!rep.marked_trusted);
    }

    #[test]
    fn v2_marks_import_as_legacy_vote() {
        let snap = ReputationSnapshot {
            version: 2,
            exported_at: Utc::now(),
            uninstall_events: vec![],
            report_events: vec![],
            package_marks: vec![PackageMark {
                package_name: "com.bank.app".into(),
                marked_system: false,
                marked_trusted: true,
                marked_suspicious: true,
            }],
            flag_votes: vec![],
        };
        let db = ReputationDb::open(":memory:").unwrap();
        db.merge_snapshot(&snap).unwrap();
        let rep = db.get_reputation("com.bank.app", Some(LEGACY_DEVICE)).unwrap();
        assert!(rep.marked_trusted);
        assert!(!rep.marked_suspicious);
        assert_eq!(rep.my_vote.as_deref(), Some("trusted"));
    }
}
