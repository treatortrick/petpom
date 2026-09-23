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
             );
             -- 探险：投喂小鱼干 = 送它出门，到点回来带一张卡片（2026-09-23）
             -- card_key 在出发时就抽好并落库，避免重启/刷新重抽（否则可以刷稀有卡）
             CREATE TABLE IF NOT EXISTS expeditions (
               id         INTEGER PRIMARY KEY AUTOINCREMENT,
               started_at TEXT NOT NULL,
               due_unix   INTEGER NOT NULL,
               card_key   TEXT NOT NULL,
               returned   INTEGER NOT NULL DEFAULT 0,
               collected  INTEGER NOT NULL DEFAULT 0
             );
             -- 它的小窝：已经带回来的卡片
             CREATE TABLE IF NOT EXISTS cards (
               id          INTEGER PRIMARY KEY AUTOINCREMENT,
               card_key    TEXT NOT NULL,
               obtained_at TEXT NOT NULL,
               seen        INTEGER NOT NULL DEFAULT 0
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

    // ---------- 探险 / 小窝（2026-09-23） ----------

    /// 当前进行中的探险：(id, due_unix, card_key, started_at)。同时只允许一个。
    pub fn active_expedition(&self) -> Option<(i64, i64, String, String)> {
        let conn = self.conn.lock().unwrap();
        conn.query_row(
            "SELECT id, due_unix, card_key, started_at FROM expeditions
             WHERE returned = 0 ORDER BY id DESC LIMIT 1",
            [],
            |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?)),
        )
        .optional()
        .ok()
        .flatten()
    }

    /// 出发：记一条探险（card_key 此刻已抽定，due_unix 为到点时间）
    pub fn start_expedition(&self, started_at: &str, due_unix: i64, card_key: &str) {
        let conn = self.conn.lock().unwrap();
        let _ = conn.execute(
            "INSERT INTO expeditions(started_at, due_unix, card_key) VALUES(?1, ?2, ?3)",
            rusqlite::params![started_at, due_unix, card_key],
        );
    }

    /// 已到点但还没被结算的探险（含 app 未运行期间到点的，用于离线结算）
    pub fn due_expeditions(&self, now_unix: i64) -> Vec<(i64, String)> {
        let conn = self.conn.lock().unwrap();
        let mut st = match conn.prepare(
            "SELECT id, card_key FROM expeditions WHERE returned = 0 AND due_unix <= ?1
             ORDER BY id ASC",
        ) {
            Ok(s) => s,
            Err(_) => return Vec::new(),
        };
        let rows = st.query_map([now_unix], |r| Ok((r.get(0)?, r.get(1)?)));
        match rows {
            Ok(it) => it.filter_map(|x| x.ok()).collect(),
            Err(_) => Vec::new(),
        }
    }

    /// 到点结算：标记 returned 并把卡片收进小窝（seen=0，等主人看）
    pub fn return_expedition(&self, id: i64, card_key: &str, obtained_at: &str) {
        let conn = self.conn.lock().unwrap();
        let _ = conn.execute(
            "UPDATE expeditions SET returned = 1, collected = 1 WHERE id = ?1",
            [id],
        );
        let _ = conn.execute(
            "INSERT INTO cards(card_key, obtained_at) VALUES(?1, ?2)",
            rusqlite::params![card_key, obtained_at],
        );
    }

    /// 小窝内容：(card_key, obtained_at, seen)
    pub fn den_cards(&self) -> Vec<(String, String, i64)> {
        let conn = self.conn.lock().unwrap();
        let mut st = match conn.prepare(
            "SELECT card_key, obtained_at, seen FROM cards ORDER BY id DESC",
        ) {
            Ok(s) => s,
            Err(_) => return Vec::new(),
        };
        let rows = st.query_map([], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)));
        match rows {
            Ok(it) => it.filter_map(|x| x.ok()).collect(),
            Err(_) => Vec::new(),
        }
    }

    pub fn mark_den_seen(&self) {
        let conn = self.conn.lock().unwrap();
        let _ = conn.execute("UPDATE cards SET seen = 1 WHERE seen = 0", []);
    }

    pub fn unseen_count(&self) -> i64 {
        let conn = self.conn.lock().unwrap();
        conn.query_row("SELECT COUNT(*) FROM cards WHERE seen = 0", [], |r| r.get(0))
            .unwrap_or(0)
    }
}
