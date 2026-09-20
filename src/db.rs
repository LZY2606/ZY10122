//! SQLite 持久层：建表、幂等写入、派生证据追加、作业状态机。

use rusqlite::{params, Connection, OptionalExtension};
use std::collections::BTreeMap;

use crate::model::*;

pub const SCHEMA_VERSION: i64 = 1;

pub fn open(path: &str) -> rusqlite::Result<Connection> {
    let conn = Connection::open(path)?;
    conn.pragma_update(None, "foreign_keys", "ON")?;
    conn.pragma_update(None, "busy_timeout", 5000)?;
    init(&conn)?;
    Ok(conn)
}

pub fn init(conn: &Connection) -> rusqlite::Result<()> {
    conn.execute_batch(SCHEMA)?;
    let v: i64 = conn
        .query_row("SELECT value FROM meta WHERE key='schema_version'", [], |r| {
            r.get::<_, String>(0).map(|s| s.parse::<i64>().unwrap_or(0))
        })
        .optional()?
        .unwrap_or(0);
    if v == 0 {
        conn.execute(
            "INSERT INTO meta(key,value) VALUES('schema_version',?1)",
            params![SCHEMA_VERSION.to_string()],
        )?;
    }
    Ok(())
}

const SCHEMA: &str = r#"
CREATE TABLE IF NOT EXISTS meta (
  key TEXT PRIMARY KEY,
  value TEXT NOT NULL
);
CREATE TABLE IF NOT EXISTS stations (
  station_id TEXT PRIMARY KEY,
  x REAL NOT NULL,
  y REAL NOT NULL,
  label TEXT NOT NULL
);
CREATE TABLE IF NOT EXISTS clock_segments (
  segment_id INTEGER PRIMARY KEY AUTOINCREMENT,
  station_id TEXT NOT NULL REFERENCES stations(station_id),
  t_start REAL NOT NULL,
  t_end REAL,
  offset_sec REAL NOT NULL,
  source TEXT NOT NULL,
  UNIQUE(station_id, t_start)
);
CREATE TABLE IF NOT EXISTS events (
  event_id TEXT PRIMARY KEY,
  label TEXT NOT NULL,
  window_start REAL NOT NULL,
  window_end REAL NOT NULL,
  sound_speed_mps REAL NOT NULL,
  bounds_min_x REAL NOT NULL,
  bounds_min_y REAL NOT NULL,
  bounds_max_x REAL NOT NULL,
  bounds_max_y REAL NOT NULL
);
CREATE TABLE IF NOT EXISTS batches (
  batch_uid TEXT PRIMARY KEY,
  event_id TEXT NOT NULL REFERENCES events(event_id),
  payload_fingerprint TEXT NOT NULL,
  received_at TEXT NOT NULL DEFAULT (strftime('%Y-%m-%dT%H:%M:%fZ','now')),
  duplicate_count INTEGER NOT NULL DEFAULT 0
);
CREATE TABLE IF NOT EXISTS observations (
  obs_uid TEXT PRIMARY KEY,
  batch_uid TEXT NOT NULL REFERENCES batches(batch_uid),
  event_id TEXT NOT NULL REFERENCES events(event_id),
  station_id TEXT NOT NULL REFERENCES stations(station_id),
  local_onset_sec REAL NOT NULL,
  corrected_onset_sec REAL,
  clock_segment_id INTEGER REFERENCES clock_segments(segment_id),
  peak_band_hz REAL NOT NULL,
  snr_db REAL NOT NULL,
  duplicate_of TEXT REFERENCES observations(obs_uid)
);
CREATE INDEX IF NOT EXISTS idx_obs_event ON observations(event_id);
CREATE TABLE IF NOT EXISTS lags (
  lag_uid TEXT PRIMARY KEY,
  obs_uid TEXT NOT NULL REFERENCES observations(obs_uid),
  event_id TEXT NOT NULL REFERENCES events(event_id),
  station_id TEXT NOT NULL REFERENCES stations(station_id),
  peer_id TEXT NOT NULL REFERENCES stations(station_id),
  lag_sec REAL NOT NULL,
  peak_band_hz REAL NOT NULL,
  snr_db REAL NOT NULL,
  hint TEXT NOT NULL
);
CREATE INDEX IF NOT EXISTS idx_lags_event ON lags(event_id);
CREATE TABLE IF NOT EXISTS evidence (
  evidence_id TEXT PRIMARY KEY,
  event_id TEXT NOT NULL REFERENCES events(event_id),
  kind TEXT NOT NULL,
  payload TEXT NOT NULL,
  basis TEXT NOT NULL,
  created_at TEXT NOT NULL DEFAULT (strftime('%Y-%m-%dT%H:%M:%fZ','now')),
  superseded_by TEXT REFERENCES evidence(evidence_id),
  active INTEGER NOT NULL DEFAULT 1
);
CREATE INDEX IF NOT EXISTS idx_evidence_event ON evidence(event_id);
CREATE TABLE IF NOT EXISTS solve_jobs (
  job_id INTEGER PRIMARY KEY AUTOINCREMENT,
  event_id TEXT NOT NULL REFERENCES events(event_id),
  fingerprint TEXT NOT NULL,
  status TEXT NOT NULL,
  error TEXT,
  created_at TEXT NOT NULL DEFAULT (strftime('%Y-%m-%dT%H:%M:%fZ','now')),
  finished_at TEXT
);
CREATE INDEX IF NOT EXISTS idx_jobs_event ON solve_jobs(event_id);
CREATE TABLE IF NOT EXISTS candidates (
  candidate_id TEXT PRIMARY KEY,
  job_id INTEGER NOT NULL REFERENCES solve_jobs(job_id),
  rank INTEGER NOT NULL,
  kind TEXT NOT NULL,
  point_x REAL, point_y REAL,
  curve_json TEXT,
  region_json TEXT,
  rms_sec REAL,
  underdetermined TEXT NOT NULL,
  score REAL NOT NULL,
  signature TEXT NOT NULL,
  residuals_json TEXT NOT NULL
);
CREATE INDEX IF NOT EXISTS idx_cand_job ON candidates(job_id);
"#;

