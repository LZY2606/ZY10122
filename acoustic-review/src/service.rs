//! Application services used by both the HTTP layer and the generator.

use crate::db::Db;
use crate::models::*;
use crate::units::{PhysicalModel, COORDINATE_UNIT, DELAY_UNIT, SPEED_UNIT, TIME_UNIT};
use rusqlite::params;
use serde_json::{json, Value};
use std::time::{SystemTime, UNIX_EPOCH};

fn now_s() -> f64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs_f64())
        .unwrap_or(0.0)
}

pub fn default_model() -> PhysicalModel {
    PhysicalModel::default()
}

pub fn list_stations(db: &Db) -> Result<Vec<Station>, String> {
    let conn = db.conn.lock().unwrap();
    let mut stmt = conn
        .prepare("SELECT station_id, name, x, y FROM stations ORDER BY station_id")
        .map_err(|e| e.to_string())?;
    let rows = stmt
        .query_map([], |r| {
            Ok(Station {
                station_id: r.get(0)?,
                name: r.get(1)?,
                x: r.get(2)?,
                y: r.get(3)?,
            })
        })
        .map_err(|e| e.to_string())?;
    let mut out = Vec::new();
    for row in rows {
        out.push(row.map_err(|e| e.to_string())?);
    }
    Ok(out)
}

pub fn create_station(db: &Db, req: CreateStationReq) -> Result<(), String> {
    if req.station_id.trim().is_empty() {
        return Err("station_id must be non-empty".into());
    }
    if !req.x.is_finite() || !req.y.is_finite() {
        return Err("station coordinates must be finite metres".into());
    }
    let conn = db.conn.lock().unwrap();
    conn.execute(
        "INSERT OR IGNORE INTO stations(station_id, name, x, y, created_at) VALUES (?1,?2,?3,?4,?5)",
        params![req.station_id, req.name, req.x, req.y, now_s()],
    )
    .map_err(|e| e.to_string())?;
    Ok(())
}

pub fn list_events(db: &Db) -> Result<Vec<EventHeader>, String> {
    let conn = db.conn.lock().unwrap();
    let mut stmt = conn
        .prepare(
            "SELECT event_id, title, t_start, t_end, sound_speed, coord_unit, time_unit
             FROM events ORDER BY t_start, event_id",
        )
        .map_err(|e| e.to_string())?;
    let rows = stmt
        .query_map([], |r| {
            Ok(EventHeader {
                event_id: r.get(0)?,
                title: r.get(1)?,
                t_start: r.get(2)?,
                t_end: r.get(3)?,
                sound_speed: r.get(4)?,
                coordinate_unit: r.get(5)?,
                time_unit: r.get(6)?,
            })
        })
        .map_err(|e| e.to_string())?;
    let mut out = Vec::new();
    for row in rows {
        out.push(row.map_err(|e| e.to_string())?);
    }
    Ok(out)
}

pub fn create_event(db: &Db, req: CreateEventReq) -> Result<(), String> {
    if req.event_id.trim().is_empty() {
        return Err("event_id must be non-empty".into());
    }
    if !req.t_start.is_finite() || !req.t_end.is_finite() || req.t_end <= req.t_start {
        return Err("event needs finite t_start < t_end (half-open [t_start, t_end))".into());
    }
    let model = PhysicalModel {
        sound_speed: req.sound_speed,
        ..PhysicalModel::default()
    };
    model.validate()?;
    let conn = db.conn.lock().unwrap();
    conn.execute(
        "INSERT INTO events(event_id, title, t_start, t_end, sound_speed, coord_unit, time_unit, created_at)
         VALUES (?1,?2,?3,?4,?5,?6,?7,?8)",
        params![req.event_id, req.title, req.t_start, req.t_end, req.sound_speed, COORDINATE_UNIT, TIME_UNIT, now_s()],
    )
    .map_err(|e| e.to_string())?;
    Ok(())
}

/// Link every observation in the event's half-open time window belonging to a
/// known station. Repeated reports from one station are all retained (the
/// solver/timeline shows duplicates); missing stations simply never appear.
pub fn link_window(db: &Db, event_id: &str) -> Result<usize, String> {
    let (t0, t1, _, _) = crate::jobs::linked_observations(db, event_id)?;
    let conn = db.conn.lock().unwrap();
    let n = conn
        .execute(
            "INSERT OR IGNORE INTO event_observations(event_id, obs_id)
             SELECT ?1, obs_id FROM observations
             WHERE (event_ref = ?1
                    OR (event_ref IS NULL AND local_onset_s >= ?2 AND local_onset_s < ?3))",
            params![event_id, t0, t1],
        )
        .map_err(|e| e.to_string())?;
    Ok(n)
}

