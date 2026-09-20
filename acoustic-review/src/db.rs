//! SQLite storage. Nothing here mutates an original observation or an older
//! candidate: reviewer decisions are append-only derived evidence and every
//! solve writes a fresh candidate set tied to a new job row.

use rusqlite::Connection;
use std::sync::Mutex;

pub struct Db {
    pub conn: Mutex<Connection>,
}

const SCHEMA: &str = r#"
PRAGMA journal_mode = WAL;
PRAGMA foreign_keys = ON;

CREATE TABLE IF NOT EXISTS stations (
    station_id   TEXT PRIMARY KEY,
    name         TEXT NOT NULL,
    x            REAL NOT NULL,
    y            REAL NOT NULL,
    created_at   REAL NOT NULL
);

-- Clock model: each collector owns non-overlapping, half-open segments
-- [start_s, end_s). `offset_s` is added to the device clock to obtain site
-- time. Two segments that merely touch never overlap, so at the touching
-- instant only the later one is in effect. end_s IS NULL means open-ended.
CREATE TABLE IF NOT EXISTS clock_segments (
    seg_id       INTEGER PRIMARY KEY AUTOINCREMENT,
    station_id   TEXT NOT NULL REFERENCES stations(station_id),
    start_s      REAL NOT NULL,
    end_s        REAL,
    offset_s     REAL NOT NULL,
    sigma_s      REAL NOT NULL DEFAULT 0.002,
    note         TEXT NOT NULL DEFAULT '',
    CHECK (end_s IS NULL OR end_s > start_s),
    UNIQUE (station_id, start_s)
);

CREATE TABLE IF NOT EXISTS batches (
    fingerprint  TEXT PRIMARY KEY,
    received_at  REAL NOT NULL,
    payload_json TEXT NOT NULL
);

CREATE TABLE IF NOT EXISTS observations (
    obs_id        INTEGER PRIMARY KEY AUTOINCREMENT,
    batch_fp      TEXT NOT NULL REFERENCES batches(fingerprint),
    station_id    TEXT NOT NULL REFERENCES stations(station_id),
    event_ref     TEXT,
    t_device      REAL NOT NULL,
    local_onset_s REAL NOT NULL,
    peak_band_hz  REAL NOT NULL,
    snr_db        REAL NOT NULL,
    created_seq   INTEGER NOT NULL
);

CREATE INDEX IF NOT EXISTS idx_obs_station_time ON observations(station_id, local_onset_s);
CREATE INDEX IF NOT EXISTS idx_obs_event ON observations(event_ref);

CREATE TABLE IF NOT EXISTS xcorr_delays (
    xc_id         INTEGER PRIMARY KEY AUTOINCREMENT,
    obs_a         INTEGER NOT NULL REFERENCES observations(obs_id),
    obs_b         INTEGER NOT NULL REFERENCES observations(obs_id),
    delay_s       REAL NOT NULL,             -- t_b - t_a on corrected time
    sigma_s       REAL NOT NULL,
    peak_index    INTEGER NOT NULL DEFAULT 0,-- 0 = direct, >0 = reflected peak
    multipath     INTEGER NOT NULL DEFAULT 0,
    CHECK (obs_a <> obs_b)
);

CREATE INDEX IF NOT EXISTS idx_xc_a ON xcorr_delays(obs_a);
CREATE INDEX IF NOT EXISTS idx_xc_b ON xcorr_delays(obs_b);

CREATE TABLE IF NOT EXISTS events (
    event_id      TEXT PRIMARY KEY,
    title         TEXT NOT NULL,
    t_start       REAL NOT NULL,
    t_end         REAL NOT NULL,
    sound_speed   REAL NOT NULL,
    coord_unit    TEXT NOT NULL DEFAULT 'm',
    time_unit     TEXT NOT NULL DEFAULT 's',
    created_at    REAL NOT NULL,
    CHECK (t_end > t_start)
);