// ---------- 站点 / 事件 / 时钟段 ----------

pub fn upsert_station(conn: &Connection, s: &Station) -> rusqlite::Result<()> {
    conn.execute(
        "INSERT INTO stations(station_id,x,y,label) VALUES(?1,?2,?3,?4)
         ON CONFLICT(station_id) DO UPDATE SET x=excluded.x,y=excluded.y,label=excluded.label",
        params![s.station_id, s.x, s.y, s.label],
    )?;
    Ok(())
}

pub fn list_stations(conn: &Connection) -> rusqlite::Result<Vec<Station>> {
    let mut stmt = conn.prepare(
        "SELECT station_id,x,y,label FROM stations ORDER BY station_id",
    )?;
    let rows = stmt.query_map([], |r| {
        Ok(Station {
            station_id: r.get(0)?,
            x: r.get(1)?,
            y: r.get(2)?,
            label: r.get(3)?,
        })
    })?;
    rows.collect()
}

pub fn station_position(conn: &Connection, id: &str) -> rusqlite::Result<Option<(f64, f64)>> {
    conn.query_row("SELECT x,y FROM stations WHERE station_id=?1", params![id], |r| {
        Ok((r.get(0)?, r.get(1)?))
    })
    .optional()
}

#[derive(Debug, Clone, serde::Serialize)]
pub struct EventRow {
    pub event_id: String,
    pub label: String,
    pub window_start: f64,
    pub window_end: f64,
    pub sound_speed_mps: f64,
    pub bounds: SiteBounds,
}

pub fn create_event(conn: &Connection, e: &EventInput) -> rusqlite::Result<()> {
    let b = e.site_bounds;
    conn.execute(
        "INSERT INTO events(event_id,label,window_start,window_end,sound_speed_mps,
          bounds_min_x,bounds_min_y,bounds_max_x,bounds_max_y)
         VALUES(?1,?2,?3,?4,?5,?6,?7,?8,?9)
         ON CONFLICT(event_id) DO UPDATE SET
          label=excluded.label, window_start=excluded.window_start,
          window_end=excluded.window_end, sound_speed_mps=excluded.sound_speed_mps,
          bounds_min_x=excluded.bounds_min_x, bounds_min_y=excluded.bounds_min_y,
          bounds_max_x=excluded.bounds_max_x, bounds_max_y=excluded.bounds_max_y",
        params![
            e.event_id, e.label, e.window_start, e.window_end, e.sound_speed_mps,
            b.min_x, b.min_y, b.max_x, b.max_y
        ],
    )?;
    Ok(())
}

