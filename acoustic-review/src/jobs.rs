//! Solve jobs: fingerprints, lifecycle and crash-safe candidate commits.

use crate::clock::list_segments;
use crate::db::Db;
use crate::evidence::list_evidence;
use crate::solver::{self, EdgeObs, Snapshot, StationInfo};
use crate::units::PhysicalModel;
use rusqlite::params;
use serde_json::{json, Value};
use std::collections::{BTreeMap, BTreeSet};
use std::time::{SystemTime, UNIX_EPOCH};

pub const ONSET_EDGE_SIGMA: f64 = 0.004;

fn now_s() -> f64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs_f64())
        .unwrap_or(0.0)
}

#[derive(Debug, Clone)]
pub struct LinkedObs {
    pub obs_id: i64,
    pub station_id: String,
    pub local_onset_s: f64,
    pub peak_band_hz: f64,
    pub snr_db: f64,
}

pub fn linked_observations(
    db: &Db,
    event_id: &str,
) -> Result<(f64, f64, f64, Vec<LinkedObs>), String> {
    let conn = db.conn.lock().unwrap();
    let (t0, t1, speed): (f64, f64, f64) = conn
        .query_row(
            "SELECT t_start, t_end, sound_speed FROM events WHERE event_id=?1",
            params![event_id],
            |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
        )
        .map_err(|e| e.to_string())?;
    let mut stmt = conn
        .prepare(
            "SELECT o.obs_id, o.station_id, o.local_onset_s, o.peak_band_hz, o.snr_db
             FROM event_observations eo
             JOIN observations o ON o.obs_id = eo.obs_id
             WHERE eo.event_id=?1
             ORDER BY o.local_onset_s, o.obs_id",
        )
        .map_err(|e| e.to_string())?;
    let rows = stmt
        .query_map(params![event_id], |r| {
            Ok(LinkedObs {
                obs_id: r.get(0)?,
                station_id: r.get(1)?,
                local_onset_s: r.get(2)?,
                peak_band_hz: r.get(3)?,
                snr_db: r.get(4)?,
            })
        })
        .map_err(|e| e.to_string())?;
    let mut out = Vec::new();
    for row in rows {
        out.push(row.map_err(|e| e.to_string())?);
    }
    Ok((t0, t1, speed, out))
}

fn stations_in_event(db: &Db, obs: &[LinkedObs]) -> Result<Vec<StationInfo>, String> {
    let ids: BTreeSet<String> = obs.iter().map(|o| o.station_id.clone()).collect();
    let conn = db.conn.lock().unwrap();
    let mut stmt = conn
        .prepare("SELECT station_id, x, y FROM stations ORDER BY station_id")
        .map_err(|e| e.to_string())?;
    let rows = stmt
        .query_map([], |r| {
            Ok((
                r.get::<_, String>(0)?,
                r.get::<_, f64>(1)?,
                r.get::<_, f64>(2)?,
            ))
        })
        .map_err(|e| e.to_string())?;
    let mut all = Vec::new();
    for row in rows {
        all.push(row.map_err(|e| e.to_string())?);
    }
    drop(stmt);
    drop(conn);
    Ok(all
        .into_iter()
        .filter(|(id, _, _)| ids.contains(id))
        .map(|(station_id, x, y)| StationInfo { station_id, x, y })
        .collect())
}