CREATE TABLE IF NOT EXISTS event_observations (
    event_id      TEXT NOT NULL REFERENCES events(event_id),
    obs_id        INTEGER NOT NULL REFERENCES observations(obs_id),
    PRIMARY KEY (event_id, obs_id)
);

-- Append-only derived evidence produced by reviewer actions.
-- content_id is a hash of (event_id, kind, canonical payload) and is the
-- stable evidence number reused across re-solves.
CREATE TABLE IF NOT EXISTS evidence (
    content_id    TEXT PRIMARY KEY,
    event_id      TEXT NOT NULL REFERENCES events(event_id),
    kind          TEXT NOT NULL CHECK (kind IN ('exclude_peak','lock_clock','keep_pair')),
    payload_json  TEXT NOT NULL,
    created_at    REAL NOT NULL
);

CREATE INDEX IF NOT EXISTS idx_evidence_event ON evidence(event_id);

-- Job lifecycle: created -> running -> done | failed. Candidates only become
-- auditable for a job whose state is 'done'.
CREATE TABLE IF NOT EXISTS jobs (
    fingerprint   TEXT PRIMARY KEY,
    event_id      TEXT NOT NULL REFERENCES events(event_id),
    state         TEXT NOT NULL CHECK (state IN ('created','running','done','failed')),
    created_at    REAL NOT NULL,
    started_at    REAL,
    finished_at   REAL,
    error         TEXT NOT NULL DEFAULT '',
    worker        TEXT NOT NULL DEFAULT ''
);

CREATE TABLE IF NOT EXISTS candidates (
    cand_id       INTEGER PRIMARY KEY AUTOINCREMENT,
    job_fp        TEXT NOT NULL REFERENCES jobs(fingerprint),
    event_id      TEXT NOT NULL REFERENCES events(event_id),
    -- per-job deterministic 0-based rank; the global id never reorders
    rank_in_job   INTEGER NOT NULL,
    cand_key      TEXT NOT NULL,            -- deterministic content key
    kind          TEXT NOT NULL CHECK (kind IN ('point','region')),
    status        TEXT NOT NULL CHECK (status IN ('determined','underdetermined')),
    reason        TEXT NOT NULL DEFAULT '',
    x             REAL,
    y             REAL,
    region_json   TEXT NOT NULL DEFAULT 'null',
    metrics_json  TEXT NOT NULL,
    residuals_json TEXT NOT NULL,
    UNIQUE (job_fp, rank_in_job)
);

CREATE INDEX IF NOT EXISTS idx_cand_event ON candidates(event_id);
"#;

impl Db {
    pub fn open(path: &str) -> rusqlite::Result<Self> {
        let conn = Connection::open(path)?;
        conn.execute_batch(SCHEMA)?;
        Ok(Db {
            conn: Mutex::new(conn),
        })
    }

    /// In-memory database (tests / ephemeral demos).
    pub fn open_memory() -> rusqlite::Result<Self> {
        let conn = Connection::open_in_memory()?;
        conn.execute_batch(SCHEMA)?;
        Ok(Db {
            conn: Mutex::new(conn),
        })
    }

    /// Crash recovery.
    ///
    /// 1. Jobs left `running` (worker killed mid-transaction) are moved back
    ///    to `created` so they are retried; their half-written candidates are
    ///    removed because they were never committed as auditable.
    /// 2. Batches are keyed by fingerprint, so replayed uploads after a crash
    ///    never insert observations twice.
    pub fn recover_pending_jobs(&self) -> rusqlite::Result<usize> {
        let conn = self.conn.lock().unwrap();
        conn.execute("DELETE FROM candidates WHERE job_fp IN (SELECT fingerprint FROM jobs WHERE state='running')", [])?;
        let n = conn.execute(
            "UPDATE jobs SET state='created', started_at=NULL, worker='' WHERE state='running'",
            [],
        )?;
        Ok(n)
    }
}