pub fn list_events(conn: &Connection) -> rusqlite::Result<Vec<EventRow>> {
    let mut stmt = conn.prepare(
        "SELECT event_id,label,window_start,window_end,sound_speed_mps,
                bounds_min_x,bounds_min_y,bounds_max_x,bounds_max_y
         FROM events ORDER BY event_id",
    )?;
    let rows = stmt.query_map([], |r| {
        Ok(EventRow {
            event_id: r.get(0)?,
            label: r.get(1)?,
            window_start: r.get(2)?,
            window_end: r.get(3)?,
            sound_speed_mps: r.get(4)?,
            bounds: SiteBounds {
                min_x: r.get(5)?,
                min_y: r.get(6)?,
                max_x: r.get(7)?,
                max_y: r.get(8)?,
            },
        })
    })?;
    rows.collect()
}

pub fn get_event(conn: &Connection, event_id: &str) -> rusqlite::Result<Option<EventRow>> {
    list_events(conn).map(|v| v.into_iter().find(|e| e.event_id == event_id))
}

pub fn insert_clock_segment(conn: &Connection, c: &ClockSegment) -> rusqlite::Result<i64> {
    conn.execute(
        "INSERT INTO clock_segments(station_id,t_start,t_end,offset_sec,source)
         VALUES(?1,?2,?3,?4,?5)
         ON CONFLICT(station_id,t_start) DO UPDATE SET
          t_end=excluded.t_end, offset_sec=excluded.offset_sec, source=excluded.source",
        params![c.station_id, c.t_start, c.t_end, c.offset_sec, c.source],
    )?;
    conn.query_row(
        "SELECT segment_id FROM clock_segments WHERE station_id=?1 AND t_start=?2",
        params![c.station_id, c.t_start],
        |r| r.get(0),
    )
}

pub fn list_clock_segments(conn: &Connection) -> rusqlite::Result<Vec<(ClockSegment, i64)>> {
    let mut stmt = conn.prepare(
        "SELECT segment_id,station_id,t_start,t_end,offset_sec,source
         FROM clock_segments ORDER BY station_id,t_start",
    )?;
    let rows = stmt.query_map([], |r| {
        let id: i64 = r.get(0)?;
        Ok((
            ClockSegment {
                station_id: r.get(1)?,
                t_start: r.get(2)?,
                t_end: r.get(3)?,
                offset_sec: r.get(4)?,
                source: r.get(5)?,
            },
            id,
        ))
    })?;
    rows.collect()
}

/// 半开区间 `[t_start, t_end)` 内生效的段；`t_end IS NULL` 延伸到无穷远。
/// 注意恰好在段边界上的时刻只属于后一段。
pub fn clock_segments_at(
    segs: &[(ClockSegment, i64)],
    station_id: &str,
    t: f64,
) -> Vec<(ClockSegment, i64)> {
    segs.iter()
        .filter(|(s, _)| {
            s.station_id == station_id && t >= s.t_start && s.t_end.map_or(true, |e| t < e)
        })
        .cloned()
        .collect()
}

/// 校验半开边界：两个相接段不能在同一时刻同时生效；同站段不得重叠或顺序错乱。
pub fn validate_clock_partition(segs: &[ClockSegment]) -> Result<(), String> {
    let mut by_station: BTreeMap<String, Vec<&ClockSegment>> = BTreeMap::new();
    for s in segs {
        by_station.entry(s.station_id.clone()).or_default().push(s);
    }
    for (station, mut list) in by_station {
        list.sort_by(|a, b| a.t_start.partial_cmp(&b.t_start).unwrap());
        for w in list.windows(2) {
            let (a, b) = (w[0], w[1]);
            if a.t_end.map_or(false, |e| e > b.t_start) {
                return Err(format!(
                    "station {station}: segments overlap ({}, {})",
                    a.t_start, b.t_start
                ));
            }
            // 相接是允许的：a.t_end == b.t_start，半开语义保证边界点唯一归属。
            if a.t_start == b.t_start {
                return Err(format!(
                    "station {station}: two segments start at {0}",
                    a.t_start
                ));
            }
        }
    }
    Ok(())
}

// ---------- 指纹与批次幂等 ----------