pub fn build_snapshot(db: &Db, event_id: &str, model: PhysicalModel) -> Result<Snapshot, String> {
    let (t0, t1, speed, obs) = linked_observations(db, event_id)?;
    let t_mid = (t0 + t1) / 2.0;
    let stations = stations_in_event(db, &obs)?;

    let mut segments = BTreeMap::new();
    for s in &stations {
        segments.insert(s.station_id.clone(), list_segments(db, &s.station_id)?);
    }

    let obs_ids: BTreeSet<i64> = obs.iter().map(|o| o.obs_id).collect();
    let mut edges: Vec<EdgeObs> = Vec::new();

    // Explicit cross-correlation delays uploaded by the collectors.
    {
        let conn = db.conn.lock().unwrap();
        let mut stmt = conn
            .prepare(
                "SELECT x.xc_id, x.obs_a, x.obs_b, sa.station_id, sb.station_id,
                        x.delay_s, x.sigma_s, x.peak_index, x.multipath
                 FROM xcorr_delays x
                 JOIN observations oa ON oa.obs_id = x.obs_a
                 JOIN observations ob ON ob.obs_id = x.obs_b
                 JOIN stations sa ON sa.station_id = oa.station_id
                 JOIN stations sb ON sb.station_id = ob.station_id
                 ORDER BY x.peak_index, x.xc_id",
            )
            .map_err(|e| e.to_string())?;
        let rows = stmt
            .query_map([], |r| {
                Ok((
                    r.get::<_, i64>(0)?,
                    r.get::<_, i64>(1)?,
                    r.get::<_, i64>(2)?,
                    r.get::<_, String>(3)?,
                    r.get::<_, String>(4)?,
                    r.get::<_, f64>(5)?,
                    r.get::<_, f64>(6)?,
                    r.get::<_, i64>(7)?,
                    r.get::<_, i64>(8)?,
                ))
            })
            .map_err(|e| e.to_string())?;
        for row in rows {
            let (xc_id, oa, ob, sa, sb, delay, sigma, peak_index, mp) =
                row.map_err(|e| e.to_string())?;
            if obs_ids.contains(&oa) && obs_ids.contains(&ob) {
                edges.push(EdgeObs {
                    station_a: sa,
                    station_b: sb,
                    delay_s: delay,
                    sigma_s: sigma,
                    multipath: mp != 0,
                    xc_id: Some(xc_id),
                    peak_index,
                    origin: "xcorr".into(),
                });
            }
        }
    }

    // Onset differences between stations (local onset is device time; the
    // solver applies the clock difference correction).
    let earliest: BTreeMap<String, &LinkedObs> = obs.iter().fold(BTreeMap::new(), |mut acc, o| {
        acc.entry(o.station_id.clone()).or_insert(o);
        acc
    });
    let mut by_station: Vec<&LinkedObs> = earliest.values().copied().collect();
    by_station.sort_by(|a, b| a.station_id.cmp(&b.station_id));
    for w in by_station.windows(2) {
        edges.push(EdgeObs {
            station_a: w[0].station_id.clone(),
            station_b: w[1].station_id.clone(),
            delay_s: w[1].local_onset_s - w[0].local_onset_s,
            sigma_s: ONSET_EDGE_SIGMA,
            multipath: false,
            xc_id: None,
            peak_index: 0,
            origin: "onset".into(),
        });
    }

    let ev = list_evidence(db, event_id)?;
    let mut exclude_edges = BTreeSet::new();
    let mut locks = BTreeMap::new();
    let mut kept_pairs = Vec::new();
    for e in ev {
        match e.kind.as_str() {
            "exclude_peak" => {
                let sa = e.payload["station_a"].as_str().unwrap_or("").to_string();
                let sb = e.payload["station_b"].as_str().unwrap_or("").to_string();
                let peak = e.payload["peak_index"].as_i64().unwrap_or(0);
                exclude_edges.insert((sa, sb, peak));
            }
            "lock_clock" => {
                let sid = e.payload["station_id"].as_str().unwrap_or("").to_string();
                let off = e.payload["offset_s"].as_f64().unwrap_or(0.0);
                let sigma = e.payload["sigma_s"].as_f64().unwrap_or(0.0);
                locks.insert(sid, (off, sigma));
            }
            "keep_pair" => {
                let ka = e.payload["candidate_a_key"]
                    .as_str()
                    .unwrap_or("")
                    .to_string();
                let kb = e.payload["candidate_b_key"]
                    .as_str()
                    .unwrap_or("")
                    .to_string();
                kept_pairs.push((ka, kb));
            }
            _ => {}
        }
    }

    Ok(Snapshot {
        event_id: event_id.into(),
        t_mid,
        sound_speed: speed,
        observable_radius: model.observable_radius,
        stations,
        segments,
        edges,
        exclude_edges,
        locks,
        kept_pairs,
    })
}

/// Fingerprint of everything that can change the solve output (excluding
/// timestamps), so identical inputs share a job identity.
pub fn fingerprint(snap: &Snapshot) -> String {
    let edges: Vec<Value> = snap
        .edges
        .iter()
        .map(|e| {
            json!({
                "a": e.station_a, "b": e.station_b, "delay": e.delay_s,
                "sigma": e.sigma_s, "peak": e.peak_index, "mp": e.multipath,
                "origin": e.origin, "xc": e.xc_id,
            })
        })
        .collect();
    let stations: Vec<Value> = snap
        .stations
        .iter()
        .map(|s| json!({"id": s.station_id, "x": s.x, "y": s.y}))
        .collect();
    let mut segs: Vec<Value> = Vec::new();
    for (sid, list) in &snap.segments {
        for seg in list {
            segs.push(json!({
                "station": sid, "start": seg.start_s, "end": seg.end_s,
                "offset": seg.offset_s, "sigma": seg.sigma_s,
            }));
        }
    }
    let excluded: Vec<Value> = snap
        .exclude_edges
        .iter()
        .map(|(a, b, p)| json!([a, b, p]))
        .collect();
    let locked: Vec<Value> = snap
        .locks
        .iter()
        .map(|(sid, (o, s))| json!({"station": sid, "offset": o, "sigma": s}))
        .collect();
    let value = json!({
        "event": snap.event_id,
        "t_mid": snap.t_mid,
        "sound_speed": snap.sound_speed,
        "radius": snap.observable_radius,
        "stations": stations,
        "segments": segs,
        "edges": edges,
        "excluded": excluded,
        "locked": locked,
        "kept_pairs": snap.kept_pairs,
    });
    crate::hash::json_fingerprint(&["job-v2"], &value)
}

