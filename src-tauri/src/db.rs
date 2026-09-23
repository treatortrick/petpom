use rusqlite::{Connection, OptionalExtension};
use std::path::Path;
use std::sync::Mutex;

pub struct Db {
    conn: Mutex<Connection>,
}

impl Db {
    pub fn open(path: &Path) -> Result<Self, rusqlite::Error> {
        let conn = Connection::open(path)?;
        conn.execute_batch(
            "PRAGMA journal_mode=WAL;
             CREATE TABLE IF NOT EXISTS settings (
               key   TEXT PRIMARY KEY,
               value TEXT NOT NULL
             );
             CREATE TABLE IF NOT EXISTS rounds (
               id         INTEGER PRIMARY KEY AUTOINCREMENT,
               started_at TEXT NOT NULL,
               ended_at   TEXT NOT NULL,
               focus_min  INTEGER NOT NULL,
               snoozes    INTEGER NOT NULL DEFAULT 0,
               result     TEXT NOT NULL
             );",
        )?;
        Ok(Db { conn: Mutex::new(conn) })
    }

    pub fn get_setting(&self, key: &str) -> Option<String> {
        let conn = self.conn.lock().unwrap();
        conn.query_row(
            "SELECT value FROM settings WHERE key = ?1",
            [key],
            |r| r.get(0),
        )
        .optional()
        .ok()
        .flatten()
    }

    pub fn set_setting(&self, key: &str, value: &str) {
        let conn = self.conn.lock().unwrap();
        let _ = conn.execute(
            "INSERT INTO settings(key, value) VALUES(?1, ?2)
             ON CONFLICT(key) DO UPDATE SET value = excluded.value",
            [key, value],
        );
    }

    /// 记录一轮专注的结果：result ∈ 完成/婉拒/忽略/跳过/手动停止
    /// started_unix：本轮专注开始墙钟时间（Unix 秒）；0 = 未知（回退为结束时刻）
    /// 2026-09-22 修复：此前 started_at/ended_at 写同一时刻，无法追溯真实开始时间
    pub fn insert_round(&self, started_unix: i64, focus_min: i64, snoozes: i64, result: &str) {
        let conn = self.conn.lock().unwrap();
        let ended = chrono::Local::now();
        let started = if started_unix > 0 {
            chrono::DateTime::from_timestamp(started_unix, 0)
                .map(|t| t.with_timezone(&chrono::Local))
                .unwrap_or(ended)
        } else {
            ended
        };
        let _ = conn.execute(
            "INSERT INTO rounds(started_at, ended_at, focus_min, snoozes, result)
             VALUES(?1, ?2, ?3, ?4, ?5)",
            rusqlite::params![
                started.format("%Y-%m-%d %H:%M:%S").to_string(),
                ended.format("%Y-%m-%d %H:%M:%S").to_string(),
                focus_min,
                snoozes,
                result
            ],
        );
    }

    /// 今日战绩：(完成轮数, 累计专注分钟)。只统计 result='完成' 的轮次。
    pub fn stats_today(&self) -> (i64, i64) {
        let conn = self.conn.lock().unwrap();
        conn.query_row(
            "SELECT COUNT(*), COALESCE(SUM(focus_min), 0) FROM rounds
             WHERE result = '完成' AND date(ended_at) = date('now', 'localtime')",
            [],
            |r| Ok((r.get(0)?, r.get(1)?)),
        )
        .unwrap_or((0, 0))
    }
}
