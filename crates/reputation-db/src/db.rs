use crate::models::{
    PackageMark, PackageReputation, ReportEvent, ReputationSnapshot, UninstallEvent,
};
use chrono::Utc;
use rusqlite::{params, Connection};
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
}

pub struct ReputationDb {
    conn: Connection,
    path: PathBuf,
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
            std::fs::create_dir_all(parent)?;
        }
        let conn = Connection::open(&path)?;
        let db = Self {
            conn,
            path: path.clone(),
        };
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
            ",
        )?;
        self.ensure_column("packages", "marked_system", "INTEGER NOT NULL DEFAULT 0")?;
        self.ensure_column("packages", "marked_trusted", "INTEGER NOT NULL DEFAULT 0")?;
        let added_suspicious =
            self.ensure_column("packages", "marked_suspicious", "INTEGER NOT NULL DEFAULT 0")?;
        if added_suspicious {
            // Migrate legacy report_events → boolean flag (one report is enough).
            self.conn.execute(
                "UPDATE packages SET marked_suspicious = 1
                 WHERE package_name IN (SELECT DISTINCT package_name FROM report_events)",
                [],
            )?;
        }
        Ok(())
    }

    /// Returns `true` if the column was newly added.
    fn ensure_column(&self, table: &str, column: &str, definition: &str) -> Result<bool, DbError> {
        let mut stmt = self
            .conn
            .prepare(&format!("PRAGMA table_info({table})"))?;
        let exists = stmt
            .query_map([], |row| row.get::<_, String>(1))?
            .filter_map(|r| r.ok())
            .any(|name| name == column);
        if !exists {
            self.conn.execute(
                &format!("ALTER TABLE {table} ADD COLUMN {column} {definition}"),
                [],
            )?;
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
        Ok(event)
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
        Ok(event)
    }

    pub fn set_marked_system(&self, package_name: &str, value: bool) -> Result<(), DbError> {
        self.touch_package(package_name)?;
        self.conn.execute(
            "UPDATE packages SET marked_system = ?2 WHERE package_name = ?1",
            params![package_name, value as i32],
        )?;
        Ok(())
    }

    pub fn set_marked_trusted(&self, package_name: &str, value: bool) -> Result<(), DbError> {
        self.touch_package(package_name)?;
        self.conn.execute(
            "UPDATE packages SET marked_trusted = ?2 WHERE package_name = ?1",
            params![package_name, value as i32],
        )?;
        Ok(())
    }

    pub fn set_marked_suspicious(&self, package_name: &str, value: bool) -> Result<(), DbError> {
        self.touch_package(package_name)?;
        self.conn.execute(
            "UPDATE packages SET marked_suspicious = ?2 WHERE package_name = ?1",
            params![package_name, value as i32],
        )?;
        Ok(())
    }

    pub fn get_reputation(&self, package_name: &str) -> Result<PackageReputation, DbError> {
        let uninstall_count: u64 = self.conn.query_row(
            "SELECT COUNT(*) FROM uninstall_events WHERE package_name = ?1 AND success = 1",
            params![package_name],
            |row| row.get(0),
        )?;
        let report_count: u64 = self.conn.query_row(
            "SELECT COUNT(*) FROM report_events WHERE package_name = ?1",
            params![package_name],
            |row| row.get(0),
        )?;
        let (marked_system, marked_trusted, marked_suspicious) = self.load_marks_for(package_name)?;
        Ok(PackageReputation {
            package_name: package_name.to_string(),
            uninstall_count,
            report_count,
            marked_system,
            marked_trusted,
            marked_suspicious,
        })
    }

    pub fn all_reputations(&self) -> Result<Vec<PackageReputation>, DbError> {
        let mut stmt = self.conn.prepare(
            "SELECT package_name, marked_system, marked_trusted, marked_suspicious,
                    (SELECT COUNT(*) FROM uninstall_events u WHERE u.package_name = p.package_name AND u.success = 1),
                    (SELECT COUNT(*) FROM report_events r WHERE r.package_name = p.package_name)
             FROM packages p",
        )?;
        let rows = stmt.query_map([], |row| {
            Ok(PackageReputation {
                package_name: row.get(0)?,
                marked_system: row.get::<_, i32>(1)? != 0,
                marked_trusted: row.get::<_, i32>(2)? != 0,
                marked_suspicious: row.get::<_, i32>(3)? != 0,
                uninstall_count: row.get(4)?,
                report_count: row.get(5)?,
            })
        })?;
        rows.collect::<Result<Vec<_>, _>>().map_err(DbError::from)
    }

    fn load_marks_for(&self, package_name: &str) -> Result<(bool, bool, bool), DbError> {
        let result = self.conn.query_row(
            "SELECT marked_system, marked_trusted, marked_suspicious FROM packages WHERE package_name = ?1",
            params![package_name],
            |row| {
                Ok((
                    row.get::<_, i32>(0)? != 0,
                    row.get::<_, i32>(1)? != 0,
                    row.get::<_, i32>(2)? != 0,
                ))
            },
        );
        match result {
            Ok(marks) => Ok(marks),
            Err(rusqlite::Error::QueryReturnedNoRows) => Ok((false, false, false)),
            Err(e) => Err(DbError::from(e)),
        }
    }

    fn load_package_marks(&self) -> Result<Vec<PackageMark>, DbError> {
        let mut stmt = self.conn.prepare(
            "SELECT package_name, marked_system, marked_trusted, marked_suspicious FROM packages
             WHERE marked_system = 1 OR marked_trusted = 1 OR marked_suspicious = 1",
        )?;
        let rows = stmt.query_map([], |row| {
            Ok(PackageMark {
                package_name: row.get(0)?,
                marked_system: row.get::<_, i32>(1)? != 0,
                marked_trusted: row.get::<_, i32>(2)? != 0,
                marked_suspicious: row.get::<_, i32>(3)? != 0,
            })
        })?;
        rows.collect::<Result<Vec<_>, _>>().map_err(DbError::from)
    }

    pub fn export_snapshot(&self) -> Result<ReputationSnapshot, DbError> {
        let uninstall_events = self.load_uninstall_events()?;
        let report_events = self.load_report_events()?;
        let package_marks = self.load_package_marks()?;
        Ok(ReputationSnapshot {
            version: ReputationSnapshot::VERSION,
            exported_at: Utc::now(),
            uninstall_events,
            report_events,
            package_marks,
        })
    }

    pub fn merge_snapshot(&self, snapshot: &ReputationSnapshot) -> Result<(), DbError> {
        for event in &snapshot.uninstall_events {
            self.touch_package(&event.package_name)?;
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
            // Legacy snapshots only carried report events; treat any report as the bool flag.
            self.set_marked_suspicious(&event.package_name, true)?;
        }
        for mark in &snapshot.package_marks {
            self.touch_package(&mark.package_name)?;
            if mark.marked_system {
                self.set_marked_system(&mark.package_name, true)?;
            }
            if mark.marked_trusted {
                self.set_marked_trusted(&mark.package_name, true)?;
            }
            if mark.marked_suspicious {
                self.set_marked_suspicious(&mark.package_name, true)?;
            }
        }
        Ok(())
    }

    fn load_uninstall_events(&self) -> Result<Vec<UninstallEvent>, DbError> {
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

    fn load_report_events(&self) -> Result<Vec<ReportEvent>, DbError> {
        let mut stmt = self.conn.prepare(
            "SELECT id, package_name, device_serial, reported_at, reason FROM report_events",
        )?;
        let rows = stmt.query_map([], |row| {
            let ts: String = row.get(3)?;
            Ok(ReportEvent {
                id: row.get(0)?,
                package_name: row.get(1)?,
                device_serial: row.get(2)?,
                reported_at: ts.parse().unwrap_or_else(|_| Utc::now()),
                reason: row.get(4)?,
            })
        })?;
        rows.collect::<Result<Vec<_>, _>>().map_err(DbError::from)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn package_marks_persist_and_sync() {
        let db = ReputationDb::open(":memory:").unwrap();
        db.set_marked_system("com.android.settings", true).unwrap();
        db.set_marked_trusted("com.bank.app", true).unwrap();
        db.set_marked_suspicious("com.bad.app", true).unwrap();
        let rep = db.get_reputation("com.android.settings").unwrap();
        assert!(rep.marked_system);
        let trusted = db.get_reputation("com.bank.app").unwrap();
        assert!(trusted.marked_trusted);
        assert!(!trusted.is_suspicious());
        let bad = db.get_reputation("com.bad.app").unwrap();
        assert!(bad.marked_suspicious);
        assert!(bad.is_suspicious());
        db.set_marked_suspicious("com.bad.app", false).unwrap();
        assert!(!db.get_reputation("com.bad.app").unwrap().is_suspicious());
        for _ in 0..6 {
            db.record_uninstall("com.android.settings", "s1", true, None)
                .unwrap();
        }
        let system = db.get_reputation("com.android.settings").unwrap();
        assert_eq!(system.uninstall_count, 6);
        assert!(!system.is_suspicious());
        db.set_marked_suspicious("com.bad.app", true).unwrap();
        let snap = db.export_snapshot().unwrap();
        assert_eq!(snap.package_marks.len(), 3);
        let db2 = ReputationDb::open(":memory:").unwrap();
        db2.merge_snapshot(&snap).unwrap();
        assert!(db2.get_reputation("com.bank.app").unwrap().marked_trusted);
        assert!(db2.get_reputation("com.bad.app").unwrap().marked_suspicious);
    }

    #[test]
    fn merge_snapshot_deduplicates_events() {
        let db = ReputationDb::open(":memory:").unwrap();
        db.record_uninstall("com.evil.ads", "serial1", true, None)
            .unwrap();
        let snap = db.export_snapshot().unwrap();
        let db2 = ReputationDb::open(":memory:").unwrap();
        db2.merge_snapshot(&snap).unwrap();
        db2.merge_snapshot(&snap).unwrap();
        let rep = db2.get_reputation("com.evil.ads").unwrap();
        assert_eq!(rep.uninstall_count, 1);
    }
}
