//! 时钟分段（半开边界）与求解器欠定几何的集成测试。

use acoustic_review::{
    clock_boundary_case, db, model::*, solver, synthetic_three_station_case,
};
use rusqlite::Connection;
use tempfile::tempdir;

fn fresh_conn() -> Connection {
    // keep() 保留临时目录，避免函数返回时数据库随 TempDir 被删除。
    let dir = tempdir().unwrap().keep();
    let path = dir.join("t.db");
    db::open(path.to_str().unwrap()).unwrap()
}

// ---------- 半开时钟边界 ----------

#[test]
fn half_open_segments_boundary_belongs_to_later_segment() {
    let segs = clock_boundary_case();
    // t == 100.0 恰为相接点：只能命中后一段（offset 0.020）。
    let at = db::clock_segments_at(&segs.iter().map(|(s, id)| (s.clone(), *id)).collect::<Vec<_>>(),
        "S1", 100.0);
    assert_eq!(at.len(), 1, "边界时刻必须唯一归属");
    assert!((at[0].0.offset_sec - 0.020).abs() < 1e-12);

    // 边界前一个 epsilon 属于前一段。
    let before = db::clock_segments_at(
        &segs.iter().map(|(s, id)| (s.clone(), *id)).collect::<Vec<_>>(),
        "S1", 100.0 - 1e-9);
    assert_eq!(before.len(), 1);
    assert!((before[0].0.offset_sec - 0.010).abs() < 1e-12);
}

#[test]
fn overlapping_segments_rejected_adjacent_allowed() {
    let (segs, _) = synthetic_three_station_case();
    let base: Vec<ClockSegment> = segs.clone();
    assert!(db::validate_clock_partition(&base).is_ok(), "相接段合法");

    let mut bad = base.clone();
    bad.push(ClockSegment {
        station_id: "S1".into(), t_start: 50.0, t_end: Some(150.0),
        offset_sec: 0.0, source: "bad".into(),
    });
    assert!(db::validate_clock_partition(&bad).is_err(), "重叠段必须拒绝");
}

// ---------- 批次幂等 ----------

#[test]
fn duplicate_batch_and_duplicate_obs_do_not_double_insert() {
    let mut conn = fresh_conn();
    let data = acoustic_review::synthetic_build_minimal();
    for s in &data.stations { db::upsert_station(&conn, s).unwrap(); }
    for e in &data.events { db::create_event(&conn, e).unwrap(); }
    for c in &data.clocks { db::insert_clock_segment(&mut conn, c).unwrap(); }

    let raw = serde_json::json!(data.batches[0]);
    let r1 = db::ingest_batch(&mut conn, &data.batches[0], &raw).unwrap();
    assert!(matches!(r1, db::IngestResult::Inserted { .. }));
    let n1: i64 = conn.query_row("SELECT COUNT(*) FROM observations", [], |r| r.get(0)).unwrap();

    let r2 = db::ingest_batch(&mut conn, &data.batches[0], &raw).unwrap();
    assert_eq!(r2, db::IngestResult::DuplicateBatch);
    let n2: i64 = conn.query_row("SELECT COUNT(*) FROM observations", [], |r| r.get(0)).unwrap();
    assert_eq!(n1, n2, "重传批次不新增观测");
}

// ---------- 求解器：欠定几何 / 超光速 / 域外不夹紧 ----------

fn solve_with_evidence(
    conn: &mut Connection,
    event_id: &str,
    evidence: Vec<(&str, serde_json::Value)>,
) -> Vec<Candidate> {
    for (kind, payload) in evidence {
        db::append_evidence(conn, event_id, kind, &payload, "test").unwrap();
    }
    let event = db::get_event(conn, event_id).unwrap().unwrap();
    let input = solver::SolverInput {
        event: solver::SolverEvent {
            event_id: event.event_id.clone(),
            window_start: event.window_start,
            window_end: event.window_end,
            sound_speed_mps: event.sound_speed_mps,
            bounds: event.bounds,
        },
        stations: db::list_stations(conn).unwrap(),
        clock_segments: db::list_clock_segments(conn).unwrap(),
        observations: db::list_observations(conn, event_id).unwrap(),
        lags: db::list_lags(conn, event_id).unwrap(),
        evidence: db::list_evidence(conn, event_id).unwrap(),
    };
    let out = solver::solve(input, 1);
    out.candidates
}