/// 对原始批次 JSON 做确定性规范化（字段排序）后取 SHA-256。
/// 同样的观测内容无论 JSON 键序如何，指纹相同。
pub fn canonical_fingerprint(value: &serde_json::Value) -> String {
    use sha2::{Digest, Sha256};
    let canonical = canonical_string(value);
    let mut h = Sha256::new();
    h.update(canonical.as_bytes());
    hex::encode(h.finalize())
}

fn canonical_string(v: &serde_json::Value) -> String {
    match v {
        serde_json::Value::Null => "null".into(),
        serde_json::Value::Bool(b) => b.to_string(),
        serde_json::Value::Number(n) => canonical_number(n),
        serde_json::Value::String(s) => serde_json::to_string(s).unwrap_or_default(),
        serde_json::Value::Array(a) => {
            let mut out = String::from("[");
            for (i, x) in a.iter().enumerate() {
                if i > 0 {
                    out.push(',');
                }
                out.push_str(&canonical_string(x));
            }
            out.push(']');
            out
        }
        serde_json::Value::Object(m) => {
            let mut keys: Vec<&String> = m.keys().collect();
            keys.sort();
            let mut out = String::from("{");
            for (i, k) in keys.iter().enumerate() {
                if i > 0 {
                    out.push(',');
                }
                out.push_str(&serde_json::to_string(k).unwrap_or_default());
                out.push(':');
                out.push_str(&canonical_string(&m[*k]));
            }
            out.push('}');
            out
        }
    }
}

fn canonical_number(n: &serde_json::Number) -> String {
    if let Some(i) = n.as_i64() {
        i.to_string()
    } else if let Some(u) = n.as_u64() {
        u.to_string()
    } else if let Some(f) = n.as_f64() {
        if f.is_finite() {
            format!("{f:.12e}")
        } else {
            "null".into()
        }
    } else {
        n.to_string()
    }
}

#[derive(Debug, PartialEq, Eq)]
pub enum IngestResult {
    Inserted { observations_added: usize },
    DuplicateBatch,
}

/// 幂等写入一个批次：
/// - 相同 `batch_uid` 直接返回，不重复添加观测；
/// - 批内/库内相同 `obs_uid` 记为重复上报（`duplicate_of` 指向保留观测），不产生新证据；
/// - lag UID 缺省时由 `(batch_uid, obs_uid, peer_id, index)` 确定性生成。
pub fn ingest_batch(conn: &mut Connection, batch: &BatchInput, raw: &serde_json::Value) -> rusqlite::Result<IngestResult> {
    let exists: bool = conn
        .query_row(
            "SELECT 1 FROM batches WHERE batch_uid=?1",
            params![batch.batch_uid],
            |_| Ok(true),
        )
        .optional()?
        .unwrap_or(false);
    if exists {
        conn.execute(
            "UPDATE batches SET duplicate_count=duplicate_count+1 WHERE batch_uid=?1",
            params![batch.batch_uid],
        )?;
        return Ok(IngestResult::DuplicateBatch);
    }
    let fp = canonical_fingerprint(raw);
    let tx = conn.transaction()?;
    tx.execute(
        "INSERT INTO batches(batch_uid,event_id,payload_fingerprint) VALUES(?1,?2,?3)",
        params![batch.batch_uid, batch.event_id, fp],
    )?;
    let mut added = 0usize;
    for (idx, obs) in batch.observations.iter().enumerate() {
        let existing_obs: Option<String> = tx
            .query_row(
                "SELECT obs_uid FROM observations WHERE obs_uid=?1",
                params![obs.obs_uid],
                |r| r.get(0),
            )
            .optional()?;
        if let Some(orig) = existing_obs {
            // 重复上报：记录到保留观测的链条上（保留最早出现的一条）。
            tx.execute(
                "UPDATE observations SET duplicate_of=COALESCE(duplicate_of,?2) WHERE obs_uid=?1",
                params![orig, orig],
            )?;
            continue;
        }
        // 同站重复（设备换了 uid 重传）：按事件+站点+本地时间 5e-6s 容差识别。
        let twin: Option<String> = tx
            .query_row(
                "SELECT obs_uid FROM observations
                 WHERE event_id=?1 AND station_id=?2
                   AND ABS(local_onset_sec-?3) < 0.000005
                 ORDER BY obs_uid LIMIT 1",
                params![batch.event_id, obs.station_id, obs.local_onset_sec],
                |r| r.get(0),
            )
            .optional()?;
        tx.execute(
            "INSERT INTO observations(obs_uid,batch_uid,event_id,station_id,
              local_onset_sec,peak_band_hz,snr_db,duplicate_of)
             VALUES(?1,?2,?3,?4,?5,?6,?7,?8)",
            params![
                obs.obs_uid, batch.batch_uid, batch.event_id, obs.station_id,
                obs.local_onset_sec, obs.peak_band_hz, obs.snr_db, twin
            ],
        )?;
        added += 1;
        for (j, lag) in obs.lags.iter().enumerate() {
            let lag_uid = lag.lag_uid.clone().unwrap_or_else(|| {
                format!("lag:{}:{}:{}:{}", batch.batch_uid, idx, j, lag.peer_id)
            });
            tx.execute(
                "INSERT OR IGNORE INTO lags(lag_uid,obs_uid,event_id,station_id,peer_id,
                  lag_sec,peak_band_hz,snr_db,hint)
                 VALUES(?1,?2,?3,?4,?5,?6,?7,?8,?9)",
                params![
                    lag_uid, obs.obs_uid, batch.event_id, obs.station_id, lag.peer_id,
                    lag.lag_sec, lag.peak_band_hz, lag.snr_db, lag.hint
                ],
            )?;
        }
    }
    tx.commit()?;
    apply_clock_corrections(conn)?;
    Ok(IngestResult::Inserted { observations_added: added })
}