fn latest_done_job(db: &Db, event_id: &str) -> Result<Option<String>, String> {
    let conn = db.conn.lock().unwrap();
    let fp: Option<String> = conn
        .query_row(
            "SELECT fingerprint FROM jobs WHERE event_id=?1 AND state='done'
             ORDER BY finished_at DESC, fingerprint DESC LIMIT 1",
            params![event_id],
            |r| r.get(0),
        )
        .ok()
        .flatten();
    Ok(fp)
}

fn observations_payload(db: &Db, event_id: &str) -> Result<Value, String> {
    let conn = db.conn.lock().unwrap();
    let mut stmt = conn
        .prepare(
            "SELECT o.obs_id, o.station_id, o.local_onset_s, o.peak_band_hz, o.snr_db,
                    o.batch_fp,
                    EXISTS (SELECT 1 FROM xcorr_delays x WHERE (x.obs_a=o.obs_id OR x.obs_b=o.obs_id) AND x.multipath=1) AS mp,
                    EXISTS (SELECT 1 FROM evidence e WHERE e.event_id=?1
                              AND e.kind='exclude_peak') AS any_excl
             FROM event_observations eo
             JOIN observations o ON o.obs_id = eo.obs_id
             WHERE eo.event_id=?1
             ORDER BY o.local_onset_s, o.station_id, o.obs_id",
        )
        .map_err(|e| e.to_string())?;
    let rows = stmt
        .query_map(params![event_id], |r| {
            Ok(json!({
                "obs_id": r.get::<_, i64>(0)?,
                "station_id": r.get::<_, String>(1)?,
                "local_onset_s": r.get::<_, f64>(2)?,
                "peak_band_hz": r.get::<_, f64>(3)?,
                "snr_db": r.get::<_, f64>(4)?,
                "batch_fp": r.get::<_, String>(5)?,
                "multipath": r.get::<_, i64>(6)? != 0,
            }))
        })
        .map_err(|e| e.to_string())?;
    let mut obs = Vec::new();
    for row in rows {
        obs.push(row.map_err(|e| e.to_string())?);
    }
    Ok(json!(obs))
}

fn edge_payload(db: &Db, event_id: &str) -> Result<Value, String> {
    let conn = db.conn.lock().unwrap();
    let mut stmt = conn
        .prepare(
            "SELECT x.xc_id, sa.station_id, sb.station_id, x.delay_s, x.sigma_s, x.peak_index, x.multipath,
                   oa.local_onset_s, ob.local_onset_s
             FROM event_observations ea
             JOIN observations oa ON oa.obs_id = ea.obs_id
             JOIN xcorr_delays x ON x.obs_a = oa.obs_id
             JOIN observations ob ON ob.obs_id = x.obs_b
             JOIN event_observations eb ON eb.obs_id = ob.obs_id AND eb.event_id = ea.event_id
             JOIN stations sa ON sa.station_id = oa.station_id
             JOIN stations sb ON sb.station_id = ob.station_id
             WHERE ea.event_id=?1
             ORDER BY sa.station_id, sb.station_id, x.peak_index",
        )
        .map_err(|e| e.to_string())?;
    let rows = stmt
        .query_map(params![event_id], |r| {
            Ok(json!({
                "xc_id": r.get::<_, i64>(0)?,
                "station_a": r.get::<_, String>(1)?,
                "station_b": r.get::<_, String>(2)?,
                "delay_s": r.get::<_, f64>(3)?,
                "sigma_s": r.get::<_, f64>(4)?,
                "peak_index": r.get::<_, i64>(5)?,
                "multipath": r.get::<_, i64>(6)? != 0,
                "onset_a_s": r.get::<_, f64>(7)?,
                "onset_b_s": r.get::<_, f64>(8)?,
                "delay_unit": DELAY_UNIT,
            }))
        })
        .map_err(|e| e.to_string())?;
    let mut edges = Vec::new();
    for row in rows {
        edges.push(row.map_err(|e| e.to_string())?);
    }
    Ok(json!(edges))
}