#[test]
fn locked_clocks_resolve_point_unlocked_is_clock_freedom() {
    let mut conn = fresh_conn();
    let (segs, data) = synthetic_three_station_case();
    seed(&mut conn, &segs, &data);

    let before = solve_with_evidence(&mut conn, "EV", vec![]);
    assert!(before.iter().any(|c| c.underdetermined == "clock_freedom" || c.kind != "point"),
        "未锁定时钟时不允许给出点定位");

    let seg_ids: Vec<i64> = conn
        .prepare("SELECT segment_id FROM clock_segments ORDER BY segment_id").unwrap()
        .query_map([], |r| r.get::<_, i64>(0)).unwrap()
        .map(Result::unwrap).collect();
    let ev: Vec<(&str, serde_json::Value)> = seg_ids.iter()
        .map(|id| ("lock_clock", serde_json::json!({"segment_id": id})))
        .collect();
    let after = solve_with_evidence(&mut conn, "EV", ev);
    let best = &after[0];
    assert_eq!(best.kind, "point", "三台站 + 锁定时钟应得到点");
    let p = best.point.unwrap();
    assert!((p[0] - 14.0).abs() < 0.25 && (p[1] - 10.0).abs() < 0.25,
        "点定位应接近真实源 (14,10)，得到 {:?}", p);
    assert!(best.rms_sec.unwrap() < 1e-4);
}

#[test]
fn single_tdoa_is_curve_sensor_geometry() {
    let mut conn = fresh_conn();
    let (segs, data) = synthetic_three_station_case();
    seed(&mut conn, &segs, &data);
    // 只锁定两个锚段并删除第三个台站观测，制造单 TDOA。
    let seg_ids: Vec<i64> = conn
        .prepare("SELECT segment_id FROM clock_segments WHERE station_id IN ('S1','S2')").unwrap()
        .query_map([], |r| r.get::<_, i64>(0)).unwrap()
        .map(Result::unwrap).collect();
    conn.execute("DELETE FROM observations WHERE station_id='S3'", []).unwrap();
    conn.execute("DELETE FROM lags WHERE station_id='S3' OR peer_id='S3'", []).unwrap();
    let ev: Vec<(&str, serde_json::Value)> = seg_ids.iter()
        .map(|id| ("lock_clock", serde_json::json!({"segment_id": id}))).collect();
    let out = solve_with_evidence(&mut conn, "EV", ev);
    assert_eq!(out[0].kind, "curve");
    assert_eq!(out[0].underdetermined, "sensor_geometry");
    assert!(out[0].curve.as_ref().unwrap().points.len() >= 2);
}

#[test]
fn superluminal_lag_is_marked_not_clamped() {
    let mut conn = fresh_conn();
    let (segs, mut data) = synthetic_three_station_case();
    // 注入超光速 CCF：基线 30 m，给 80 m 等效延迟。
    data.batches[0].observations[0].lags.push(LagInput {
        peer_id: "S2".into(), lag_sec: 80.0 / 343.0, peak_band_hz: 1000.0,
        snr_db: 2.0, hint: "direct".into(), lag_uid: Some("lag:super".into()),
    });
    seed_with_lags(&mut conn, &segs, &data);
    let out = solve_with_evidence(&mut conn, "EV", vec![]);
    assert!(out.iter().any(|c| c.underdetermined == "unobservable_input"
        || c.residuals.iter().any(|r| r.excluded_reason.as_deref() == Some("superluminal_lag"))));
    // 任何点候选都不允许被夹回场地边界。
    for c in &out {
        if let Some(p) = c.point {
            let b = data.events[0].site_bounds;
            let inside = p[0] >= b.min_x && p[0] <= b.max_x && p[1] >= b.min_y && p[1] <= b.max_y;
            if !inside {
                assert_eq!(c.underdetermined, "outside_observable_geometry");
            }
        }
    }
}

// ---------- 排名 / 证据编号稳定 ----------

