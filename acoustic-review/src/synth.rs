//! Deterministic synthetic data generator.
//!
//! Produces three review scenarios:
//! 1. `EVT-IMPACT`  - five stations, one station silent, one duplicated
//!    report, and a wall reflection leaking into a cross-correlation peak.
//! 2. `EVT-MIRROR`  - three collinear stations: two mirror locations are
//!    geometrically inseparable (sensor geometry underdetermination).
//! 3. `EVT-DRIFT`   - a station with NO covering clock segment: the residual
//!    clock degree of freedom yields a region (clock freedom).

use crate::db::Db;
use crate::ingest::ingest;
use crate::models::{
    ClockSegmentReq, CreateEventReq, CreateStationReq, IngestObs, IngestReq, IngestXcorr,
};
use crate::service::{create_event, create_station, link_window};
use serde_json::json;

struct Rng(u64);
impl Rng {
    fn next(&mut self) -> f64 {
        let mut x = self.0;
        x ^= x << 13;
        x ^= x >> 7;
        x ^= x << 17;
        self.0 = x;
        ((x >> 11) as f64) / (1u64 << 53) as f64
    }
    fn normal(&mut self) -> f64 {
        // Box-Muller
        let u1 = self.next().max(1e-12);
        let u2 = self.next();
        (-2.0 * u1.ln()).sqrt() * (std::f64::consts::TAU * u2).cos()
    }
}

pub const SOUND_SPEED: f64 = 343.0;

fn station(db: &Db, id: &str, name: &str, x: f64, y: f64) {
    create_station(
        db,
        CreateStationReq {
            station_id: id.into(),
            name: name.into(),
            x,
            y,
        },
    )
    .expect("create station");
}

fn seg(db: &Db, sid: &str, start: f64, end: Option<f64>, offset: f64, sigma: f64) {
    crate::clock::add_segment(
        db,
        &ClockSegmentReq {
            station_id: sid.into(),
            start_s: start,
            end_s: end,
            offset_s: offset,
            sigma_s: sigma,
            note: "synthetic".into(),
        },
    )
    .expect("add clock segment");
}

fn ingest_obs(db: &Db, fp: &str, observations: Vec<IngestObs>) {
    let req = IngestReq {
        fingerprint: Some(fp.into()),
        received_at: Some(1_700_000_000.0),
        observations,
    };
    ingest(db, req).expect("ingest synthetic batch");
}

fn obs(
    station_id: &str,
    event_ref: &str,
    t_device: f64,
    onset: f64,
    band: f64,
    snr: f64,
    xcorr: Vec<IngestXcorr>,
) -> IngestObs {
    IngestObs {
        station_id: station_id.into(),
        event_ref: Some(event_ref.into()),
        t_device,
        local_onset_s: onset,
        peak_band_hz: band,
        snr_db: snr,
        xcorr,
    }
}

/// Generate station layout, clock segments, events and batches.
pub fn generate(db: &Db) -> serde_json::Value {
    let mut rng = Rng(0x5EED_1116_0001);

    // Five stations around a 120 m x 80 m warehouse floor.
    station(db, "A", "north-west mast", 0.0, 40.0);
    station(db, "B", "north-east mast", 120.0, 40.0);
    station(db, "C", "south-west mast", 0.0, -40.0);
    station(db, "D", "south-east mast", 120.0, -40.0);
    station(db, "E", "centre mast", 60.0, 0.0);
    // Extra mast collinear with A and B for the mirror-ambiguity scenario.
    station(db, "F", "north-centre mast", 60.0, 40.0);

    // Half-open calibration segments [0, 60) and [60, +inf).
    for sid in ["A", "B", "C", "D", "E", "F"] {
        let off = (rng.next() - 0.5) * 0.01;
        seg(db, sid, 0.0, Some(60.0), off, 0.0015);
        let off2 = off + (rng.next() - 0.5) * 0.004;
        seg(db, sid, 60.0, None, off2, 0.0015);
    }

    generate_impact(db, &mut rng);
    generate_mirror(db, &mut rng);
    generate_drift(db, &mut rng);

    json!({"generated": true, "events": ["EVT-IMPACT", "EVT-MIRROR", "EVT-DRIFT"]})
}

fn range_to(x: f64, y: f64, sx: f64, sy: f64) -> f64 {
    ((x - sx).powi(2) + (y - sy).powi(2)).sqrt()
}