/// 用当前生效的时钟段重算每条观测的校正时间。
/// 仅写派生列；原观测（local_onset_sec 等）永不修改。
pub fn apply_clock_corrections(conn: &mut Connection) -> rusqlite::Result<()> {
    let segs = list_clock_segments(conn)?;
    let mut stmt = conn.prepare(
        "SELECT obs_uid,station_id,local_onset_sec FROM observations",
    )?;
    let obs: Vec<(String, String, f64)> = stmt
        .query_map([], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)))?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    drop(stmt);
    for (obs_uid, station, local) in obs {
        let hits = clock_segments_at(&segs, &station, local);
        match hits.as_slice() {
            [(seg, id)] => {
                let corrected = local - seg.offset_sec;
                conn.execute(
                    "UPDATE observations SET corrected_onset_sec=?1, clock_segment_id=?2
                     WHERE obs_uid=?3",
                    params![corrected, id, obs_uid],
                )?;
            }
            _ => {
                // 0 个（未建模）或多于 1 个（模型冲突）都视为时钟未锁定。
                conn.execute(
                    "UPDATE observations SET corrected_onset_sec=NULL, clock_segment_id=NULL
                     WHERE obs_uid=?1",
                    params![obs_uid],
                )?;
            }
        }
    }
    Ok(())
}

// ---------- 观测 / 延迟读取 ----------

pub fn list_observations(conn: &Connection, event_id: &str) -> rusqlite::Result<Vec<ObservationRow>> {
    let mut stmt = conn.prepare(
        "SELECT obs_uid,batch_uid,event_id,station_id,local_onset_sec,
                corrected_onset_sec,clock_segment_id,peak_band_hz,snr_db,duplicate_of
         FROM observations WHERE event_id=?1
         ORDER BY COALESCE(corrected_onset_sec, local_onset_sec), station_id, obs_uid",
    )?;
    let rows = stmt.query_map(params![event_id], |r| {
        let locked = r.get::<_, Option<i64>>(6)?;
        Ok(ObservationRow {
            obs_uid: r.get(0)?,
            batch_uid: r.get(1)?,
            event_id: r.get(2)?,
            station_id: r.get(3)?,
            local_onset_sec: r.get(4)?,
            corrected_onset_sec: r.get(5)?,
            clock_locked: locked.is_some(),
            clock_segment_id: locked,
            peak_band_hz: r.get(7)?,
            snr_db: r.get(8)?,
            duplicate_of: r.get(9)?,
        })
    })?;
    rows.collect()
}

pub fn list_lags(conn: &Connection, event_id: &str) -> rusqlite::Result<Vec<LagRow>> {
    let mut stmt = conn.prepare(
        "SELECT lag_uid,obs_uid,event_id,station_id,peer_id,lag_sec,peak_band_hz,snr_db,hint
         FROM lags WHERE event_id=?1 ORDER BY station_id,obs_uid,lag_uid",
    )?;
    let rows = stmt.query_map(params![event_id], |r| {
        Ok(LagRow {
            lag_uid: r.get(0)?,
            obs_uid: r.get(1)?,
            station_id: r.get(3)?,
            peer_id: r.get(4)?,
            lag_sec: r.get(5)?,
            peak_band_hz: r.get(6)?,
            snr_db: r.get(7)?,
            hint: r.get(8)?,
        })
    })?;
    rows.collect()
}