#[test]
fn repeated_solve_has_stable_ranking_ids_and_fingerprint() {
    let mut conn = fresh_conn();
    let (segs, data) = synthetic_three_station_case();
    seed(&mut conn, &segs, &data);
    let run = |conn: &mut Connection| {
        let event = db::get_event(conn, "EV").unwrap().unwrap();
        let input = solver::SolverInput {
            event: solver::SolverEvent {
                event_id: event.event_id, window_start: event.window_start,
                window_end: event.window_end, sound_speed_mps: event.sound_speed_mps,
                bounds: event.bounds,
            },
            stations: db::list_stations(conn).unwrap(),
            clock_segments: db::list_clock_segments(conn).unwrap(),
            observations: db::list_observations(conn, "EV").unwrap(),
            lags: db::list_lags(conn, "EV").unwrap(),
            evidence: db::list_evidence(conn, "EV").unwrap(),
        };
        solver::solve(input, 7)
    };
    let a = run(&mut conn);
    let b = run(&mut conn);
    assert_eq!(a.fingerprint, b.fingerprint, "固定输入指纹稳定");
    let ids_a: Vec<String> = a.candidates.iter().map(|c| c.candidate_id.clone()).collect();
    let ids_b: Vec<String> = b.candidates.iter().map(|c| c.candidate_id.clone()).collect();
    assert_eq!(ids_a, ids_b, "并行求解不改变候选编号");
    let sig_a: Vec<&str> = a.candidates.iter().map(|c| c.signature.as_str()).collect();
    let sig_b: Vec<&str> = b.candidates.iter().map(|c| c.signature.as_str()).collect();
    assert_eq!(sig_a, sig_b, "排名顺序稳定");
}

#[test]
fn evidence_ids_are_append_only_and_sequential() {
    let mut conn = fresh_conn();
    let (segs, data) = synthetic_three_station_case();
    seed(&mut conn, &segs, &data);
    let id1 = db::append_evidence(&mut conn, "EV", "lock_clock",
        &serde_json::json!({"segment_id": 1}), "a").unwrap();
    let id2 = db::append_evidence(&mut conn, "EV", "exclude_reflection",
        &serde_json::json!({"lag_uid": "x"}), "b").unwrap();
    assert_eq!(id1, "ev-EV:0001");
    assert_eq!(id2, "ev-EV:0002");
    let n: i64 = conn.query_row("SELECT COUNT(*) FROM evidence", [], |r| r.get(0)).unwrap();
    assert_eq!(n, 2);
}

// ---------- 崩溃恢复 ----------

#[test]
fn crash_recovery_drops_unfinished_candidates() {
    let mut conn = fresh_conn();
    let (segs, data) = synthetic_three_station_case();
    seed(&mut conn, &segs, &data);
    conn.execute("INSERT INTO solve_jobs(event_id,fingerprint,status) VALUES('EV','fp','running')", []).unwrap();
    let jid: i64 = conn.query_row("SELECT last_insert_rowid()", [], |r| r.get(0)).unwrap();
    conn.execute(
        "INSERT INTO candidates(candidate_id,job_id,rank,kind,underdetermined,score,signature,residuals_json)
         VALUES('fake',?1,1,'point','',0,'x','[]')", rusqlite::params![jid]).unwrap();
    // claim 模拟重启：running → pending，并清掉未完成候选。
    let claimed = db::claim_next_job(&mut conn).unwrap();
    assert_eq!(claimed, Some(jid));
    let n: i64 = conn.query_row("SELECT COUNT(*) FROM candidates WHERE candidate_id='fake'", [], |r| r.get(0)).unwrap();
    assert_eq!(n, 0, "未完成候选不得残留为可审核结果");
    let status: String = conn.query_row("SELECT status FROM solve_jobs WHERE job_id=?1",
        rusqlite::params![jid], |r| r.get(0)).unwrap();
    assert_eq!(status, "running");
}

// ---------- helpers ----------

fn seed(conn: &mut Connection, segs: &[ClockSegment], data: &SeedDataLike) {
    seed_with_lags(conn, segs, data);
}
fn seed_with_lags(conn: &mut Connection, segs: &[ClockSegment], data: &SeedDataLike) {
    for s in &data.stations { db::upsert_station(conn, s).unwrap(); }
    for c in segs { db::insert_clock_segment(conn, c).unwrap(); }
    for e in &data.events { db::create_event(conn, e).unwrap(); }
    for b in &data.batches {
        let raw = serde_json::json!(b);
        db::ingest_batch(conn, b, &raw).unwrap();
    }
}

type SeedDataLike = acoustic_review::SeedDataLike;