fn generate_impact(db: &Db, rng: &mut Rng) {
    create_event(
        db,
        CreateEventReq {
            event_id: "EVT-IMPACT".into(),
            title: "Metal impact, north aisle".into(),
            t_start: 40.0,
            t_end: 40.5,
            sound_speed: SOUND_SPEED,
        },
    )
    .unwrap();

    let (x, y) = (90.0f64, 20.0f64);
    let pos = [
        ("A", 0.0, 40.0),
        ("B", 120.0, 40.0),
        ("C", 0.0, -40.0),
        ("D", 120.0, -40.0),
        ("E", 60.0, 0.0),
    ];
    let offsets: std::collections::BTreeMap<&str, f64> = [
        ("A", 0.003),
        ("B", -0.002),
        ("C", 0.0011),
        ("D", -0.004),
        ("E", 0.0022),
    ]
    .into_iter()
    .collect();

    let t0 = 40.1;
    let mut rows: Vec<IngestObs> = Vec::new();
    // Station C is intentionally absent (missing report).
    for (sid, sx, sy) in pos.into_iter().filter(|(id, _, _)| *id != "C") {
        let travel = range_to(x, y, sx, sy) / SOUND_SPEED;
        let onset = t0 + travel + offsets[sid] + rng.normal() * 0.0006;
        rows.push(obs(
            sid,
            "EVT-IMPACT",
            onset,
            onset,
            1250.0 + rng.next() * 400.0,
            18.0 + rng.next() * 6.0,
            vec![],
        ));
    }
    // Duplicated upload from station B (same onset rounded, second batch copy).
    {
        let b_onset = rows[1].local_onset_s;
        rows.push(obs(
            "B",
            "EVT-IMPACT",
            b_onset,
            b_onset,
            1280.0,
            19.1,
            vec![],
        ));
    }

    // Cross-correlation edges (direct peaks) plus one reflected path A<->D
    // (delayed by ~8 ms, multipath flag set).
    let delay =
        |rows: &[IngestObs], i: usize, j: usize| rows[j].local_onset_s - rows[i].local_onset_s;
    let indices = [("A", 0usize), ("B", 1), ("D", 2), ("E", 3)];
    let mut edges: Vec<(usize, usize, i64, bool)> = vec![
        (0, 1, 0, false),
        (0, 2, 0, false),
        (0, 3, 0, false),
        (1, 2, 0, false),
        (1, 3, 0, false),
        (2, 3, 0, false),
    ];
    // A wall reflection between A and D contaminated the nominal direct-peak
    // pick: peak_index 0 (it masquerades as direct), multipath flagged, +8.2ms.
    // The reviewer must explicitly distinguish it from direct evidence.
    {
        let (i, j) = (0usize, 2usize);
        let d = rows[j].local_onset_s - rows[i].local_onset_s + 0.0082;
        rows[i].xcorr.push(IngestXcorr {
            obs_index: j,
            delay_s: d,
            sigma_s: 0.0012,
            peak_index: 0,
            multipath: true,
        });
    }
    let _ = indices;
    for (i, j, peak, mp) in edges.drain(..) {
        let d = delay(&rows, i, j) + rng.normal() * 0.0004;
        rows[i].xcorr.push(IngestXcorr {
            obs_index: j,
            delay_s: d,
            sigma_s: 0.0012,
            peak_index: peak,
            multipath: mp,
        });
    }

    ingest_obs(db, "batch-impact-001", rows);
    link_window(db, "EVT-IMPACT").unwrap();
}

fn generate_mirror(db: &Db, _rng: &mut Rng) {
    create_event(
        db,
        CreateEventReq {
            event_id: "EVT-MIRROR".into(),
            title: "Two-collinear-baseline ambiguity".into(),
            t_start: 40.0,
            t_end: 40.5,
            sound_speed: SOUND_SPEED,
        },
    )
    .unwrap();

    // Three collinear stations (y = 40). The source off the line and its
    // reflection across the line produce identical range differences: two
    // inseparable mirror points.
    let (x, y) = (40.0f64, 25.0f64);
    let pos = [("A", 0.0, 40.0), ("F", 60.0, 40.0), ("B", 120.0, 40.0)];
    let offsets = [("A", 0.003), ("F", 0.0010), ("B", -0.002)];
    let t0 = 40.12;
    let mut rows: Vec<IngestObs> = Vec::new();
    for ((sid, sx, sy), (_, off)) in pos.iter().zip(offsets.iter()) {
        let onset = t0 + range_to(x, y, *sx, *sy) / SOUND_SPEED + off;
        rows.push(obs(sid, "EVT-MIRROR", onset, onset, 900.0, 15.0, vec![]));
    }
    for (i, j) in [(0usize, 1usize), (0, 2), (1, 2)] {
        let d = rows[j].local_onset_s - rows[i].local_onset_s;
        rows[i].xcorr.push(IngestXcorr {
            obs_index: j,
            delay_s: d,
            sigma_s: 0.001,
            peak_index: 0,
            multipath: false,
        });
    }
    ingest_obs(db, "batch-mirror-001", rows);
    link_window(db, "EVT-MIRROR").unwrap();
}

fn generate_drift(db: &Db, _rng: &mut Rng) {
    create_event(
        db,
        CreateEventReq {
            event_id: "EVT-DRIFT".into(),
            title: "Uncalibrated collector segment".into(),
            t_start: 120.0,
            t_end: 120.5,
            sound_speed: SOUND_SPEED,
        },
    )
    .unwrap();

    // Remove station D's clock coverage for t>=60 so its offset is free.
    {
        let conn = db.conn.lock().unwrap();
        conn.execute(
            "DELETE FROM clock_segments WHERE station_id='D' AND start_s>=60",
            [],
        )
        .unwrap();
    }

    let (x, y) = (30.0f64, -10.0f64);
    let pos = [
        ("A", 0.0, 40.0),
        ("C", 0.0, -40.0),
        ("D", 120.0, -40.0),
        ("E", 60.0, 0.0),
    ];
    let t0 = 120.1;
    let mut rows: Vec<IngestObs> = Vec::new();
    // D's true offset is large and unknown; use raw device onset anyway.
    let true_off = |sid: &str| match sid {
        "A" => 0.0041,
        "C" => 0.0020,
        "D" => 0.0370,
        "E" => 0.0030,
        _ => 0.0,
    };
    for (sid, sx, sy) in pos {
        let onset = t0 + range_to(x, y, sx, sy) / SOUND_SPEED + true_off(sid);
        rows.push(obs(sid, "EVT-DRIFT", onset, onset, 1100.0, 14.0, vec![]));
    }
    for (i, j) in [(0usize, 1usize), (0, 2), (0, 3), (1, 2), (2, 3)] {
        let d = rows[j].local_onset_s - rows[i].local_onset_s;
        rows[i].xcorr.push(IngestXcorr {
            obs_index: j,
            delay_s: d,
            sigma_s: 0.001,
            peak_index: 0,
            multipath: false,
        });
    }
    ingest_obs(db, "batch-drift-001", rows);
    link_window(db, "EVT-DRIFT").unwrap();
}