// ---------- 派生证据（手工判断）----------

/// 支持的派生证据：
/// - exclude_reflection: 排除某条反射路径 lag（payload: {"lag_uid":..}）
/// - lock_clock: 锁定一个时钟校正段（payload: {"segment_id":..}）
/// - keep_ambiguous: 保留两个不可分位置（payload: {"candidate_ids":[..]}）
pub fn append_evidence(
    conn: &mut Connection,
    event_id: &str,
    kind: &str,
    payload: &serde_json::Value,
    basis: &str,
) -> rusqlite::Result<String> {
    let count: i64 = conn.query_row(
        "SELECT COUNT(*) FROM evidence WHERE event_id=?1",
        params![event_id],
        |r| r.get(0),
    )?;
    // 证据编号按事件内顺序，重算只产生新证据，从不复用旧编号。
    let evidence_id = format!("ev-{}:{:04}", event_id, count + 1);
    let payload_str = serde_json::to_string(payload).unwrap_or_else(|_| "{}".into());
    conn.execute(
        "INSERT INTO evidence(evidence_id,event_id,kind,payload,basis)
         VALUES(?1,?2,?3,?4,?5)",
        params![evidence_id, event_id, kind, payload_str, basis],
    )?;
    Ok(evidence_id)
}

pub fn list_evidence(conn: &Connection, event_id: &str) -> rusqlite::Result<Vec<EvidenceRow>> {
    let mut stmt = conn.prepare(
        "SELECT evidence_id,event_id,kind,payload,basis,created_at,superseded_by,active
         FROM evidence WHERE event_id=?1 ORDER BY evidence_id",
    )?;
    let rows = stmt.query_map(params![event_id], |r| {
        let payload: String = r.get(3)?;
        Ok(EvidenceRow {
            evidence_id: r.get(0)?,
            event_id: r.get(1)?,
            kind: r.get(2)?,
            payload: serde_json::from_str(&payload).unwrap_or(serde_json::Value::Null),
            basis: r.get(4)?,
            created_at: r.get(5)?,
            superseded_by: r.get(6)?,
            active: r.get::<_, i64>(7)? != 0,
        })
    })?;
    rows.collect()
}

// ---------- 求解作业与候选 ----------

/// 找一个 pending 的作业；崩溃后把残留 running 重置为 pending（候选随事务清除）。
pub fn claim_next_job(conn: &mut Connection) -> rusqlite::Result<Option<i64>> {
    conn.execute(
        "UPDATE solve_jobs SET status='pending', error='recovered after crash'
         WHERE status='running'",
        [],
    )?;
    conn.execute(
        "DELETE FROM candidates WHERE job_id IN
           (SELECT job_id FROM solve_jobs WHERE status='pending')",
        [],
    )?;
    let id: Option<i64> = conn
        .query_row(
            "SELECT job_id FROM solve_jobs WHERE status='pending'
             ORDER BY job_id LIMIT 1",
            [],
            |r| r.get(0),
        )
        .optional()?;
    if let Some(id) = id {
        conn.execute(
            "UPDATE solve_jobs SET status='running', error=NULL WHERE job_id=?1",
            params![id],
        )?;
    }
    Ok(id)
}

/// 幂等创建作业：同一事件、同一输入指纹下已有未失败作业则复用，不重算。
pub fn enqueue_job(conn: &mut Connection, event_id: &str, fingerprint: &str) -> rusqlite::Result<i64> {
    if let Some(id) = conn
        .query_row(
            "SELECT job_id FROM solve_jobs
             WHERE event_id=?1 AND fingerprint=?2 AND status IN ('pending','running','completed')
             ORDER BY job_id DESC LIMIT 1",
            params![event_id, fingerprint],
            |r| r.get(0),
        )
        .optional()?
    {
        return Ok(id);
    }
    conn.execute(
        "INSERT INTO solve_jobs(event_id,fingerprint,status) VALUES(?1,?2,'pending')",
        params![event_id, fingerprint],
    )?;
    Ok(conn.last_insert_rowid())
}