#[derive(Debug, PartialEq, Eq)]
pub enum RunOutcome {
    Created,
    Reused,
}

/// Enqueue a job (idempotent) and execute it synchronously inside one
/// transaction. Candidates only become visible after state flips to 'done'.
pub fn enqueue_and_run(
    db: &Db,
    snap: &Snapshot,
    worker: &str,
) -> Result<(String, RunOutcome), String> {
    let fp = fingerprint(snap);
    {
        let conn = db.conn.lock().unwrap();
        let exists: i64 = conn
            .query_row(
                "SELECT COUNT(*) FROM jobs WHERE fingerprint=?1",
                params![fp],
                |r| r.get(0),
            )
            .map_err(|e| e.to_string())?;
        if exists == 0 {
            conn.execute(
                "INSERT INTO jobs(fingerprint, event_id, state, created_at, worker) VALUES (?1,?2,'created',?3,'')",
                params![fp, snap.event_id, now_s()],
            )
            .map_err(|e| e.to_string())?;
        } else {
            let state: String = conn
                .query_row(
                    "SELECT state FROM jobs WHERE fingerprint=?1",
                    params![fp],
                    |r| r.get(0),
                )
                .map_err(|e| e.to_string())?;
            if state == "done" {
                return Ok((fp, RunOutcome::Reused));
            }
        }
    }

    let candidates = solver::solve(snap);

    let mut conn = db.conn.lock().unwrap();
    let tx = conn.transaction().map_err(|e| e.to_string())?;
    tx.execute(
        "UPDATE jobs SET state='running', started_at=?1, worker=?2 WHERE fingerprint=?3",
        params![now_s(), worker, fp],
    )
    .map_err(|e| e.to_string())?;
    tx.execute("DELETE FROM candidates WHERE job_fp=?1", params![fp])
        .map_err(|e| e.to_string())?;
    for (rank, cand) in candidates.iter().enumerate() {
        tx.execute(
            "INSERT INTO candidates
               (job_fp, event_id, rank_in_job, cand_key, kind, status, reason, x, y, region_json, metrics_json, residuals_json)
             VALUES (?1,?2,?3,?4,?5,?6,?7,?8,?9,?10,?11,?12)",
            params![
                fp,
                snap.event_id,
                rank as i64,
                cand.cand_key,
                cand.kind,
                cand.status,
                cand.reason,
                cand.x,
                cand.y,
                cand.region.clone().unwrap_or(json!(null)).to_string(),
                cand.metrics.to_string(),
                serde_json::to_string(&cand.residuals).unwrap_or_else(|_| "[]".into()),
            ],
        )
        .map_err(|e| e.to_string())?;
    }
    tx.execute(
        "UPDATE jobs SET state='done', finished_at=?1, error='' WHERE fingerprint=?2",
        params![now_s(), fp],
    )
    .map_err(|e| e.to_string())?;
    tx.commit().map_err(|e| e.to_string())?;
    Ok((fp, RunOutcome::Created))
}

/// Test hook: leave a job half-finished to simulate a worker crash.
pub fn crash_after_running(
    db: &Db,
    snap: &Snapshot,
    partial_candidates: usize,
) -> Result<String, String> {
    let fp = fingerprint(snap);
    {
        let conn = db.conn.lock().unwrap();
        conn.execute(
            "INSERT OR IGNORE INTO jobs(fingerprint, event_id, state, created_at, worker) VALUES (?1,?2,'running',?3,'crashed-worker')",
            params![fp, snap.event_id, now_s()],
        )
        .map_err(|e| e.to_string())?;
    }
    let candidates = solver::solve(snap);
    let conn = db.conn.lock().unwrap();
    for (rank, cand) in candidates.iter().take(partial_candidates).enumerate() {
        conn.execute(
            "INSERT INTO candidates
               (job_fp, event_id, rank_in_job, cand_key, kind, status, reason, x, y, region_json, metrics_json, residuals_json)
             VALUES (?1,?2,?3,?4,?5,?6,?7,?8,?9,?10,?11,?12)",
            params![
                fp, snap.event_id, rank as i64, cand.cand_key, cand.kind, cand.status, cand.reason,
                cand.x, cand.y,
                cand.region.clone().unwrap_or(json!(null)).to_string(),
                cand.metrics.to_string(),
                serde_json::to_string(&cand.residuals).unwrap_or_else(|_| "[]".into()),
            ],
        )
        .map_err(|e| e.to_string())?;
    }
    Ok(fp)
}
