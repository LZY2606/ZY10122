//! End-to-end tests on an in-memory database.

use acoustic_review::*;

fn seeded() -> Db {
    let db = Db::open_memory().unwrap();
    synth::generate(&db);
    db
}

fn solve(db: &Db, event: &str) -> serde_json::Value {
    let fp = service::solve_event(db, event).unwrap();
    let conn = db.conn.lock().unwrap();
    let mut stmt = conn
        .prepare("SELECT kind, status, reason, x, y FROM candidates WHERE job_fp=?1 ORDER BY rank_in_job")
        .unwrap();
    let mut rows = stmt.query(rusqlite::params![fp]).unwrap();
    let mut out = Vec::new();
    while let Some(r) = rows.next().unwrap() {
        out.push(serde_json::json!({
            "kind": r.get::<_, String>(0).unwrap(),
            "status": r.get::<_, String>(1).unwrap(),
            "reason": r.get::<_, String>(2).unwrap(),
            "x": r.get::<_, Option<f64>>(3).unwrap(),
            "y": r.get::<_, Option<f64>>(4).unwrap(),
        }));
    }
    serde_json::json!(out)
}

#[test]
fn impact_event_localizes_near_truth() {
    let db = seeded();
    let cands = solve(&db, "EVT-IMPACT");
    let arr = cands.as_array().unwrap();
    assert!(!arr.is_empty(), "expected candidates, got {cands}");
    let best = &arr[0];
    assert_eq!(best["kind"], "point");
    assert_eq!(best["status"], "determined");
    let x = best["x"].as_f64().unwrap();
    let y = best["y"].as_f64().unwrap();
    let err = ((x - 90.0).powi(2) + (y - 20.0).powi(2)).sqrt();
    assert!(
        err < 6.0,
        "impact localization error {err} m too large: {cands}"
    );
}

#[test]
fn mirror_event_has_two_inseparable_points() {
    let db = seeded();
    let cands = solve(&db, "EVT-MIRROR");
    let arr = cands.as_array().unwrap();
    let points: Vec<_> = arr.iter().filter(|c| c["kind"] == "point").collect();
    assert_eq!(points.len(), 2, "expected mirror pair, got {cands}");
    for p in &points {
        assert_eq!(p["status"], "underdetermined");
        assert!(p["reason"].as_str().unwrap().contains("sensor_geometry"));
    }
}

#[test]
fn drift_event_is_clock_underdetermined_region() {
    let db = seeded();
    let cands = solve(&db, "EVT-DRIFT");
    let arr = cands.as_array().unwrap();
    assert_eq!(arr.len(), 1);
    assert_eq!(arr[0]["kind"], "region");
    assert_eq!(arr[0]["status"], "underdetermined");
    assert_eq!(arr[0]["reason"], "clock_freedom");
}

#[test]
fn keep_pair_marks_both_mirror_locations() {
    let db = Db::open_memory().unwrap();
    synth::generate(&db);
    let fp = service::solve_event(&db, "EVT-MIRROR").unwrap();
    let keys: Vec<String> = {
        let conn = db.conn.lock().unwrap();
        let mut stmt = conn
            .prepare("SELECT cand_key FROM candidates WHERE job_fp=?1 ORDER BY rank_in_job")
            .unwrap();
        let mut rows = stmt.query(rusqlite::params![fp]).unwrap();
        let mut v = Vec::new();
        while let Some(r) = rows.next().unwrap() {
            v.push(r.get::<_, String>(0).unwrap());
        }
        v
    };
    assert_eq!(keys.len(), 2);
    crate::evidence::add_evidence(
        &db,
        "EVT-MIRROR",
        crate::models::EvidenceReq {
            kind: "keep_pair".into(),
            payload: serde_json::json!({"candidate_a_key": keys[0], "candidate_b_key": keys[1]}),
        },
    )
    .unwrap();
    let fp2 = service::solve_event(&db, "EVT-MIRROR").unwrap();
    assert_ne!(fp, fp2);
    let conn = db.conn.lock().unwrap();
    let kept: i64 = conn
        .query_row(
            "SELECT COUNT(*) FROM candidates WHERE job_fp=?1",
            rusqlite::params![fp2],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(kept, 2, "both inseparable locations retained");
    let metrics: String = conn
        .query_row(
            "SELECT metrics_json FROM candidates WHERE job_fp=?1 AND rank_in_job=0",
            rusqlite::params![fp2],
            |r| r.get(0),
        )
        .unwrap();
    assert!(metrics.contains("kept_pair"));
}

#[test]
fn exclude_reflected_peak_changes_only_new_job() {
    let db = Db::open_memory().unwrap();
    synth::generate(&db);
    let fp1 = service::solve_event(&db, "EVT-IMPACT").unwrap();

    let excluded_before: i64 = {
        let conn = db.conn.lock().unwrap();
        let raw: String = conn
            .query_row(
                "SELECT residuals_json FROM candidates WHERE job_fp=?1 AND rank_in_job=0",
                rusqlite::params![fp1],
                |r| r.get(0),
            )
            .unwrap();
        let v: serde_json::Value = serde_json::from_str(&raw).unwrap();
        v.as_array()
            .unwrap()
            .iter()
            .filter(|r| r["excluded"] == true)
            .count() as i64
    };

    crate::evidence::add_evidence(
        &db,
        "EVT-IMPACT",
        crate::models::EvidenceReq {
            kind: "exclude_peak".into(),
            payload: serde_json::json!({"station_a": "A", "station_b": "D", "peak_index": 0}),
        },
    )
    .unwrap();
    let fp2 = service::solve_event(&db, "EVT-IMPACT").unwrap();
    assert_ne!(fp1, fp2);

    let conn = db.conn.lock().unwrap();
    let raw: String = conn
        .query_row(
            "SELECT residuals_json FROM candidates WHERE job_fp=?1 AND rank_in_job=0",
            rusqlite::params![fp2],
            |r| r.get(0),
        )
        .unwrap();
    let v: serde_json::Value = serde_json::from_str(&raw).unwrap();
    let excluded_after = v
        .as_array()
        .unwrap()
        .iter()
        .filter(|r| r["excluded"] == true)
        .count() as i64;
    assert!(
        excluded_after > excluded_before,
        "reflected peak excluded in new job: {v}"
    );
    // the old job is untouched
    let old_count: i64 = conn
        .query_row(
            "SELECT COUNT(*) FROM candidates WHERE job_fp=?1",
            rusqlite::params![fp1],
            |r| r.get(0),
        )
        .unwrap();
    assert!(old_count >= 1);
}