/// 原子提交：候选全部写入后作业才变 completed；
/// 失败时候选随事务回滚，作业保持 failed，不会被当成可审核结果。
pub fn complete_job(
    conn: &mut Connection,
    job_id: i64,
    candidates: &[Candidate],
) -> rusqlite::Result<()> {
    let tx = conn.transaction()?;
    for c in candidates {
        let (px, py): (Option<f64>, Option<f64>) =
            c.point.map(|p| (Some(p[0]), Some(p[1]))).unwrap_or((None, None));
        let curve = c.curve.as_ref().and_then(|v| serde_json::to_string(v).ok());
        let region = c.region.as_ref().and_then(|v| serde_json::to_string(v).ok());
        let residuals = serde_json::to_string(&c.residuals).unwrap_or_else(|_| "[]".into());
        tx.execute(
            "INSERT INTO candidates(candidate_id,job_id,rank,kind,point_x,point_y,
               curve_json,region_json,rms_sec,underdetermined,score,signature,residuals_json)
             VALUES(?1,?2,?3,?4,?5,?6,?7,?8,?9,?10,?11,?12,?13)",
            params![
                c.candidate_id, job_id, c.rank, c.kind,
                px,
                py,
                curve, region, c.rms_sec, c.underdetermined, c.score, c.signature, residuals
            ],
        )?;
    }
    tx.execute(
        "UPDATE solve_jobs SET status='completed', error=NULL,
         finished_at=strftime('%Y-%m-%dT%H:%M:%fZ','now') WHERE job_id=?1",
        params![job_id],
    )?;
    tx.commit()?;
    Ok(())
}

pub fn fail_job(conn: &mut Connection, job_id: i64, error: &str) -> rusqlite::Result<()> {
    conn.execute(
        "UPDATE solve_jobs SET status='failed', error=?2,
         finished_at=strftime('%Y-%m-%dT%H:%M:%fZ','now') WHERE job_id=?1",
        params![job_id, error],
    )?;
    Ok(())
}

pub fn list_jobs(conn: &Connection, event_id: &str) -> rusqlite::Result<Vec<JobView>> {
    let mut stmt = conn.prepare(
        "SELECT job_id,event_id,status,fingerprint,error,created_at
         FROM solve_jobs WHERE event_id=?1 ORDER BY job_id",
    )?;
    let rows = stmt.query_map(params![event_id], |r| {
        Ok(JobView {
            job_id: r.get(0)?,
            event_id: r.get(1)?,
            status: r.get(2)?,
            fingerprint: r.get(3)?,
            error: r.get(4)?,
            created_at: r.get(5)?,
        })
    })?;
    rows.collect()
}

/// 只返回已完成作业的候选；未完成候选不存在可审核视图。
pub fn latest_completed_job(conn: &Connection, event_id: &str) -> rusqlite::Result<Option<i64>> {
    conn.query_row(
        "SELECT job_id FROM solve_jobs WHERE event_id=?1 AND status='completed'
         ORDER BY job_id DESC LIMIT 1",
        params![event_id],
        |r| r.get(0),
    )
    .optional()
}

pub fn list_candidates(conn: &Connection, job_id: i64) -> rusqlite::Result<Vec<Candidate>> {
    let mut stmt = conn.prepare(
        "SELECT candidate_id,job_id,rank,kind,point_x,point_y,curve_json,region_json,
                rms_sec,underdetermined,score,signature,residuals_json
         FROM candidates WHERE job_id=?1 ORDER BY rank",
    )?;
    let rows = stmt.query_map(params![job_id], |r| {
        let px: Option<f64> = r.get(4)?;
        let py: Option<f64> = r.get(5)?;
        let curve_s: Option<String> = r.get(6)?;
        let region_s: Option<String> = r.get(7)?;
        let residuals_s: String = r.get(12)?;
        Ok(Candidate {
            candidate_id: r.get(0)?,
            job_id: r.get(1)?,
            rank: r.get(2)?,
            kind: r.get(3)?,
            point: px.and_then(|x| py.map(|y| [x, y])),
            curve: curve_s.and_then(|s| serde_json::from_str(&s).ok()),
            region: region_s.and_then(|s| serde_json::from_str(&s).ok()),
            rms_sec: r.get(8)?,
            underdetermined: r.get(9)?,
            score: r.get(10)?,
            signature: r.get(11)?,
            residuals: serde_json::from_str(&residuals_s).unwrap_or_default(),
        })
    })?;
    rows.collect()
}
