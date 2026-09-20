//! Batch ingestion.
//!
//! A batch is identified by a fingerprint. Re-posting the same fingerprint is
//! a no-op: the rows committed on the first attempt are returned and no
//! duplicate observations appear, which is the post-crash duplicate guard.

use crate::db::Db;
use crate::hash::json_fingerprint;
use crate::models::IngestReq;
use rusqlite::params;
use serde_json::json;

pub struct IngestResult {
    pub fingerprint: String,
    pub already_present: bool,
    pub observation_ids: Vec<i64>,
}

pub fn fingerprint_of(req: &IngestReq) -> String {
    req.fingerprint.clone().unwrap_or_else(|| {
        json_fingerprint(
            &["batch-v1"],
            &serde_json::to_value(&req.observations).unwrap_or(json!(null)),
        )
    })
}

pub fn ingest(db: &Db, req: IngestReq) -> Result<IngestResult, String> {
    if req.observations.is_empty() {
        return Err("batch contains no observations".into());
    }
    for (i, obs) in req.observations.iter().enumerate() {
        if !obs.local_onset_s.is_finite() || !obs.t_device.is_finite() {
            return Err(format!("observation {i} has non-finite time fields"));
        }
        if obs.peak_band_hz < 0.0 || !obs.snr_db.is_finite() {
            return Err(format!("observation {i} has invalid band/SNR"));
        }
        for x in &obs.xcorr {
            if x.obs_index >= req.observations.len() || x.obs_index == i {
                return Err(format!("observation {i} has an out-of-range xcorr link"));
            }
            if !x.sigma_s.is_finite() || x.sigma_s <= 0.0 {
                return Err(format!("observation {i} has invalid xcorr sigma"));
            }
        }
    }

    let fp = fingerprint_of(&req);
    let now = req.received_at.unwrap_or_else(|| current_epoch_s());
    let canonical = crate::hash::canonical_json(&json!({
        "observations": req.observations,
        "received_at": null,
    }));

    let mut conn = db.conn.lock().unwrap();
    let tx = conn.transaction().map_err(|e| e.to_string())?;

    let already: i64 = tx
        .query_row(
            "SELECT COUNT(*) FROM batches WHERE fingerprint=?1",
            params![fp],
            |r| r.get(0),
        )
        .map_err(|e| e.to_string())?;

    if already > 0 {
        let mut ids = Vec::new();
        {
            let mut stmt = tx
                .prepare("SELECT obs_id FROM observations WHERE batch_fp=?1 ORDER BY obs_id")
                .map_err(|e| e.to_string())?;
            let rows = stmt
                .query_map(params![fp], |r| r.get::<_, i64>(0))
                .map_err(|e| e.to_string())?;
            for row in rows {
                ids.push(row.map_err(|e| e.to_string())?);
            }
        }
        return Ok(IngestResult {
            fingerprint: fp,
            already_present: true,
            observation_ids: ids,
        });
    }

    tx.execute(
        "INSERT INTO batches(fingerprint, received_at, payload_json) VALUES (?1,?2,?3)",
        params![fp, now, canonical],
    )
    .map_err(|e| e.to_string())?;

    let mut ids = Vec::with_capacity(req.observations.len());
    for (seq, obs) in req.observations.iter().enumerate() {
        let station_exists: i64 = tx
            .query_row(
                "SELECT COUNT(*) FROM stations WHERE station_id=?1",
                params![obs.station_id],
                |r| r.get(0),
            )
            .map_err(|e| e.to_string())?;
        if station_exists == 0 {
            return Err(format!("unknown station_id '{}'", obs.station_id));
        }
        tx.execute(
            "INSERT INTO observations
               (batch_fp, station_id, event_ref, t_device, local_onset_s, peak_band_hz, snr_db, created_seq)
             VALUES (?1,?2,?3,?4,?5,?6,?7,?8)",
            params![
                fp,
                obs.station_id,
                obs.event_ref,
                obs.t_device,
                obs.local_onset_s,
                obs.peak_band_hz,
                obs.snr_db,
                seq as i64
            ],
        )
        .map_err(|e| e.to_string())?;
        ids.push(tx.last_insert_rowid());
    }

    for (i, obs) in req.observations.iter().enumerate() {
        for link in &obs.xcorr {
            let (a, b) = if i < link.obs_index {
                (ids[i], ids[link.obs_index])
            } else {
                (ids[link.obs_index], ids[i])
            };
            tx.execute(
                "INSERT INTO xcorr_delays (obs_a, obs_b, delay_s, sigma_s, peak_index, multipath)
                 VALUES (?1,?2,?3,?4,?5,?6)",
                params![
                    a,
                    b,
                    link.delay_s,
                    link.sigma_s,
                    link.peak_index,
                    link.multipath as i64
                ],
            )
            .map_err(|e| e.to_string())?;
        }
    }

    tx.commit().map_err(|e| e.to_string())?;
    Ok(IngestResult {
        fingerprint: fp,
        already_present: false,
        observation_ids: ids,
    })
}

fn current_epoch_s() -> f64 {
    use std::time::{SystemTime, UNIX_EPOCH};
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs_f64())
        .unwrap_or(0.0)
}