fn candidates_payload(db: &Db, job_fp: &str) -> Result<Vec<Candidate>, String> {
    let conn = db.conn.lock().unwrap();
    let mut stmt = conn
        .prepare(
            "SELECT cand_id, job_fp, rank_in_job, cand_key, kind, status, reason,
                    x, y, region_json, metrics_json, residuals_json
             FROM candidates WHERE job_fp=?1 ORDER BY rank_in_job",
        )
        .map_err(|e| e.to_string())?;
    let rows = stmt
        .query_map(params![job_fp], |r| {
            let region_raw: String = r.get(9)?;
            let metrics_raw: String = r.get(10)?;
            let residuals_raw: String = r.get(11)?;
            let region: Option<Region> = serde_json::from_str(&region_raw).ok().flatten();
            Ok(Candidate {
                cand_id: r.get(0)?,
                cand_key: r.get(3)?,
                job_fp: r.get(1)?,
                rank_in_job: r.get(2)?,
                kind: r.get(4)?,
                status: r.get(5)?,
                reason: r.get(6)?,
                x: r.get(7)?,
                y: r.get(8)?,
                region,
                metrics: serde_json::from_str(&metrics_raw).unwrap_or(json!({})),
                residuals: serde_json::from_str(&residuals_raw).unwrap_or_default(),
                kept: false,
            })
        })
        .map_err(|e| e.to_string())?;
    let mut out = Vec::new();
    for row in rows {
        let mut cand = row.map_err(|e| e.to_string())?;
        cand.kept = cand.metrics.get("kept_pair").is_some();
        out.push(cand);
    }
    Ok(out)
}

pub fn event_detail(db: &Db, event_id: &str) -> Result<Value, String> {
    let (t0, t1, speed, linked) = crate::jobs::linked_observations(db, event_id)?;
    let job_fp = latest_done_job(db, event_id)?;
    let candidates = match &job_fp {
        Some(fp) => candidates_payload(db, fp)?,
        None => Vec::new(),
    };
    let stations = list_stations(db)?;
    let evidence = crate::evidence::list_evidence(db, event_id)?
        .into_iter()
        .map(|e| json!({"content_id": e.content_id, "kind": e.kind, "payload": e.payload}))
        .collect::<Vec<_>>();
    let mut segments = Vec::new();
    for st in &stations {
        for seg in crate::clock::list_segments(db, &st.station_id)? {
            segments.push(json!({
                "seg_id": seg.seg_id,
                "station_id": seg.station_id,
                "start_s": seg.start_s,
                "end_s": seg.end_s,
                "offset_s": seg.offset_s,
                "sigma_s": seg.sigma_s,
                "note": seg.note,
                "time_unit": TIME_UNIT,
            }));
        }
    }

    let jobs = {
        let conn = db.conn.lock().unwrap();
        let mut stmt = conn
            .prepare("SELECT fingerprint, state, created_at, finished_at FROM jobs WHERE event_id=?1 ORDER BY created_at DESC")
            .map_err(|e| e.to_string())?;
        let rows = stmt
            .query_map(params![event_id], |r| -> rusqlite::Result<Value> {
                Ok(json!({
                    "fingerprint": r.get::<_, String>(0)?,
                    "state": r.get::<_, String>(1)?,
                    "created_at": r.get::<_, Option<f64>>(2)?,
                    "finished_at": r.get::<_, Option<f64>>(3)?,
                }))
            })
            .map_err(|e| e.to_string())?;
        let mut out = Vec::new();
        for row in rows {
            out.push(row.map_err(|e| e.to_string())?);
        }
        out
    };

    Ok(json!({
        "event": {
            "event_id": event_id,
            "t_start": t0,
            "t_end": t1,
            "interval_semantics": "half-open [t_start, t_end)",
            "sound_speed": speed,
            "speed_unit": SPEED_UNIT,
            "coordinate_unit": COORDINATE_UNIT,
            "time_unit": TIME_UNIT,
            "delay_unit": DELAY_UNIT,
            "linked_observation_count": linked.len(),
        },
        "stations": stations,
        "clock_segments": segments,
        "observations": observations_payload(db, event_id)?,
        "edges": edge_payload(db, event_id)?,
        "evidence": evidence,
        "jobs": jobs,
        "latest_done_job": job_fp,
        "candidates": candidates,
        "observable_radius_m": default_model().observable_radius,
    }))
}

/// Run a fresh solve using the current derived evidence. Original observations
/// and earlier candidate rows are never modified.
pub fn solve_event(db: &Db, event_id: &str) -> Result<String, String> {
    let model = default_model();
    let snap = crate::jobs::build_snapshot(db, event_id, model)?;
    let (fp, _) = crate::jobs::enqueue_and_run(db, &snap, "http-worker")?;
    Ok(fp)
}
