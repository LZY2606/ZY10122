//! Piecewise clock model with half-open `[start, end)` segments.
//!
//! At the instant two segments touch, the earlier one has already ended and
//! the later one begins, so `segment_at(join)` returns exactly one row.
//! Overlapping segments (which would make the offset ambiguous) are rejected
//! at write time.

use crate::db::Db;
use crate::models::ClockSegment;
use rusqlite::params;

pub fn list_segments(db: &Db, station_id: &str) -> Result<Vec<ClockSegment>, String> {
    let conn = db.conn.lock().unwrap();
    let mut stmt = conn
        .prepare(
            "SELECT seg_id, station_id, start_s, end_s, offset_s, sigma_s, note
             FROM clock_segments WHERE station_id=?1 ORDER BY start_s",
        )
        .map_err(|e| e.to_string())?;
    let rows = stmt
        .query_map(params![station_id], |r| {
            Ok(ClockSegment {
                seg_id: r.get(0)?,
                station_id: r.get(1)?,
                start_s: r.get(2)?,
                end_s: r.get(3)?,
                offset_s: r.get(4)?,
                sigma_s: r.get(5)?,
                note: r.get(6)?,
            })
        })
        .map_err(|e| e.to_string())?;
    let mut out = Vec::new();
    for row in rows {
        out.push(row.map_err(|e| e.to_string())?);
    }
    Ok(out)
}

/// Find the unique segment satisfying `start <= t < end` (NULL end = forever).
pub fn segment_at(segs: &[ClockSegment], t: f64) -> Option<&ClockSegment> {
    for seg in segs {
        let after_start = t >= seg.start_s;
        let before_end = match seg.end_s {
            Some(end) => t < end,
            None => true,
        };
        if after_start && before_end {
            return Some(seg);
        }
    }
    None
}

/// Half-open overlap test: `[a0,a1)` and `[b0,b1)` overlap iff a0 < b1 &&
/// b0 < a1. Touching boundaries (a1 == b0) do NOT overlap.
fn overlaps(a0: f64, a1: f64, b0: f64, b1: f64) -> bool {
    a0 < b1 && b0 < a1
}

pub fn add_segment(db: &Db, input: &crate::models::ClockSegmentReq) -> Result<i64, String> {
    if !input.start_s.is_finite() || !input.offset_s.is_finite() {
        return Err("start_s/offset_s must be finite".into());
    }
    if let Some(end) = input.end_s {
        if !end.is_finite() || !(end > input.start_s) {
            return Err("end_s must be finite and strictly greater than start_s".into());
        }
    }
    if !(input.sigma_s.is_finite() && input.sigma_s >= 0.0) {
        return Err("sigma_s must be finite and non-negative".into());
    }
    let existing = list_segments(db, &input.station_id)?;
    let new_end = input.end_s.unwrap_or(f64::INFINITY);
    for seg in &existing {
        let old_end = seg.end_s.unwrap_or(f64::INFINITY);
        if overlaps(input.start_s, new_end, seg.start_s, old_end) {
            return Err(format!(
                "half-open segment [{}, {}) overlaps existing segment [{}, {})",
                input.start_s,
                input
                    .end_s
                    .map(|v| format!("{v}"))
                    .unwrap_or_else(|| "∞".into()),
                seg.start_s,
                seg.end_s
                    .map(|v| format!("{v}"))
                    .unwrap_or_else(|| "∞".into()),
            ));
        }
    }
    let conn = db.conn.lock().unwrap();
    conn.execute(
        "INSERT INTO clock_segments (station_id, start_s, end_s, offset_s, sigma_s, note)
         VALUES (?1,?2,?3,?4,?5,?6)",
        params![
            input.station_id,
            input.start_s,
            input.end_s,
            input.offset_s,
            input.sigma_s,
            input.note
        ],
    )
    .map_err(|e| e.to_string())?;
    Ok(conn.last_insert_rowid())
}

/// Effective offset knowledge for a station at time `t`.
#[derive(Debug, Clone, Copy)]
pub struct EffectiveOffset {
    pub offset_s: f64,
    pub sigma_s: f64,
    /// True when the offset is pinned by a reviewer lock rather than a
    /// provisioning segment.
    pub locked: bool,
    /// True when no segment covers the instant; the offset is then free
    /// (large sigma) and becomes a clock-freedom degree of freedom.
    pub unknown: bool,
}

pub fn effective_offset(
    segments: &[ClockSegment],
    lock: Option<(f64, f64)>,
    t: f64,
) -> EffectiveOffset {
    if let Some((offset, sigma)) = lock {
        return EffectiveOffset {
            offset_s: offset,
            sigma_s: sigma,
            locked: true,
            unknown: false,
        };
    }
    match segment_at(segments, t) {
        Some(seg) => EffectiveOffset {
            offset_s: seg.offset_s,
            sigma_s: seg.sigma_s,
            locked: false,
            unknown: false,
        },
        None => EffectiveOffset {
            offset_s: 0.0,
            sigma_s: FREE_CLOCK_SIGMA,
            locked: false,
            unknown: true,
        },
    }
}

/// One second of uncertainty: structurally a free clock parameter.
pub const FREE_CLOCK_SIGMA: f64 = 1.0;
