//! Append-only derived evidence. Reviewer decisions never overwrite
//! observations or previous candidates; they are read by the next solve.

use crate::db::Db;
use crate::hash::json_fingerprint;
use crate::models::EvidenceReq;
use rusqlite::params;
use serde_json::Value;
use std::time::{SystemTime, UNIX_EPOCH};

#[derive(Debug, Clone)]
pub struct Evidence {
    pub content_id: String,
    pub kind: String,
    pub payload: Value,
}

fn now_s() -> f64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs_f64())
        .unwrap_or(0.0)
}

pub fn add_evidence(db: &Db, event_id: &str, req: EvidenceReq) -> Result<String, String> {
    validate(&req.kind, &req.payload)?;
    let content_id = json_fingerprint(&["evidence-v1", event_id, &req.kind], &req.payload);
    let conn = db.conn.lock().unwrap();
    conn.execute(
        "INSERT OR IGNORE INTO evidence (content_id, event_id, kind, payload_json, created_at)
         VALUES (?1,?2,?3,?4,?5)",
        params![
            content_id,
            event_id,
            req.kind,
            req.payload.to_string(),
            now_s()
        ],
    )
    .map_err(|e| e.to_string())?;
    Ok(content_id)
}

pub fn list_evidence(db: &Db, event_id: &str) -> Result<Vec<Evidence>, String> {
    let conn = db.conn.lock().unwrap();
    let mut stmt = conn
        .prepare("SELECT content_id, kind, payload_json FROM evidence WHERE event_id=?1 ORDER BY created_at, content_id")
        .map_err(|e| e.to_string())?;
    let rows = stmt
        .query_map(params![event_id], |r| {
            let raw: String = r.get(2)?;
            Ok(Evidence {
                content_id: r.get(0)?,
                kind: r.get(1)?,
                payload: serde_json::from_str(&raw).unwrap_or(Value::Null),
            })
        })
        .map_err(|e| e.to_string())?;
    let mut out = Vec::new();
    for row in rows {
        out.push(row.map_err(|e| e.to_string())?);
    }
    Ok(out)
}

fn validate(kind: &str, payload: &Value) -> Result<(), String> {
    match kind {
        "exclude_peak" => {
            require_fields(payload, &["station_a", "station_b", "peak_index"])?;
        }
        "lock_clock" => {
            require_fields(payload, &["station_id", "offset_s"])?;
        }
        "keep_pair" => {
            require_fields(payload, &["candidate_a_key", "candidate_b_key"])?;
        }
        other => return Err(format!("unknown evidence kind '{other}'")),
    }
    Ok(())
}

fn require_fields(payload: &Value, fields: &[&str]) -> Result<(), String> {
    let obj = payload.as_object().ok_or("payload must be a JSON object")?;
    for field in fields {
        if !obj.contains_key(*field) {
            return Err(format!("payload missing field '{field}'"));
        }
    }
    Ok(())
}
